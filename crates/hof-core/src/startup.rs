//! Application startup and crash recovery logic.
//!
//! This module provides functionality for:
//! - Resetting videos stuck in `downloading` status back to `pending`
//! - Cleaning up orphaned `.part` files from interrupted downloads
//! - Initializing and hydrating the actor system from database state

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::actors::cleanup::{CleanupActor, CleanupActorArgs, CleanupPartFiles};
use crate::actors::download_supervisor::{DownloadSupervisor, DownloadSupervisorArgs};
use crate::actors::scheduler::{SchedulerActor, SchedulerArgs};
use crate::config::Config;
use crate::db;
use crate::domain::video::DownloadProgress;
use crate::ytdlp::YtdlpClient;

use kameo::prelude::*;

/// Actors created during startup.
pub struct ActorSystem {
    /// The download supervisor (manages concurrent downloads).
    pub supervisor: ActorRef<DownloadSupervisor>,
    /// The scheduler (triggers source indexing).
    pub scheduler: ActorRef<SchedulerActor>,
    /// The cleanup actor (enforces retention and quotas).
    pub cleanup: ActorRef<CleanupActor>,
    /// Channel receiver for download progress updates.
    pub progress_rx: mpsc::Receiver<DownloadProgress>,
}

/// Initialize the actor system and perform crash recovery.
///
/// This function:
/// 1. Ensures the default output directory exists
/// 2. Resets any videos stuck in `downloading` status
/// 3. Cleans up orphaned `.part` files
/// 4. Creates and starts all singleton actors
/// 5. Returns handles to interact with the actor system
///
/// # Errors
///
/// Returns an error if initialization fails.
pub async fn initialize(pool: PgPool, config: &Config) -> Result<ActorSystem> {
    info!("Initializing actor system");

    // Phase 0: Ensure output directory exists
    tokio::fs::create_dir_all(&config.storage.default_output_dir)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to create output directory: {}",
                config.storage.default_output_dir.display()
            )
        })?;
    info!(
        output_dir = %config.storage.default_output_dir.display(),
        "Output directory ready"
    );

    // Phase 0.5: Ensure TMPDIR is valid (nix-shell sets TMPDIR to a session-specific
    // directory that may not exist after a restart). The yt-dlp crate uses tempfile
    // which respects TMPDIR.
    ensure_valid_tmpdir().await?;

    // Phase 0.6: Verify yt-dlp binary exists
    verify_ytdlp_binary(&config.download.ytdlp_path).await?;

    // Phase 1: Crash recovery
    recover_from_crash(&pool, &config.storage.default_output_dir).await?;

    // Phase 2: Initialize yt-dlp client
    let ytdlp = Arc::new(
        YtdlpClient::new(
            &config.download.ytdlp_path,
            None, // Use system ffmpeg
            &config.storage.default_output_dir,
        )
        .await
        .wrap_err("Failed to initialize yt-dlp client")?,
    );

    // Phase 3: Create progress channel
    let (progress_tx, progress_rx) = mpsc::channel(1000);

    // Phase 4: Start actors
    let supervisor = start_supervisor(pool.clone(), ytdlp.clone(), config, progress_tx);
    let scheduler = start_scheduler(pool.clone(), ytdlp.clone(), supervisor.clone());
    let cleanup = start_cleanup(pool.clone(), config);

    // Phase 5: Initial cleanup of part files
    let output_dirs = collect_output_directories(&pool).await?;
    cleanup
        .tell(CleanupPartFiles {
            directories: output_dirs,
        })
        .await
        .wrap_err("Failed to clean up part files")?;

    info!("Actor system initialized successfully");

    Ok(ActorSystem {
        supervisor,
        scheduler,
        cleanup,
        progress_rx,
    })
}

/// Perform crash recovery operations.
async fn recover_from_crash(pool: &PgPool, default_output_dir: &Path) -> Result<()> {
    info!("Starting crash recovery");

    // Reset stuck downloads
    let reset_count = db::reset_stuck_downloads(pool)
        .await
        .wrap_err("Failed to reset stuck downloads")?;

    if reset_count > 0 {
        info!(count = reset_count, "Reset stuck downloads to pending");
    } else {
        debug!("No stuck downloads to reset");
    }

    // Clean up part files in default directory
    clean_part_files(default_output_dir).await?;

    info!("Crash recovery complete");
    Ok(())
}

/// Clean up orphaned .part files from a directory.
async fn clean_part_files(dir: &Path) -> Result<()> {
    if !dir.exists() {
        debug!(dir = %dir.display(), "Output directory does not exist");
        return Ok(());
    }

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .wrap_err_with(|| format!("Failed to read directory: {}", dir.display()))?;

    let mut cleaned = 0;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        // yt-dlp creates .part files for in-progress downloads
        // and .ytdl files for metadata
        let is_part_file = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("part"));
        let is_ytdl_file = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ytdl"));

        if is_part_file || is_ytdl_file {
            info!(path = %path.display(), "Cleaning up orphaned file");
            if let Err(e) = tokio::fs::remove_file(&path).await {
                warn!(path = %path.display(), error = %e, "Failed to remove file");
            } else {
                cleaned += 1;
            }
        }
    }

    if cleaned > 0 {
        info!(count = cleaned, dir = %dir.display(), "Cleaned up orphaned files");
    }

    Ok(())
}

/// Start the download supervisor actor.
fn start_supervisor(
    pool: PgPool,
    ytdlp: Arc<YtdlpClient>,
    config: &Config,
    progress_tx: mpsc::Sender<DownloadProgress>,
) -> ActorRef<DownloadSupervisor> {
    let args = DownloadSupervisorArgs {
        pool,
        ytdlp,
        config: config.download.clone(),
        progress_tx,
    };

    let supervisor = DownloadSupervisor::spawn(args);

    info!("Download supervisor started");
    supervisor
}

/// Start the scheduler actor.
fn start_scheduler(
    pool: PgPool,
    ytdlp: Arc<YtdlpClient>,
    supervisor: ActorRef<DownloadSupervisor>,
) -> ActorRef<SchedulerActor> {
    let args = SchedulerArgs {
        pool,
        ytdlp,
        supervisor,
        check_interval: None, // Use default
    };

    let scheduler = SchedulerActor::spawn(args);

    info!("Scheduler started");
    scheduler
}

/// Start the cleanup actor.
fn start_cleanup(pool: PgPool, config: &Config) -> ActorRef<CleanupActor> {
    let args = CleanupActorArgs {
        pool,
        global_retention_days: config.storage.retention_days,
        cleanup_interval: None, // Use default
    };

    let cleanup = CleanupActor::spawn(args);

    info!("Cleanup actor started");
    cleanup
}

/// Verify that the yt-dlp binary exists and is executable.
async fn verify_ytdlp_binary(ytdlp_path: &Path) -> Result<()> {
    // First check if the path exists directly
    if ytdlp_path.is_absolute() && ytdlp_path.exists() {
        info!(path = %ytdlp_path.display(), "yt-dlp binary found");
        return Ok(());
    }

    // If it's just a command name (like "yt-dlp"), try to find it in PATH
    let output = tokio::process::Command::new(ytdlp_path)
        .arg("--version")
        .output()
        .await;

    match output {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            info!(
                path = %ytdlp_path.display(),
                version = %version.trim(),
                "yt-dlp binary verified"
            );
            Ok(())
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(color_eyre::eyre::eyre!(
                "yt-dlp binary at '{}' failed to run: {}",
                ytdlp_path.display(),
                stderr.trim()
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(color_eyre::eyre::eyre!(
            "yt-dlp binary not found at '{}'. \
                 Please install yt-dlp: https://github.com/yt-dlp/yt-dlp#installation \
                 or set YTDLP_PATH to the correct path.",
            ytdlp_path.display()
        )),
        Err(e) => Err(color_eyre::eyre::eyre!(
            "Failed to verify yt-dlp binary at '{}': {}",
            ytdlp_path.display(),
            e
        )),
    }
}

/// Ensure TMPDIR points to a valid, existing directory.
///
/// nix-shell sets TMPDIR to a session-specific directory (e.g., `/tmp/nix-shell.xxx/`)
/// which may not exist after a restart. The `tempfile` crate (used by yt-dlp) respects
/// TMPDIR, so we need to ensure it's valid.
///
/// If TMPDIR is invalid, we create it (or fall back to /tmp if that fails).
async fn ensure_valid_tmpdir() -> Result<()> {
    if let Ok(tmpdir) = env::var("TMPDIR") {
        let tmpdir_path = Path::new(&tmpdir);
        if tmpdir_path.exists() {
            debug!(tmpdir = %tmpdir, "TMPDIR is valid");
        } else {
            // Try to create the TMPDIR
            match tokio::fs::create_dir_all(&tmpdir_path).await {
                Ok(()) => {
                    info!(tmpdir = %tmpdir, "Created missing TMPDIR");
                }
                Err(e) => {
                    // Can't create it - this is a problem since we can't safely change env vars
                    // Log a warning and hope /tmp works as a fallback
                    warn!(
                        tmpdir = %tmpdir,
                        error = %e,
                        "TMPDIR does not exist and cannot be created. \
                         Set TMPDIR=/tmp before starting the application."
                    );
                }
            }
        }
    }
    Ok(())
}

/// Collect all unique output directories from profiles.
async fn collect_output_directories(pool: &PgPool) -> Result<Vec<PathBuf>> {
    let profiles = db::list_profiles(pool).await?;

    let mut dirs: Vec<PathBuf> = profiles
        .into_iter()
        .map(|p| PathBuf::from(&p.output_dir))
        .collect();

    // Deduplicate
    dirs.sort();
    dirs.dedup();

    Ok(dirs)
}

/// Gracefully shutdown the actor system.
///
/// # Errors
///
/// This function currently does not return errors, but returns `Result`
/// for future compatibility. Actor stop errors are logged but not propagated.
pub async fn shutdown(system: ActorSystem) -> Result<()> {
    info!("Shutting down actor system");

    // Stop actors in reverse order of dependency
    // Scheduler first (stops spawning new indexers)
    if let Err(e) = system.scheduler.stop_gracefully().await {
        warn!(error = %e, "Error stopping scheduler");
    }
    system.scheduler.wait_for_shutdown().await;
    info!("Scheduler stopped");

    // Supervisor next (completes in-flight downloads)
    if let Err(e) = system.supervisor.stop_gracefully().await {
        warn!(error = %e, "Error stopping supervisor");
    }
    system.supervisor.wait_for_shutdown().await;
    info!("Download supervisor stopped");

    // Cleanup last
    if let Err(e) = system.cleanup.stop_gracefully().await {
        warn!(error = %e, "Error stopping cleanup");
    }
    system.cleanup.wait_for_shutdown().await;
    info!("Cleanup actor stopped");

    info!("Actor system shutdown complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_clean_part_files() {
        let temp_dir = TempDir::new().unwrap();

        // Create some test files
        fs::write(temp_dir.path().join("video.part"), "data").unwrap();
        fs::write(temp_dir.path().join("video.ytdl"), "metadata").unwrap();
        fs::write(temp_dir.path().join("video.mp4"), "video").unwrap();

        clean_part_files(temp_dir.path()).await.unwrap();

        // .part and .ytdl should be gone
        assert!(!temp_dir.path().join("video.part").exists());
        assert!(!temp_dir.path().join("video.ytdl").exists());
        // .mp4 should remain
        assert!(temp_dir.path().join("video.mp4").exists());
    }

    #[test]
    fn test_collect_output_directories_dedup() {
        // Test the deduplication logic
        let mut dirs = vec![
            PathBuf::from("/a/b"),
            PathBuf::from("/c/d"),
            PathBuf::from("/a/b"),
            PathBuf::from("/e/f"),
            PathBuf::from("/c/d"),
        ];

        dirs.sort();
        dirs.dedup();

        assert_eq!(dirs.len(), 3);
        assert!(dirs.contains(&PathBuf::from("/a/b")));
        assert!(dirs.contains(&PathBuf::from("/c/d")));
        assert!(dirs.contains(&PathBuf::from("/e/f")));
    }
}
