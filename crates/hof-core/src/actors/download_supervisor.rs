//! The `DownloadSupervisor` is a singleton that manages download concurrency.
//!
//! It holds a `tokio::sync::Semaphore` with a configurable number of permits
//! (default 3) and spawns short-lived `DownloadWorker` actors when permits
//! are available. It also handles retry logic with exponential backoff.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use kameo::Reply;
use kameo::prelude::*;
use sqlx::PgPool;
use tokio::sync::{Semaphore, mpsc};
use tokio::time::Instant;
use tracing::{debug, error, info, instrument, warn};
use ulid::Ulid;

use crate::config::DownloadConfig as AppDownloadConfig;
use crate::db;
use crate::domain::profile::Profile;
use crate::domain::source::Source;
use crate::domain::video::{DownloadProgress, Video, VideoStatus};
use crate::ytdlp::YtdlpClient;

use super::download_worker::{DownloadConfig, DownloadOutcome, DownloadWorker, DownloadWorkerArgs};

/// Exponential backoff configuration.
const BACKOFF_BASE_SECS: u64 = 120; // 2 minutes
const BACKOFF_MAX_SECS: u64 = 3840; // 64 minutes

/// Maximum rate limit backoff multiplier.
/// With base delay of 5s and multiplier of 60, max delay is 5 minutes.
const MAX_RATE_LIMIT_MULTIPLIER: u32 = 60;

/// The download supervisor actor.
///
/// Manages concurrent downloads using a semaphore and handles retry logic
/// with exponential backoff.
pub struct DownloadSupervisor {
    /// Database pool.
    pool: PgPool,
    /// yt-dlp client.
    ytdlp: Arc<YtdlpClient>,
    /// Semaphore for limiting concurrent downloads.
    semaphore: Arc<Semaphore>,
    /// Delay between yt-dlp invocations to avoid rate limiting.
    rate_limit_delay: Duration,
    /// Last time we started a download (for rate limiting).
    last_download_start: Option<Instant>,
    /// Current rate limit backoff multiplier (increases on 429s).
    rate_limit_backoff_multiplier: u32,
    /// Active downloads (`video_id` -> worker actor ref).
    active_downloads: HashMap<Ulid, ActorRef<DownloadWorker>>,
    /// Channel for broadcasting progress updates.
    progress_tx: mpsc::Sender<DownloadProgress>,
    /// Download timeout.
    download_timeout: Duration,
    /// Maximum download attempts before marking as permanently failed.
    max_attempts: u32,
}

impl std::fmt::Debug for DownloadSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadSupervisor")
            .field("active_downloads", &self.active_downloads.len())
            .field("rate_limit_backoff", &self.rate_limit_backoff_multiplier)
            .finish_non_exhaustive()
    }
}

/// Arguments for spawning the download supervisor.
pub struct DownloadSupervisorArgs {
    pub pool: PgPool,
    pub ytdlp: Arc<YtdlpClient>,
    pub config: AppDownloadConfig,
    pub progress_tx: mpsc::Sender<DownloadProgress>,
}

impl Actor for DownloadSupervisor {
    type Args = DownloadSupervisorArgs;
    type Error = color_eyre::eyre::Error;

    async fn on_start(args: Self::Args, _actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        info!(
            max_concurrent = args.config.max_concurrent,
            timeout = ?args.config.timeout,
            "Download supervisor starting"
        );

        let supervisor = Self {
            pool: args.pool,
            ytdlp: args.ytdlp,
            semaphore: Arc::new(Semaphore::new(args.config.max_concurrent as usize)),
            rate_limit_delay: args.config.rate_limit_delay,
            last_download_start: None,
            rate_limit_backoff_multiplier: 1,
            active_downloads: HashMap::new(),
            progress_tx: args.progress_tx,
            download_timeout: args.config.timeout,
            max_attempts: args.config.max_attempts,
        };

        Ok(supervisor)
    }

    async fn on_stop(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        reason: ActorStopReason,
    ) -> Result<(), Self::Error> {
        info!(
            reason = ?reason,
            active_downloads = self.active_downloads.len(),
            "Download supervisor stopping"
        );

        // Stop all active workers
        for (video_id, worker_ref) in self.active_downloads.drain() {
            debug!(video_id = %video_id, "Stopping download worker");
            worker_ref.stop_gracefully().await.ok();
        }

        Ok(())
    }
}

/// Request to enqueue a video for download.
#[derive(Debug, Clone)]
pub struct EnqueueDownload {
    pub video: Video,
    pub profile: Profile,
    pub source: Source,
}

impl Message<EnqueueDownload> for DownloadSupervisor {
    type Reply = Result<(), String>;

    #[instrument(skip_all, fields(video_id = %msg.video.id))]
    async fn handle(
        &mut self,
        msg: EnqueueDownload,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let video_id = msg.video.id;

        // Check if already downloading
        if self.active_downloads.contains_key(&video_id) {
            debug!("Video already being downloaded");
            return Ok(());
        }

        // Check video status
        match msg.video.status {
            VideoStatus::Completed | VideoStatus::PermanentlyFailed | VideoStatus::Skipped => {
                debug!(status = ?msg.video.status, "Video not eligible for download");
                return Ok(());
            }
            VideoStatus::Failed => {
                // Check if it's time to retry
                if let Some(next_retry) = msg.video.next_retry
                    && next_retry > Utc::now()
                {
                    debug!(next_retry = %next_retry, "Video not ready for retry yet");
                    return Ok(());
                }
            }
            VideoStatus::Pending | VideoStatus::Downloading | VideoStatus::Cleaned => {
                // Eligible for download
            }
        }

        // Spawn the download task
        let supervisor_ref = ctx.actor_ref().clone();
        let pool = self.pool.clone();
        let ytdlp = self.ytdlp.clone();
        let semaphore = self.semaphore.clone();
        let progress_tx = self.progress_tx.clone();
        let download_timeout = self.download_timeout;
        let rate_limit_delay = self.effective_rate_limit_delay();
        let last_download_start = self.last_download_start;

        let video = msg.video;
        let profile = msg.profile;
        let source = msg.source;

        // Spawn a task to handle the download with rate limiting and semaphore
        tokio::spawn(async move {
            // Wait for rate limit delay since last download
            if let Some(last_start) = last_download_start {
                let elapsed = last_start.elapsed();
                if elapsed < rate_limit_delay {
                    let wait_time = rate_limit_delay.checked_sub(elapsed).unwrap();
                    debug!(wait_ms = wait_time.as_millis(), "Rate limit delay");
                    tokio::time::sleep(wait_time).await;
                }
            }

            // Acquire semaphore permit
            let _permit = semaphore.acquire().await.expect("Semaphore closed");

            debug!(video_id = %video_id, "Acquired download permit");

            // Notify supervisor we're starting
            let _ = supervisor_ref.tell(DownloadStarting { video_id }).await;

            // Create download config from profile
            let config = DownloadConfig {
                timeout: download_timeout,
                quality: profile.quality.clone(),
                output_dir: PathBuf::from(&profile.output_dir),
                naming_template: profile.naming_template.clone(),
                source_id: source.id,
                source_name: source
                    .custom_name
                    .clone()
                    .unwrap_or_else(|| source.url.clone()),
            };

            // Spawn the worker actor
            let worker_args = DownloadWorkerArgs {
                pool: pool.clone(),
                video: video.clone(),
                config,
                ytdlp,
                progress_tx,
            };

            let worker_ref = DownloadWorker::spawn(worker_args);

            // Register the worker
            let _ = supervisor_ref
                .tell(RegisterWorker {
                    video_id,
                    worker_ref: worker_ref.clone(),
                })
                .await;

            // Wait for the worker to complete
            worker_ref.wait_for_shutdown().await;

            // Notify supervisor that download completed
            // The worker will have already updated the database
            let _ = supervisor_ref.tell(DownloadCompleted { video_id }).await;
        });

        // Update last download start time
        self.last_download_start = Some(Instant::now());

        Ok(())
    }
}

/// Internal message: download is starting.
struct DownloadStarting {
    video_id: Ulid,
}

impl Message<DownloadStarting> for DownloadSupervisor {
    type Reply = ();

    async fn handle(&mut self, msg: DownloadStarting, _ctx: &mut Context<Self, Self::Reply>) {
        debug!(video_id = %msg.video_id, "Download starting");
        self.last_download_start = Some(Instant::now());
    }
}

/// Internal message: register a worker.
struct RegisterWorker {
    video_id: Ulid,
    worker_ref: ActorRef<DownloadWorker>,
}

impl Message<RegisterWorker> for DownloadSupervisor {
    type Reply = ();

    async fn handle(&mut self, msg: RegisterWorker, _ctx: &mut Context<Self, Self::Reply>) {
        self.active_downloads.insert(msg.video_id, msg.worker_ref);
        debug!(
            video_id = %msg.video_id,
            active_count = self.active_downloads.len(),
            "Worker registered"
        );
    }
}

/// Internal message: download completed (success or failure).
struct DownloadCompleted {
    video_id: Ulid,
}

impl Message<DownloadCompleted> for DownloadSupervisor {
    type Reply = ();

    async fn handle(&mut self, msg: DownloadCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        self.active_downloads.remove(&msg.video_id);
        debug!(
            video_id = %msg.video_id,
            active_count = self.active_downloads.len(),
            "Download completed, worker unregistered"
        );
    }
}

/// Report a download outcome (called by worker or supervisor logic).
#[derive(Debug, Clone)]
pub struct ReportOutcome {
    pub outcome: DownloadOutcome,
}

impl Message<ReportOutcome> for DownloadSupervisor {
    type Reply = ();

    #[instrument(skip_all)]
    async fn handle(&mut self, msg: ReportOutcome, _ctx: &mut Context<Self, Self::Reply>) {
        match msg.outcome {
            DownloadOutcome::Success {
                video_id,
                file_path,
                file_size_bytes,
            } => {
                info!(
                    video_id = %video_id,
                    file_path = %file_path.display(),
                    file_size = file_size_bytes,
                    "Download successful"
                );
                // Reset rate limit backoff on success
                self.rate_limit_backoff_multiplier = 1;
            }
            DownloadOutcome::Failed {
                video_id,
                error,
                is_rate_limited,
            } => {
                self.handle_failure(video_id, &error, is_rate_limited).await;
            }
        }
    }
}

/// Request to process all pending downloads.
pub struct ProcessPendingDownloads;

impl Message<ProcessPendingDownloads> for DownloadSupervisor {
    type Reply = Result<usize, String>;

    #[instrument(skip_all)]
    async fn handle(
        &mut self,
        _msg: ProcessPendingDownloads,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Get all videos ready for download
        let videos = db::list_videos_ready_for_download(&self.pool)
            .await
            .map_err(|e| e.to_string())?;

        let count = videos.len();
        if count == 0 {
            debug!("No pending downloads");
            return Ok(0);
        }

        info!(count, "Processing pending downloads");

        // For each video, we need its profile to get download settings
        // This is a simplified version - in a full implementation we'd
        // look up the profile through the source linkage
        for video in videos {
            // Get the source(s) for this video to find the profile
            let source_ids = db::get_sources_for_video(&self.pool, video.id)
                .await
                .map_err(|e| e.to_string())?;

            if source_ids.is_empty() {
                warn!(video_id = %video.id, "Video has no linked sources, skipping");
                continue;
            }

            // Get the first source's profile (in a real app, might need better logic)
            let source = db::get_source(&self.pool, source_ids[0])
                .await
                .map_err(|e| e.to_string())?;

            let profile = db::get_profile(&self.pool, source.profile_id)
                .await
                .map_err(|e| e.to_string())?;

            // Enqueue the download
            ctx.actor_ref()
                .tell(EnqueueDownload {
                    video,
                    profile,
                    source,
                })
                .try_send()
                .map_err(|e| e.to_string())?;
        }

        Ok(count)
    }
}

/// Get the current status of the supervisor.
pub struct GetSupervisorStatus;

/// Status information for the download supervisor.
#[derive(Debug, Clone, Reply)]
pub struct SupervisorStatus {
    pub active_downloads: usize,
    pub available_permits: usize,
    pub rate_limit_backoff: u32,
}

impl Message<GetSupervisorStatus> for DownloadSupervisor {
    type Reply = SupervisorStatus;

    async fn handle(
        &mut self,
        _msg: GetSupervisorStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        SupervisorStatus {
            active_downloads: self.active_downloads.len(),
            available_permits: self.semaphore.available_permits(),
            rate_limit_backoff: self.rate_limit_backoff_multiplier,
        }
    }
}

/// Notify the supervisor of a rate limit (429) response.
pub struct NotifyRateLimited;

impl Message<NotifyRateLimited> for DownloadSupervisor {
    type Reply = ();

    async fn handle(&mut self, _msg: NotifyRateLimited, _ctx: &mut Context<Self, Self::Reply>) {
        // Increase backoff multiplier (exponentially)
        self.rate_limit_backoff_multiplier =
            (self.rate_limit_backoff_multiplier * 2).min(MAX_RATE_LIMIT_MULTIPLIER);

        warn!(
            backoff_multiplier = self.rate_limit_backoff_multiplier,
            effective_delay_secs = self.effective_rate_limit_delay().as_secs(),
            "Rate limit backoff increased"
        );
    }
}

impl DownloadSupervisor {
    /// Calculate the effective rate limit delay considering backoff.
    fn effective_rate_limit_delay(&self) -> Duration {
        Duration::from_secs(
            self.rate_limit_delay.as_secs() * u64::from(self.rate_limit_backoff_multiplier),
        )
    }

    /// Handle a download failure with retry scheduling.
    async fn handle_failure(&mut self, video_id: Ulid, error: &str, is_rate_limited: bool) {
        if is_rate_limited {
            // Increase global rate limit backoff
            self.rate_limit_backoff_multiplier =
                (self.rate_limit_backoff_multiplier * 2).min(MAX_RATE_LIMIT_MULTIPLIER);
            warn!(
                backoff_multiplier = self.rate_limit_backoff_multiplier,
                "Rate limit hit, increasing backoff"
            );
        }

        // Get current video to check attempts
        let video = match db::get_video(&self.pool, video_id).await {
            Ok(v) => v,
            Err(e) => {
                error!(error = %e, "Failed to get video for retry scheduling");
                return;
            }
        };

        let attempts = video.attempts;

        // Convert max_attempts to i32 for comparison (safe since max_attempts is small)
        let max_attempts_i32 = i32::try_from(self.max_attempts).unwrap_or(i32::MAX);

        if attempts >= max_attempts_i32 {
            // Mark as permanently failed
            error!(
                video_id = %video_id,
                attempts,
                "Max attempts reached, marking as permanently failed"
            );
            if let Err(e) = db::mark_video_failed(&self.pool, video_id, error, None).await {
                error!(error = %e, "Failed to mark video as permanently failed");
            }
        } else {
            // Schedule retry with exponential backoff
            // attempts is guaranteed non-negative here since we only get here after incrementing
            let attempts_u32 = u32::try_from(attempts).unwrap_or(0);
            let backoff_secs = BACKOFF_BASE_SECS.saturating_mul(2u64.saturating_pow(attempts_u32));
            let capped_backoff = backoff_secs.min(BACKOFF_MAX_SECS);
            // capped_backoff is at most BACKOFF_MAX_SECS (3840) which fits in i64
            let next_retry = Utc::now()
                + chrono::Duration::seconds(i64::try_from(capped_backoff).unwrap_or(i64::MAX));

            warn!(
                video_id = %video_id,
                attempts,
                next_retry = %next_retry,
                backoff_secs = capped_backoff,
                "Scheduling retry"
            );

            if let Err(e) =
                db::mark_video_failed(&self.pool, video_id, error, Some(next_retry)).await
            {
                error!(error = %e, "Failed to schedule retry");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_backoff() {
        // Test backoff calculation
        let base = BACKOFF_BASE_SECS;
        let max = BACKOFF_MAX_SECS;

        assert_eq!(base * 2u64.pow(0), 120); // 2 min
        assert_eq!(base * 2u64.pow(1), 240); // 4 min
        assert_eq!(base * 2u64.pow(2), 480); // 8 min
        assert_eq!(base * 2u64.pow(3), 960); // 16 min
        assert_eq!(base * 2u64.pow(4), 1920); // 32 min
        assert_eq!((base * 2u64.pow(5)).min(max), 3840); // 64 min (capped)
        assert_eq!((base * 2u64.pow(6)).min(max), 3840); // still capped
    }

    #[test]
    fn test_rate_limit_backoff() {
        let base_delay = Duration::from_secs(5);

        let effective_1x = base_delay.as_secs();
        let effective_2x = base_delay.as_secs() * 2;
        let effective_4x = base_delay.as_secs() * 4;

        assert_eq!(effective_1x, 5);
        assert_eq!(effective_2x, 10);
        assert_eq!(effective_4x, 20);
    }
}
