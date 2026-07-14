//! The `DownloadSupervisor` is a singleton that manages download concurrency.
//!
//! It holds a `tokio::sync::Semaphore` with a configurable number of permits
//! (default 3) and spawns short-lived `DownloadWorker` actors when permits
//! are available. It also handles retry logic with exponential backoff.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use kameo::Reply;
use kameo::prelude::*;
use metrics::{counter, gauge};
use sqlx::PgPool;
use tokio::sync::{Semaphore, mpsc};
use tokio::time::Instant;
use tracing::{debug, error, info, instrument, warn};
use ulid::Ulid;

use crate::config::DownloadConfig as AppDownloadConfig;
use crate::db;
use crate::db::ActivityBroadcaster;
use crate::domain::activity::{ActivityEventType, ActivitySeverity};
use crate::domain::profile::{OutputPreset, Profile, Quality};
use crate::domain::source::Source;
use crate::domain::video::{DownloadProgress, Video, VideoStatus};
use crate::ytdlp::FallbackStage;
use crate::ytdlp::YtdlpClient;

use super::download_worker::{
    DownloadConfig, DownloadOutcome, DownloadWorker, DownloadWorkerArgs, StartDownload,
};

struct FailureContext<'a> {
    error: &'a str,
    error_code: Option<&'static str>,
    preset: &'a OutputPreset,
    quality: &'a Quality,
    fallback_stage: Option<FallbackStage>,
    is_rate_limited: bool,
}

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
    /// Videos with an in-flight dispatch (reserved before a worker is
    /// registered). Guards against the same video being dispatched more than
    /// once concurrently: a video is reserved here synchronously when an
    /// `EnqueueDownload` is accepted, before the spawned task acquires a
    /// permit and registers its worker in `active_downloads`.
    dispatching: HashSet<Ulid>,
    /// Channel for broadcasting progress updates.
    progress_tx: mpsc::Sender<DownloadProgress>,
    /// Download timeout.
    download_timeout: Duration,
    /// Maximum download attempts before marking as permanently failed.
    max_attempts: u32,
    /// Broadcaster for real-time SSE notifications.
    broadcaster: ActivityBroadcaster,
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
    pub broadcaster: ActivityBroadcaster,
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
            dispatching: HashSet::new(),
            progress_tx: args.progress_tx,
            download_timeout: args.config.timeout,
            max_attempts: args.config.max_attempts,
            broadcaster: args.broadcaster,
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
        self.dispatching.clear();

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

        // Check video status
        match msg.video.status {
            VideoStatus::Completed
            | VideoStatus::PermanentlyFailed
            | VideoStatus::Skipped
            // A video already in `downloading` is either actively handled by a
            // worker or stuck from a crash (reset to `pending` on startup);
            // never re-dispatch it from a stale snapshot.
            | VideoStatus::Downloading => {
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
            VideoStatus::Pending | VideoStatus::Cleaned => {
                // Eligible for download
            }
        }

        // Reserve a single in-flight dispatch for this video. This runs
        // synchronously (no `.await` between here and the spawn below), so it
        // closes the race where repeated `EnqueueDownload`s for the same video
        // each spawned a worker before the first registered in
        // `active_downloads` — the bug that caused runaway re-downloads.
        if !Self::reserve_dispatch(
            self.active_downloads.contains_key(&video_id),
            &mut self.dispatching,
            video_id,
        ) {
            debug!("Video already downloading or queued for dispatch");
            return Ok(());
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
                    let wait_time = rate_limit_delay.saturating_sub(elapsed);
                    debug!(wait_ms = wait_time.as_millis(), "Rate limit delay");
                    tokio::time::sleep(wait_time).await;
                }
            }

            // Acquire semaphore permit
            let Ok(_permit) = semaphore.acquire().await else {
                // Semaphore closed during shutdown. Release the dispatch
                // reservation so the video isn't left wedged as "in flight".
                debug!(video_id = %video_id, "Semaphore closed, aborting download");
                let _ = supervisor_ref.tell(DownloadCompleted { video_id }).await;
                return;
            };

            debug!(video_id = %video_id, "Acquired download permit");

            // Notify supervisor we're starting
            let _ = supervisor_ref.tell(DownloadStarting { video_id }).await;

            // Create download config from profile
            let config = DownloadConfig {
                timeout: download_timeout,
                quality: profile.quality.clone(),
                output_preset: profile.output_preset.clone(),
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

            // Ask the worker to start the download and wait for the outcome
            let outcome = worker_ref.ask(StartDownload).await;

            // Notify supervisor that download completed
            // The worker will have already updated the database
            let _ = supervisor_ref.tell(DownloadCompleted { video_id }).await;

            // Report the outcome so activity gets logged
            if let Ok(outcome) = outcome {
                let _ = supervisor_ref.tell(ReportOutcome { outcome }).await;
            }
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

    #[instrument(skip_all, fields(video_id = %msg.video_id))]
    async fn handle(&mut self, msg: DownloadStarting, _ctx: &mut Context<Self, Self::Reply>) {
        debug!(video_id = %msg.video_id, "Download starting");
        self.last_download_start = Some(Instant::now());

        // Look up video title for the activity message
        let message = match db::get_video(&self.pool, msg.video_id).await {
            Ok(v) => format!("Started downloading \"{}\"", v.title),
            Err(_) => format!("Started downloading video {}", msg.video_id),
        };
        self.broadcaster
            .log_and_broadcast(
                &self.pool,
                ActivityEventType::DownloadStarted,
                ActivitySeverity::Info,
                &message,
                None,
                Some(msg.video_id),
                None,
            )
            .await;
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
        #[allow(clippy::cast_precision_loss)]
        gauge!(crate::metrics::DOWNLOADS_ACTIVE).set(self.active_downloads.len() as f64);
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

    #[instrument(skip_all, fields(video_id = %msg.video_id))]
    async fn handle(&mut self, msg: DownloadCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        self.active_downloads.remove(&msg.video_id);
        // Release the in-flight dispatch reservation so the video can be
        // dispatched again on a future tick (if still eligible).
        self.dispatching.remove(&msg.video_id);
        #[allow(clippy::cast_precision_loss)]
        gauge!(crate::metrics::DOWNLOADS_ACTIVE).set(self.active_downloads.len() as f64);
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
                counter!(crate::metrics::DOWNLOADS_COMPLETED_TOTAL).increment(1);
                info!(
                    video_id = %video_id,
                    file_path = %file_path.display(),
                    file_size = file_size_bytes,
                    "Download successful"
                );
                // Reset rate limit backoff on success
                self.rate_limit_backoff_multiplier = 1;

                #[allow(clippy::cast_precision_loss)]
                let size_mb = file_size_bytes as f64 / 1_048_576.0;
                let message = format!(
                    "Completed \"{}\" ({size_mb:.1} MB)",
                    file_path.file_name().unwrap_or_default().to_string_lossy()
                );
                self.broadcaster
                    .log_and_broadcast(
                        &self.pool,
                        ActivityEventType::DownloadCompleted,
                        ActivitySeverity::Success,
                        &message,
                        None,
                        Some(video_id),
                        None,
                    )
                    .await;
            }
            DownloadOutcome::Failed {
                video_id,
                error,
                error_code,
                preset,
                quality,
                fallback_stage,
                is_rate_limited,
            } => {
                let failure = FailureContext {
                    error: &error,
                    error_code,
                    preset: &preset,
                    quality: &quality,
                    fallback_stage,
                    is_rate_limited,
                };
                self.handle_failure(video_id, failure).await;
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

/// Cancel an active or pending download.
pub struct CancelDownload {
    pub video_id: Ulid,
}

impl Message<CancelDownload> for DownloadSupervisor {
    type Reply = Result<(), String>;

    #[instrument(skip_all, fields(video_id = %msg.video_id))]
    async fn handle(
        &mut self,
        msg: CancelDownload,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Stop the worker if actively downloading
        if let Some(worker_ref) = self.active_downloads.remove(&msg.video_id) {
            info!(video_id = %msg.video_id, "Cancelling active download");
            worker_ref.stop_gracefully().await.ok();
        }
        // Clear any in-flight dispatch reservation for this video.
        self.dispatching.remove(&msg.video_id);

        // Mark as failed in the DB
        db::update_video_status(&self.pool, msg.video_id, VideoStatus::Failed)
            .await
            .map_err(|e| format!("Failed to update video status: {e}"))?;

        self.broadcaster
            .log_and_broadcast(
                &self.pool,
                ActivityEventType::DownloadFailed,
                ActivitySeverity::Info,
                &format!("Download cancelled by user for video {}", msg.video_id),
                None,
                Some(msg.video_id),
                None,
            )
            .await;

        info!(video_id = %msg.video_id, "Download cancelled");
        Ok(())
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
    /// Reserve a single in-flight dispatch slot for `video_id`.
    ///
    /// Returns `true` if a slot was newly reserved (caller should proceed to
    /// spawn the download), or `false` if the video is already being
    /// downloaded (`already_active`) or already has a dispatch reserved.
    ///
    /// This is the dedup guard that prevents the same video from being
    /// dispatched more than once concurrently.
    fn reserve_dispatch(
        already_active: bool,
        dispatching: &mut HashSet<Ulid>,
        video_id: Ulid,
    ) -> bool {
        if already_active {
            return false;
        }
        dispatching.insert(video_id)
    }

    /// Calculate the effective rate limit delay considering backoff.
    fn effective_rate_limit_delay(&self) -> Duration {
        Duration::from_secs(
            self.rate_limit_delay.as_secs() * u64::from(self.rate_limit_backoff_multiplier),
        )
    }

    /// Handle a download failure with retry scheduling.
    #[allow(clippy::too_many_lines)]
    #[instrument(skip(self, failure), fields(video_id = %video_id))]
    async fn handle_failure(&mut self, video_id: Ulid, failure: FailureContext<'_>) {
        if failure.is_rate_limited {
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
            counter!(crate::metrics::DOWNLOADS_FAILED_TOTAL, "reason" => "permanent").increment(1);
            error!(
                video_id = %video_id,
                attempts,
                error_code = failure.error_code,
                preset = ?failure.preset,
                quality = ?failure.quality,
                fallback_stage = ?failure.fallback_stage,
                "Max attempts reached, marking as permanently failed"
            );
            let persisted_error = failure.error_code.map_or_else(
                || failure.error.to_string(),
                |code| format!("[{code}] {}", failure.error),
            );
            if let Err(e) =
                db::mark_video_failed(&self.pool, video_id, &persisted_error, None).await
            {
                error!(error = %e, "Failed to mark video as permanently failed");
            }
            let code_text = failure.error_code.unwrap_or("UNKNOWN");
            let message = format!(
                "[{code_text}] Permanently failed after {attempts} attempts — preset={:?} quality={:?} stage={:?} — {}",
                failure.preset, failure.quality, failure.fallback_stage, failure.error
            );
            self.broadcaster
                .log_and_broadcast(
                    &self.pool,
                    ActivityEventType::DownloadFailed,
                    ActivitySeverity::Error,
                    &message,
                    None,
                    Some(video_id),
                    None,
                )
                .await;
        } else {
            // Schedule retry with exponential backoff
            let reason = if failure.is_rate_limited {
                "rate_limited"
            } else {
                "retry"
            };
            counter!(crate::metrics::DOWNLOADS_FAILED_TOTAL, "reason" => reason).increment(1);
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
                error_code = failure.error_code,
                preset = ?failure.preset,
                quality = ?failure.quality,
                fallback_stage = ?failure.fallback_stage,
                "Scheduling retry"
            );

            let persisted_error = failure.error_code.map_or_else(
                || failure.error.to_string(),
                |code| format!("[{code}] {}", failure.error),
            );
            if let Err(e) =
                db::mark_video_failed(&self.pool, video_id, &persisted_error, Some(next_retry))
                    .await
            {
                error!(error = %e, "Failed to schedule retry");
            }

            let code_text = failure.error_code.unwrap_or("UNKNOWN");
            let message = format!(
                "[{code_text}] Retry #{attempts} scheduled at {next_retry} — preset={:?} quality={:?} stage={:?} — {}",
                failure.preset, failure.quality, failure.fallback_stage, failure.error
            );
            self.broadcaster
                .log_and_broadcast(
                    &self.pool,
                    ActivityEventType::RetryScheduled,
                    ActivitySeverity::Warning,
                    &message,
                    None,
                    Some(video_id),
                    None,
                )
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reserve_dispatch_dedups_concurrent_enqueues() {
        let mut dispatching = HashSet::new();
        let video_id = Ulid::r#gen();

        // First enqueue for an idle video reserves a slot.
        assert!(DownloadSupervisor::reserve_dispatch(
            false,
            &mut dispatching,
            video_id
        ));
        // A repeated enqueue while the dispatch is still in flight (worker not
        // yet registered in `active_downloads`) is rejected. This is the race
        // that previously spawned duplicate workers and caused runaway
        // re-downloads of the same video.
        assert!(!DownloadSupervisor::reserve_dispatch(
            false,
            &mut dispatching,
            video_id
        ));
        // An enqueue for a video already actively downloading is also rejected.
        assert!(!DownloadSupervisor::reserve_dispatch(
            true,
            &mut dispatching,
            video_id
        ));

        // After the dispatch completes the reservation is released and the
        // video can be dispatched again.
        dispatching.remove(&video_id);
        assert!(DownloadSupervisor::reserve_dispatch(
            false,
            &mut dispatching,
            video_id
        ));

        // A different video is independent.
        let other = Ulid::r#gen();
        assert!(DownloadSupervisor::reserve_dispatch(
            false,
            &mut dispatching,
            other
        ));
    }

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
