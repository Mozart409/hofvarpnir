//! Application startup and crash recovery logic.
//!
//! This module provides functionality for:
//! - Resetting videos stuck in `downloading` status back to `pending`
//! - Cleaning up orphaned `.part` files from interrupted downloads
//! - Initializing and hydrating the actor system from database state

use std::env;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use color_eyre::eyre::{Result, WrapErr};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::actors::cleanup::{CleanupActor, CleanupActorArgs, CleanupPartFiles};
use crate::actors::download_supervisor::{
    DownloadSupervisor, DownloadSupervisorArgs, GetSupervisorStatus, ProcessPendingDownloads,
};
use crate::actors::jellyfin_metadata::{JellyfinMetadataActor, JellyfinMetadataActorArgs};
use crate::actors::scheduler::{GetSchedulerStatus, SchedulerActor, SchedulerArgs};
use crate::config::Config;
use crate::db;
use crate::db::ActivityBroadcaster;
use crate::domain::system::SystemIssue;
use crate::domain::video::DownloadProgress;
use crate::runtime_config::{DrainToken, EffectiveSettings, RuntimeConfig};
use crate::ytdlp::YtdlpClient;

use kameo::prelude::*;

/// Shorthand for the live-settings receiver threaded into each actor's `Args`.
type ConfigRx = watch::Receiver<Arc<EffectiveSettings>>;

/// Actors created during startup.
pub struct ActorSystem {
    /// The download supervisor (manages concurrent downloads).
    pub supervisor: ActorRef<DownloadSupervisor>,
    /// The scheduler (triggers source indexing).
    pub scheduler: ActorRef<SchedulerActor>,
    /// The cleanup actor (enforces retention and quotas).
    pub cleanup: ActorRef<CleanupActor>,
    /// The Jellyfin metadata actor (generates metadata files).
    pub jellyfin_metadata: ActorRef<JellyfinMetadataActor>,
    /// Channel receiver for download progress updates.
    pub progress_rx: mpsc::Receiver<DownloadProgress>,
    /// Issues detected during startup (non-fatal warnings/errors).
    pub startup_issues: Vec<SystemIssue>,
    /// Broadcaster for real-time SSE notifications.
    pub broadcaster: ActivityBroadcaster,
    /// Handle to the live runtime settings (pacing/concurrency knobs).
    pub runtime_config: RuntimeConfig,
    /// Process-local drain signal (see ADR-0004). Triggering this stops new
    /// dispatch/indexing work and, once the system reaches quiescence (or
    /// `drain_timeout` elapses), signals `main`'s shutdown select arm.
    pub drain: DrainToken,
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

    let mut startup_issues = Vec::new();

    // Phase 0: Ensure output directory exists
    tokio::fs::create_dir_all(&config.storage.default_output_dir)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to create output directory: {}",
                config.storage.default_output_dir.display()
            )
        })?;

    // Check write permissions
    if let Some(issue) = verify_output_dir_writable(&config.storage.default_output_dir).await {
        startup_issues.push(issue);
    }

    // Phase 0.5: Ensure TMPDIR is valid (nix-shell sets TMPDIR to a session-specific
    // directory that may not exist after a restart). The yt-dlp crate uses tempfile
    // which respects TMPDIR.
    ensure_valid_tmpdir().await?;

    // Phase 0.6: Verify yt-dlp binary exists
    verify_ytdlp_binary(&config.download.ytdlp_path).await?;

    // Phase 0.7: Verify ffmpeg is available for audio/video muxing
    verify_ffmpeg_binary().await?;

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

    // Create broadcaster for SSE real-time notifications
    let broadcaster = ActivityBroadcaster::new();

    // Phase 3.5: Load runtime-mutable settings and start the change listener.
    // `spawn_listener` consumes the handle, so keep a clone to return on
    // `ActorSystem` for callers that want to read/patch settings later.
    let runtime_config =
        RuntimeConfig::new(pool.clone(), crate::config::EnvOverrides::from_env()).await?;
    let _listener = runtime_config.clone().spawn_listener();

    // Process-local drain signal (see ADR-0004: deliberately not persisted).
    // Threaded into the supervisor and scheduler so their dispatch/indexing
    // gates can refuse new work; `spawn_drain_watcher` below watches for it
    // to start and polls both actors for quiescence.
    let drain = DrainToken::new();

    // Phase 4: Start actors
    let supervisor = start_supervisor(
        pool.clone(),
        ytdlp.clone(),
        config,
        progress_tx,
        runtime_config.subscribe(),
        broadcaster.clone(),
        drain.clone(),
    );
    let scheduler = start_scheduler(
        pool.clone(),
        ytdlp.clone(),
        supervisor.clone(),
        runtime_config.subscribe(),
        broadcaster.clone(),
        drain.clone(),
    );
    spawn_drain_watcher(
        drain.clone(),
        supervisor.clone(),
        scheduler.clone(),
        runtime_config.clone(),
    );
    let cleanup = start_cleanup(
        pool.clone(),
        config,
        runtime_config.subscribe(),
        broadcaster.clone(),
    );
    let jellyfin_metadata = start_jellyfin_metadata(pool.clone(), broadcaster.clone());

    // Phase 4.5: Kick pending/retry-eligible downloads after startup recovery.
    // This ensures videos reset from `downloading` -> `pending` are resumed
    // without waiting for the next indexing cycle.
    match supervisor.tell(ProcessPendingDownloads).await {
        Ok(()) => {
            info!("Triggered pending download processing after startup");
        }
        Err(error) => {
            warn!(%error, "Could not contact supervisor for startup pending processing");
        }
    }

    // Phase 5: Initial cleanup of part files
    let output_dirs = collect_output_directories(&pool).await?;
    cleanup
        .tell(CleanupPartFiles {
            directories: output_dirs,
        })
        .await
        .wrap_err("Failed to clean up part files")?;

    if startup_issues.is_empty() {
        info!("Actor system initialized successfully");
    } else {
        warn!(
            issue_count = startup_issues.len(),
            "Actor system initialized with issues"
        );
    }

    Ok(ActorSystem {
        supervisor,
        scheduler,
        cleanup,
        jellyfin_metadata,
        progress_rx,
        startup_issues,
        broadcaster,
        runtime_config,
        drain,
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
    clean_part_files(&default_output_dir.join("incomplete")).await?;

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
    config_rx: ConfigRx,
    broadcaster: ActivityBroadcaster,
    drain: DrainToken,
) -> ActorRef<DownloadSupervisor> {
    let args = DownloadSupervisorArgs {
        pool,
        ytdlp,
        config: config.download.clone(),
        progress_tx,
        config_rx,
        broadcaster,
        drain,
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
    config_rx: ConfigRx,
    broadcaster: ActivityBroadcaster,
    drain: DrainToken,
) -> ActorRef<SchedulerActor> {
    let args = SchedulerArgs {
        pool,
        ytdlp,
        supervisor,
        config_rx,
        broadcaster,
        drain,
    };

    let scheduler = SchedulerActor::spawn(args);

    info!("Scheduler started");
    scheduler
}

/// Start the cleanup actor.
fn start_cleanup(
    pool: PgPool,
    config: &Config,
    config_rx: ConfigRx,
    broadcaster: ActivityBroadcaster,
) -> ActorRef<CleanupActor> {
    let args = CleanupActorArgs {
        pool,
        global_retention_days: config.storage.retention_days,
        config_rx,
        broadcaster,
    };

    let cleanup = CleanupActor::spawn(args);

    info!("Cleanup actor started");
    cleanup
}

/// Start the Jellyfin metadata actor.
fn start_jellyfin_metadata(
    pool: PgPool,
    broadcaster: ActivityBroadcaster,
) -> ActorRef<JellyfinMetadataActor> {
    let args = JellyfinMetadataActorArgs {
        pool,
        check_interval: None, // Use default (24 hours)
        broadcaster,
    };

    let jellyfin_metadata = JellyfinMetadataActor::spawn(args);

    info!("Jellyfin metadata actor started");
    jellyfin_metadata
}

/// Verify that the output directory is writable.
///
/// Attempts to create and delete a temporary file in the directory.
/// Returns a `SystemIssue` if the directory is not writable.
async fn verify_output_dir_writable(dir: &Path) -> Option<SystemIssue> {
    use ulid::Ulid;

    let test_file = dir.join(format!(".write_test_{}", Ulid::generate()));

    match tokio::fs::write(&test_file, b"test").await {
        Ok(()) => {
            // Clean up test file
            if let Err(e) = tokio::fs::remove_file(&test_file).await {
                warn!(
                    path = %test_file.display(),
                    error = %e,
                    "Failed to remove write test file"
                );
            }
            info!(dir = %dir.display(), "Output directory ready");
            None
        }
        Err(e) => {
            let message = format!(
                "Output directory '{}' is not writable: {}. Downloads will fail until this is resolved.",
                dir.display(),
                e
            );
            error!(dir = %dir.display(), error = %e, "Output directory is not writable");
            Some(SystemIssue::error("output_dir_not_writable", message))
        }
    }
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

/// Verify that ffmpeg exists and is executable.
async fn verify_ffmpeg_binary() -> Result<()> {
    let output = tokio::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .await;

    match output {
        Ok(output) if output.status.success() => {
            let version_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let version_line = version_stdout.lines().next().unwrap_or("unknown");
            info!(version = %version_line, "ffmpeg binary verified");
            Ok(())
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(color_eyre::eyre::eyre!(
                "ffmpeg is installed but failed to run: {}",
                stderr.trim()
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(color_eyre::eyre::eyre!(
            "ffmpeg binary not found on PATH. \
             Video downloads requiring merge/mux will fail. \
             Install ffmpeg and restart the application. \
             Nix users: add `ffmpeg` to your dev shell and run `nix develop`."
        )),
        Err(e) => Err(color_eyre::eyre::eyre!(
            "Failed to verify ffmpeg binary: {}",
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
        .flat_map(|p| {
            let base = PathBuf::from(&p.output_dir);
            [base.clone(), base.join("incomplete")]
        })
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

    // Cleanup
    if let Err(e) = system.cleanup.stop_gracefully().await {
        warn!(error = %e, "Error stopping cleanup");
    }
    system.cleanup.wait_for_shutdown().await;
    info!("Cleanup actor stopped");

    // Jellyfin metadata last
    if let Err(e) = system.jellyfin_metadata.stop_gracefully().await {
        warn!(error = %e, "Error stopping jellyfin metadata");
    }
    system.jellyfin_metadata.wait_for_shutdown().await;
    info!("Jellyfin metadata actor stopped");

    info!("Actor system shutdown complete");
    Ok(())
}

/// How often the drain watcher re-checks quiescence while a drain is in
/// progress and its deadline has not yet passed.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Active-work counts the drain watcher polls for quiescence.
///
/// `dispatching` matters as much as `active_downloads`: a video can be
/// reserved (`dispatching`, set synchronously in `dispatch_download`) before
/// its spawned task sleeps out the rate-limit delay, acquires a semaphore
/// permit, and only then registers as `active`. A watcher that checked only
/// `active_downloads` could read zero and shut down on top of a download
/// that is about to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QuiescenceCounts {
    active_downloads: usize,
    dispatching: usize,
    active_indexers: usize,
}

impl QuiescenceCounts {
    const fn is_quiescent(self) -> bool {
        self.active_downloads == 0 && self.dispatching == 0 && self.active_indexers == 0
    }
}

/// Poll `probe` until it reports quiescence or `deadline` passes, then
/// signal drain completion either way.
///
/// Forcing past the deadline is safe: `recover_from_crash` already resets
/// `downloading` rows back to `pending` and removes orphaned `.part` files
/// on the next boot, so a forced shutdown mid-download is recoverable, not
/// corrupting.
///
/// Callers must have already called `drain.begin(..)` and computed
/// `deadline` once, at drain start — this function does not re-read
/// `drain_timeout` and does not call `begin` itself, so an operator
/// retuning `drain_timeout_secs` mid-drain cannot silently extend or
/// truncate a shutdown already in progress.
///
/// `probe` is injectable so this is unit-testable with a pure,
/// virtual-clock-friendly closure instead of a live actor system: the real
/// probe (`live_quiescence_counts`) asks two live `ActorRef`s, which needs a
/// running actor system and a yt-dlp binary that a `start_paused` test
/// cannot provide.
async fn poll_until_quiescent<F, Fut>(mut probe: F, drain: DrainToken, deadline: DateTime<Utc>)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = QuiescenceCounts>,
{
    loop {
        if probe().await.is_quiescent() {
            info!("Drain reached quiescence; signalling shutdown");
            drain.signal_complete();
            return;
        }

        let now = Utc::now();
        if now >= deadline {
            warn!("Drain timeout elapsed with work still in flight; forcing shutdown");
            drain.signal_complete();
            return;
        }

        let wait =
            crate::runtime_config::sleep_duration_until(deadline, now).min(DRAIN_POLL_INTERVAL);
        tokio::time::sleep(wait).await;
    }
}

/// Query the live actor system for in-flight download/index work.
///
/// Takes owned `ActorRef`s (cheap to clone — see the call site in
/// `spawn_drain_watcher`) rather than borrowing them, so the returned future
/// owns everything it holds across its `.await` points instead of borrowing
/// from an ancestor scope. That sidesteps needing `ActorRef: Sync` (which
/// borrowing `&ActorRef` across an `.await` inside a `tokio::spawn`'d future
/// would require, for `&ActorRef` itself to be `Send`) for a property this
/// function has no reason to depend on.
///
/// A query failure (actor already stopped) is treated as quiescent rather
/// than propagated: there is no work left to wait for if the actor that
/// would be doing it is gone, and refusing to ever reach quiescence in that
/// case would just burn the full `drain_timeout` for no reason.
async fn live_quiescence_counts(
    supervisor: ActorRef<DownloadSupervisor>,
    scheduler: ActorRef<SchedulerActor>,
) -> QuiescenceCounts {
    let (active_downloads, dispatching) = match supervisor.ask(GetSupervisorStatus).await {
        Ok(status) => (status.active_downloads, status.dispatching),
        Err(error) => {
            warn!(%error, "Drain watcher could not reach supervisor; treating as quiescent");
            (0, 0)
        }
    };

    let active_indexers = match scheduler.ask(GetSchedulerStatus).await {
        Ok(status) => status.active_indexers,
        Err(error) => {
            warn!(%error, "Drain watcher could not reach scheduler; treating as quiescent");
            0
        }
    };

    QuiescenceCounts {
        active_downloads,
        dispatching,
        active_indexers,
    }
}

/// Spawn the background task that waits for a drain to start, then polls
/// the real actor system until quiescence or `drain_timeout` elapses.
///
/// `drain_timeout` is read from `runtime_config` exactly once, when the
/// drain starts (not re-borrowed per poll) — see `poll_until_quiescent`'s
/// doc comment for why.
fn spawn_drain_watcher(
    drain: DrainToken,
    supervisor: ActorRef<DownloadSupervisor>,
    scheduler: ActorRef<SchedulerActor>,
    runtime_config: RuntimeConfig,
) {
    tokio::spawn(async move {
        drain.wait_started().await;

        let timeout = runtime_config.current().drain_timeout.value;
        // `deadline()` can only return `None` if `started_at()` is `None`,
        // which cannot be true here: `wait_started` only resolves once
        // `begin` has set it. Fall back to "now" (i.e. an immediate forced
        // shutdown) rather than unwrapping, since it is unreachable rather
        // than provably impossible to the compiler.
        let deadline = drain.deadline(timeout).unwrap_or_else(Utc::now);

        info!(
            timeout_secs = timeout.as_secs(),
            "Drain started; watching for quiescence"
        );

        poll_until_quiescent(
            // Clone per call rather than capturing `&supervisor`/`&scheduler`:
            // each resulting future then owns its `ActorRef`s outright
            // instead of borrowing them across an `.await` — see
            // `live_quiescence_counts`'s doc comment for why that matters
            // for `Send`. `ActorRef` clones are cheap (an `Arc`-backed
            // handle), so this costs nothing measurable per poll.
            move || live_quiescence_counts(supervisor.clone(), scheduler.clone()),
            drain,
            deadline,
        )
        .await;
    });
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

    // ========================================================================
    // Drain watcher tests (Ruling C).
    //
    // These exercise `poll_until_quiescent` directly with a pure, injected
    // probe rather than a live actor system: `live_quiescence_counts` needs
    // running `DownloadSupervisor`/`SchedulerActor` actors (and, for a real
    // download, a yt-dlp binary), which `start_paused` cannot help with. The
    // actor-level gates that feed into those live counts are covered
    // separately in `download_supervisor.rs`'s `dispatch_download_respects_*`
    // tests.
    //
    // Note on time sources: `chrono::Utc::now()` (used for `DrainToken`
    // deadlines) reads the real OS clock and does NOT track tokio's paused
    // virtual clock (only `tokio::time::*` does). So a test cannot rely on a
    // *future* chrono deadline ever becoming "elapsed" by waiting out
    // virtual-clock sleeps, the way `pause_deadline_computes_sleep_and_lapses`
    // in `runtime_config.rs` relies on virtual time for `tokio::time::sleep`
    // itself. The timeout test below sidesteps this by constructing an
    // already-elapsed deadline up front, so the forced-shutdown branch fires
    // on the very first poll with no sleep involved at all.
    // ========================================================================

    #[tokio::test(start_paused = true)]
    async fn drain_times_out_and_still_shuts_down() {
        let drain = DrainToken::new();
        let started = Utc::now();
        drain.begin(started);

        // Already elapsed at call time (see the module-level note above on
        // why this test does not wait for a future deadline instead).
        let deadline = started
            .checked_sub_signed(chrono::Duration::seconds(1))
            .expect("no underflow");

        // Never reaches quiescence: work is always reported in flight.
        let probe = || async {
            QuiescenceCounts {
                active_downloads: 1,
                dispatching: 0,
                active_indexers: 0,
            }
        };

        // Bounded by a real (not virtual) timeout as a safety net: if
        // `poll_until_quiescent` had a bug that looped forever instead of
        // observing the elapsed deadline, this fails the test instead of
        // hanging the suite.
        tokio::time::timeout(
            Duration::from_secs(5),
            poll_until_quiescent(probe, drain.clone(), deadline),
        )
        .await
        .expect("poll_until_quiescent did not return promptly for an already-elapsed deadline");

        // `signal_complete` must have run despite work still being reported.
        tokio::time::timeout(Duration::from_secs(1), drain.wait_complete())
            .await
            .expect("drain did not signal complete after its deadline elapsed");
    }

    #[tokio::test(start_paused = true)]
    async fn drain_signals_complete_on_reaching_quiescence() {
        let drain = DrainToken::new();
        let started = Utc::now();
        drain.begin(started);

        // Far enough in the future that the elapsed-deadline branch cannot
        // fire during this test; quiescence must be what ends the loop.
        let deadline = started
            .checked_add_signed(chrono::Duration::hours(1))
            .expect("no overflow");

        // Reports work in flight for the first two polls, then quiescent.
        // `AtomicU32` rather than `Cell<u32>`: a plain `Cell` is `!Sync`, so
        // `&Cell<u32>` captured by the probe closure is `!Send` — fine for
        // this direct `.await` in a single test task, but it would poison
        // `poll_until_quiescent`'s generic `F`/`Fut` for a future `Send`
        // bound (needed once something like `clippy::future_not_send`
        // requires one for the real `tokio::spawn`'d caller in
        // `spawn_drain_watcher`) for no reason: this counter carries no
        // requirement to stay unsynchronized.
        let calls = std::sync::atomic::AtomicU32::new(0);
        let probe = || {
            let call = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            async move {
                if call < 2 {
                    QuiescenceCounts {
                        active_downloads: 1,
                        dispatching: 0,
                        active_indexers: 0,
                    }
                } else {
                    QuiescenceCounts {
                        active_downloads: 0,
                        dispatching: 0,
                        active_indexers: 0,
                    }
                }
            }
        };

        tokio::time::timeout(
            Duration::from_secs(5),
            poll_until_quiescent(probe, drain.clone(), deadline),
        )
        .await
        .expect("poll_until_quiescent did not return promptly once quiescent");

        tokio::time::timeout(Duration::from_secs(1), drain.wait_complete())
            .await
            .expect("drain did not signal complete after reaching quiescence");
        assert!(
            calls.load(std::sync::atomic::Ordering::Relaxed) >= 3,
            "expected at least 3 probe polls"
        );
    }
}
