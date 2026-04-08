//! The `CleanupActor` is a singleton that runs on a periodic tick.
//!
//! It enforces retention policies (source -> profile -> global precedence)
//! and storage quotas. Videos are only deleted when all referencing sources
//! agree the retention period has expired.

use std::path::{Path, PathBuf};
use std::time::Duration;

use kameo::Reply;
use kameo::prelude::*;
use metrics::{counter, gauge};
use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, error, info, instrument, warn};
use ulid::Ulid;

use chrono::{DateTime, Utc};

use crate::db;
use crate::domain::activity::{ActivityEventType, ActivitySeverity};
use crate::domain::video::{Video, VideoStatus};

/// Default cleanup interval.
const DEFAULT_CLEANUP_INTERVAL_SECS: u64 = 60 * 15; // seconds x minutes

/// The cleanup actor.
///
/// Periodically checks for videos past their retention period and
/// enforces storage quotas by removing old files.
pub struct CleanupActor {
    /// Database pool.
    pool: PgPool,
    /// Global retention policy in days (fallback if not set per-profile/source).
    global_retention_days: Option<i32>,
    /// Cleanup interval.
    cleanup_interval: Duration,
    /// Whether the cleanup loop is running.
    running: bool,
    /// Timestamp of the last cleanup run.
    last_run_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for CleanupActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CleanupActor")
            .field("global_retention_days", &self.global_retention_days)
            .field("cleanup_interval_secs", &self.cleanup_interval.as_secs())
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

/// Arguments for spawning the cleanup actor.
pub struct CleanupActorArgs {
    pub pool: PgPool,
    /// Global retention in days (from config).
    pub global_retention_days: Option<u32>,
    /// Optional custom cleanup interval.
    pub cleanup_interval: Option<Duration>,
}

impl Actor for CleanupActor {
    type Args = CleanupActorArgs;
    type Error = color_eyre::eyre::Error;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let cleanup_interval = args
            .cleanup_interval
            .unwrap_or(Duration::from_secs(DEFAULT_CLEANUP_INTERVAL_SECS));

        info!(
            global_retention_days = ?args.global_retention_days,
            cleanup_interval_secs = cleanup_interval.as_secs(),
            "Cleanup actor starting"
        );

        let actor = Self {
            pool: args.pool,
            global_retention_days: args
                .global_retention_days
                .and_then(|d| i32::try_from(d).ok()),
            cleanup_interval,
            running: false,
            last_run_at: None,
        };

        // Start the cleanup loop.
        // Use try_send() to avoid potential deadlock from self-tell with bounded mailbox.
        if let Err(e) = actor_ref.tell(StartCleanup).try_send() {
            error!(error = %e, "Failed to start cleanup loop");
            return Err(e.into());
        }

        Ok(actor)
    }

    async fn on_stop(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        reason: ActorStopReason,
    ) -> Result<(), Self::Error> {
        info!(reason = ?reason, "Cleanup actor stopping");
        self.running = false;
        Ok(())
    }
}

/// Message to start the cleanup loop.
pub struct StartCleanup;

impl Message<StartCleanup> for CleanupActor {
    type Reply = ();

    async fn handle(&mut self, _msg: StartCleanup, ctx: &mut Context<Self, Self::Reply>) {
        if self.running {
            debug!("Cleanup already running");
            return;
        }

        self.running = true;
        info!("Starting cleanup loop");

        let actor_ref = ctx.actor_ref().clone();
        let cleanup_interval = self.cleanup_interval;

        tokio::spawn(async move {
            let mut interval = interval(cleanup_interval);
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

            // Run immediately on start
            if let Err(e) = actor_ref.tell(RunCleanup).try_send() {
                error!(error = %e, "Failed to run initial cleanup");
            }

            loop {
                interval.tick().await;

                if !actor_ref.is_alive() {
                    break;
                }

                if let Err(e) = actor_ref.tell(RunCleanup).try_send() {
                    error!(error = %e, "Failed to trigger cleanup");
                    break;
                }
            }

            debug!("Cleanup loop ended");
        });
    }
}

/// Message to stop the cleanup loop.
pub struct StopCleanup;

impl Message<StopCleanup> for CleanupActor {
    type Reply = ();

    async fn handle(&mut self, _msg: StopCleanup, _ctx: &mut Context<Self, Self::Reply>) {
        info!("Stopping cleanup");
        self.running = false;
    }
}

/// Message to run cleanup immediately.
pub struct RunCleanup;

/// Result of a cleanup operation.
#[derive(Debug, Clone, Default, Reply)]
pub struct CleanupResult {
    /// Number of videos cleaned up due to retention policy.
    pub retention_cleaned: usize,
    /// Number of videos cleaned up due to quota enforcement.
    pub quota_cleaned: usize,
    /// Number of orphaned temp files removed.
    pub temp_files_cleaned: usize,
    /// Total bytes freed.
    pub bytes_freed: i64,
    /// Errors encountered.
    pub errors: Vec<String>,
}

impl Message<RunCleanup> for CleanupActor {
    type Reply = CleanupResult;

    #[instrument(skip_all)]
    async fn handle(
        &mut self,
        _msg: RunCleanup,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        info!("Running cleanup");
        self.last_run_at = Some(Utc::now());

        let mut result = CleanupResult::default();

        // Phase 1: Retention cleanup
        match self.cleanup_retention().await {
            Ok((count, bytes)) => {
                result.retention_cleaned = count;
                result.bytes_freed += bytes;
            }
            Err(e) => {
                error!(error = %e, "Retention cleanup failed");
                result.errors.push(format!("Retention cleanup failed: {e}"));
            }
        }

        // Phase 2: Quota enforcement
        match self.enforce_quotas().await {
            Ok((count, bytes)) => {
                result.quota_cleaned = count;
                result.bytes_freed += bytes;
            }
            Err(e) => {
                error!(error = %e, "Quota enforcement failed");
                result.errors.push(format!("Quota enforcement failed: {e}"));
            }
        }

        // Phase 3: Orphaned temp file cleanup
        match self.cleanup_temp_files().await {
            Ok(count) => result.temp_files_cleaned = count,
            Err(e) => {
                error!(error = %e, "Temp file cleanup failed");
                result.errors.push(format!("Temp file cleanup failed: {e}"));
            }
        }

        let total_cleaned = result.retention_cleaned + result.quota_cleaned;
        counter!(crate::metrics::VIDEOS_CLEANED_TOTAL).increment(total_cleaned as u64);
        counter!(crate::metrics::CLEANUP_TEMP_FILES_REMOVED_TOTAL)
            .increment(result.temp_files_cleaned as u64);
        #[allow(clippy::cast_precision_loss)]
        gauge!(crate::metrics::CLEANUP_BYTES_FREED).set(result.bytes_freed as f64);

        info!(
            retention_cleaned = result.retention_cleaned,
            quota_cleaned = result.quota_cleaned,
            temp_files_cleaned = result.temp_files_cleaned,
            bytes_freed = result.bytes_freed,
            errors = result.errors.len(),
            "Cleanup complete"
        );

        result
    }
}

/// Get the current cleanup status.
pub struct GetCleanupStatus;

/// Status information for the cleanup actor.
#[derive(Debug, Clone, Reply)]
pub struct CleanupStatus {
    pub running: bool,
    pub global_retention_days: Option<i32>,
    pub cleanup_interval_secs: u64,
    pub last_run_at: Option<DateTime<Utc>>,
}

impl Message<GetCleanupStatus> for CleanupActor {
    type Reply = CleanupStatus;

    async fn handle(
        &mut self,
        _msg: GetCleanupStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        CleanupStatus {
            running: self.running,
            global_retention_days: self.global_retention_days,
            cleanup_interval_secs: self.cleanup_interval.as_secs(),
            last_run_at: self.last_run_at,
        }
    }
}

impl CleanupActor {
    /// Clean up videos past their retention period.
    async fn cleanup_retention(&self) -> color_eyre::Result<(usize, i64)> {
        let videos = db::list_videos_past_retention(&self.pool, self.global_retention_days).await?;

        if videos.is_empty() {
            debug!("No videos past retention");
            return Ok((0, 0));
        }

        info!(count = videos.len(), "Found videos past retention");

        let mut cleaned = 0;
        let mut bytes_freed = 0i64;

        for video in videos {
            match self.clean_video(&video).await {
                Ok(bytes) => {
                    cleaned += 1;
                    bytes_freed += bytes;
                }
                Err(e) => {
                    warn!(
                        video_id = %video.id,
                        error = %e,
                        "Failed to clean video"
                    );
                }
            }
        }

        Ok((cleaned, bytes_freed))
    }

    /// Enforce storage quotas per profile.
    async fn enforce_quotas(&self) -> color_eyre::Result<(usize, i64)> {
        // Get all profiles
        let profiles = db::list_profiles(&self.pool).await?;

        let mut total_cleaned = 0;
        let mut total_bytes = 0i64;

        for profile in profiles {
            // Calculate current usage for this profile
            let usage = self.calculate_profile_usage(profile.id).await?;

            if usage <= profile.storage_quota_bytes {
                continue;
            }

            let over_quota = usage - profile.storage_quota_bytes;
            info!(
                profile_id = %profile.id,
                usage,
                quota = profile.storage_quota_bytes,
                over_quota,
                "Profile over quota"
            );

            // Get videos for this profile's sources, sorted by download date (oldest first)
            let videos = self.get_profile_videos_by_age(profile.id).await?;

            let mut freed = 0i64;
            for video in videos {
                if freed >= over_quota {
                    break;
                }

                match self.clean_video(&video).await {
                    Ok(bytes) => {
                        total_cleaned += 1;
                        total_bytes += bytes;
                        freed += bytes;
                    }
                    Err(e) => {
                        warn!(
                            video_id = %video.id,
                            error = %e,
                            "Failed to clean video for quota"
                        );
                    }
                }
            }
        }

        Ok((total_cleaned, total_bytes))
    }

    /// Clean a single video (delete file and update database).
    async fn clean_video(&self, video: &Video) -> color_eyre::Result<i64> {
        let bytes = video.file_size_bytes.unwrap_or(0);

        // Delete the file if it exists
        if let Some(file_path) = &video.file_path {
            let path = Path::new(file_path);
            if path.exists() {
                tokio::fs::remove_file(path).await.map_err(|e| {
                    color_eyre::eyre::eyre!("Failed to delete file {}: {}", file_path, e)
                })?;
                info!(
                    video_id = %video.id,
                    file_path,
                    bytes,
                    "Deleted video file"
                );
            } else {
                debug!(
                    video_id = %video.id,
                    file_path,
                    "File already deleted"
                );
            }
        }

        // Update video status to cleaned
        db::update_video_status(&self.pool, video.id, VideoStatus::Cleaned).await?;

        #[allow(clippy::cast_precision_loss)]
        let size_mb = bytes as f64 / 1_048_576.0;
        let message = format!("Cleaned \"{}\" ({size_mb:.1} MB freed)", video.title);
        db::log_activity(
            &self.pool,
            ActivityEventType::VideoCleaned,
            ActivitySeverity::Info,
            &message,
            None,
            Some(video.id),
            None,
        )
        .await;

        Ok(bytes)
    }

    /// Calculate total storage usage for a profile.
    async fn calculate_profile_usage(&self, profile_id: Ulid) -> color_eyre::Result<i64> {
        // Get all sources for this profile
        let sources = db::list_sources_for_profile(&self.pool, profile_id).await?;

        let mut total_bytes = 0i64;
        let mut counted_videos = std::collections::HashSet::new();

        for source in sources {
            let videos = db::list_videos_for_source(&self.pool, source.id).await?;

            for video in videos {
                // Only count each video once (may be linked to multiple sources)
                if counted_videos.contains(&video.id) {
                    continue;
                }
                counted_videos.insert(video.id);

                if video.status == VideoStatus::Completed {
                    total_bytes += video.file_size_bytes.unwrap_or(0);
                }
            }
        }

        Ok(total_bytes)
    }

    /// Clean up orphaned yt-dlp temp files from all profile output directories.
    async fn cleanup_temp_files(&self) -> color_eyre::Result<usize> {
        let profiles = db::list_profiles(&self.pool).await?;

        let mut dirs: Vec<PathBuf> = profiles
            .into_iter()
            .flat_map(|p| {
                let base = PathBuf::from(&p.output_dir);
                [base.clone(), base.join("incomplete")]
            })
            .collect();
        dirs.sort();
        dirs.dedup();

        let mut cleaned = 0;
        for dir in dirs {
            if !dir.exists() {
                continue;
            }

            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(e) => e,
                Err(e) => {
                    warn!(dir = %dir.display(), error = %e, "Failed to read directory");
                    continue;
                }
            };

            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if is_ytdlp_temp_file(&path) {
                    info!(path = %path.display(), "Cleaning up orphaned temp file");
                    if let Err(e) = tokio::fs::remove_file(&path).await {
                        warn!(path = %path.display(), error = %e, "Failed to remove temp file");
                    } else {
                        cleaned += 1;
                    }
                }
            }
        }

        if cleaned > 0 {
            info!(cleaned, "Temp file cleanup complete");
        }

        Ok(cleaned)
    }

    /// Get videos for a profile, sorted by download date (oldest first).
    async fn get_profile_videos_by_age(&self, profile_id: Ulid) -> color_eyre::Result<Vec<Video>> {
        let sources = db::list_sources_for_profile(&self.pool, profile_id).await?;

        let mut videos = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for source in sources {
            let source_videos = db::list_videos_for_source(&self.pool, source.id).await?;

            for video in source_videos {
                if seen.contains(&video.id) {
                    continue;
                }
                seen.insert(video.id);

                // Only include completed videos
                if video.status == VideoStatus::Completed {
                    videos.push(video);
                }
            }
        }

        // Sort by downloaded_at (oldest first)
        videos.sort_by(|a, b| {
            let a_time = a.downloaded_at.unwrap_or(a.created_at);
            let b_time = b.downloaded_at.unwrap_or(b.created_at);
            a_time.cmp(&b_time)
        });

        Ok(videos)
    }
}

/// Message to clean up orphaned .part files and yt-dlp temp files.
pub struct CleanupPartFiles {
    pub directories: Vec<std::path::PathBuf>,
}

/// Check whether a file is a yt-dlp temporary artifact.
///
/// Matches `.part`, `.ytdl` extensions and `temp_audio_*` / `temp_video_*`
/// intermediate files left behind after failed merges.
fn is_ytdlp_temp_file(path: &Path) -> bool {
    // .part / .ytdl extensions
    let is_part = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("part"));
    let is_ytdl = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ytdl"));

    // temp_audio_* / temp_video_* intermediate merge files
    let is_temp_merge = path.file_name().is_some_and(|name| {
        let n = name.to_string_lossy();
        n.starts_with("temp_audio_") || n.starts_with("temp_video_")
    });

    is_part || is_ytdl || is_temp_merge
}

impl Message<CleanupPartFiles> for CleanupActor {
    type Reply = Result<usize, String>;

    #[instrument(skip_all)]
    async fn handle(
        &mut self,
        msg: CleanupPartFiles,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut cleaned = 0;

        for dir in msg.directories {
            if !dir.exists() {
                continue;
            }

            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(e) => e,
                Err(e) => {
                    warn!(dir = %dir.display(), error = %e, "Failed to read directory");
                    continue;
                }
            };

            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();

                if is_ytdlp_temp_file(&path) {
                    info!(path = %path.display(), "Cleaning up orphaned temp file");
                    if let Err(e) = tokio::fs::remove_file(&path).await {
                        warn!(path = %path.display(), error = %e, "Failed to remove temp file");
                    } else {
                        cleaned += 1;
                    }
                }
            }
        }

        info!(cleaned, "Part file cleanup complete");
        Ok(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_cleanup_interval() {
        // Should be reasonable (not too frequent, not too infrequent)
        const _: () = assert!(DEFAULT_CLEANUP_INTERVAL_SECS >= 300); // At least 5 minutes
        const _: () = assert!(DEFAULT_CLEANUP_INTERVAL_SECS <= 86400); // At most 1 day
    }
}
