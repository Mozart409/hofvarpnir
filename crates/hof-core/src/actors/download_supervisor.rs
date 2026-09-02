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
use tokio::sync::{Semaphore, mpsc, watch};
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
use crate::runtime_config::{DrainToken, EffectiveSettings};
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

/// Fallback permit count when the resolved `max_concurrent_downloads` (a
/// `u32` from `EffectiveSettings`) fails to convert to `usize` — which
/// cannot happen on any real platform, but the conversion must still fall
/// back to a small bounded value, never `usize::MAX`: an unbounded semaphore
/// is exactly the unmetered download concurrency this feature exists to
/// prevent. Mirrors `runtime_config::DEFAULT_MAX_CONCURRENT`; keep in sync.
const DEFAULT_MAX_CONCURRENT: usize = 3;

/// How long the semaphore-resize watcher waits for mailbox space before
/// giving up on delivering a single resize. Mirrors
/// `scheduler::TICK_SEND_TIMEOUT` / `cleanup::TICK_SEND_TIMEOUT`.
const RESIZE_SEND_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// Total permits the semaphore is sized to, including those currently
    /// held. `Semaphore::available_permits()` excludes in-flight permits, so
    /// it cannot serve as the resize baseline.
    permits_total: usize,
    /// Live runtime settings (rate limit delay, concurrency cap, ...).
    config_rx: watch::Receiver<Arc<EffectiveSettings>>,
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
    /// Process-local drain signal. A second source (alongside the
    /// `downloads_paused` pause gate) for the same "stop dispatching new
    /// work" refusal path — see `dispatch_download` and
    /// `ProcessPendingDownloads`.
    drain: DrainToken,
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
    /// Live runtime settings, shared across all actors that consume
    /// pacing/concurrency knobs.
    pub config_rx: watch::Receiver<Arc<EffectiveSettings>>,
    pub broadcaster: ActivityBroadcaster,
    /// Process-local drain signal, threaded in from `ActorSystem`.
    pub drain: DrainToken,
}

impl Actor for DownloadSupervisor {
    type Args = DownloadSupervisorArgs;
    type Error = color_eyre::eyre::Error;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let max_concurrent =
            usize::try_from(args.config_rx.borrow().max_concurrent_downloads.value)
                .unwrap_or(DEFAULT_MAX_CONCURRENT);

        info!(
            max_concurrent,
            timeout = ?args.config.timeout,
            "Download supervisor starting"
        );

        let supervisor = Self {
            pool: args.pool,
            ytdlp: args.ytdlp,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            permits_total: max_concurrent,
            config_rx: args.config_rx.clone(),
            last_download_start: None,
            rate_limit_backoff_multiplier: 1,
            active_downloads: HashMap::new(),
            dispatching: HashSet::new(),
            progress_tx: args.progress_tx,
            download_timeout: args.config.timeout,
            max_attempts: args.config.max_attempts,
            broadcaster: args.broadcaster,
            drain: args.drain,
        };

        // Reactively resize the semaphore whenever the concurrency cap
        // changes, so a raised cap immediately wakes tasks already parked on
        // `semaphore.acquire()` rather than waiting for the next unrelated
        // dispatch to happen to call `resize_semaphore`.
        let mut watch_rx = args.config_rx;
        tokio::spawn(async move {
            loop {
                if watch_rx.changed().await.is_err() {
                    debug!("Runtime config channel closed; semaphore watcher exiting");
                    break;
                }
                if !actor_ref.is_alive() {
                    break;
                }
                let target =
                    usize::try_from(watch_rx.borrow_and_update().max_concurrent_downloads.value)
                        .unwrap_or(DEFAULT_MAX_CONCURRENT);
                match actor_ref
                    .tell(ApplySemaphoreTarget { target })
                    .mailbox_timeout(RESIZE_SEND_TIMEOUT)
                    .send()
                    .await
                {
                    Ok(()) => {}
                    Err(SendError::Timeout(_)) => {
                        warn!(
                            timeout_secs = RESIZE_SEND_TIMEOUT.as_secs(),
                            "Supervisor mailbox still full after wait, dropping this resize"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to send ApplySemaphoreTarget, actor has stopped");
                        break;
                    }
                }
            }
        });

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
        let supervisor_ref = ctx.actor_ref().clone();
        self.dispatch_download(msg, supervisor_ref).await
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

/// Internal message: apply a new semaphore permit target.
///
/// Sent by the background watcher spawned in `on_start` whenever
/// `max_concurrent_downloads` changes, so growth wakes already-queued
/// `semaphore.acquire()` callers immediately instead of waiting for the next
/// unrelated dispatch.
struct ApplySemaphoreTarget {
    target: usize,
}

impl Message<ApplySemaphoreTarget> for DownloadSupervisor {
    type Reply = ();

    async fn handle(&mut self, msg: ApplySemaphoreTarget, _ctx: &mut Context<Self, Self::Reply>) {
        self.resize_semaphore(msg.target);
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
        // `usize` has no lossless conversion to `f64`; active download counts are
        // bounded by `max_concurrent` (far below 2^53), so precision loss is moot.
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
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

        // Reconcile the semaphore against the current target now that this
        // download's permit has been released (see the `drop(permit)` in
        // `dispatch_download`'s spawned task, which runs strictly before this
        // message is sent). A shrink that couldn't fully reclaim free permits
        // when it was first requested converges here, one freed permit at a
        // time, as in-flight downloads finish. `resize_semaphore` is a no-op
        // when `target == permits_total`, so this costs nothing on the common
        // path where the cap hasn't changed.
        let target = usize::try_from(self.config_rx.borrow().max_concurrent_downloads.value)
            .unwrap_or(DEFAULT_MAX_CONCURRENT);
        self.resize_semaphore(target);

        // See the analogous conversion in `RegisterWorker::handle` above: `usize`
        // has no lossless conversion to `f64`, and the count is always small.
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
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

                // `i64` has no lossless conversion to `f64`; this value is only used
                // for a human-readable MB figure in a log message, so precision loss
                // (only material above 2^53 bytes) is irrelevant here.
                #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
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
        // Optimisation only, not the authoritative gate: skips the DB query
        // below when there is no point running it, since a pause will
        // discard whatever it returns. The real gate every dispatch path
        // must pass through lives in `dispatch_download` (see its doc
        // comment) — do not remove this early return under the assumption
        // it is redundant, but also do not treat it as sufficient on its
        // own; `EnqueueDownload` reaches `dispatch_download` without ever
        // passing through here.
        if self.config_rx.borrow().downloads_paused(Utc::now()) || self.drain.is_draining() {
            debug!("Downloads paused or draining; leaving videos pending");
            return Ok(0);
        }

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
        let supervisor_ref = ctx.actor_ref().clone();
        for video in videos {
            // Get the source(s) for this video to find the profile
            let source_ids = db::get_sources_for_video(&self.pool, video.id)
                .await
                .map_err(|e| e.to_string())?;

            let Some(&first_source_id) = source_ids.first() else {
                warn!(video_id = %video.id, "Video has no linked sources, skipping");
                continue;
            };

            // Get the first source's profile (in a real app, might need better logic)
            let source = db::get_source(&self.pool, first_source_id)
                .await
                .map_err(|e| e.to_string())?;

            let profile = db::get_profile(&self.pool, source.profile_id)
                .await
                .map_err(|e| e.to_string())?;

            // Dispatch inline (not via the mailbox) so the whole backlog is
            // processed even when it exceeds the bounded mailbox capacity.
            if let Err(e) = self
                .dispatch_download(
                    EnqueueDownload {
                        video,
                        profile,
                        source,
                    },
                    supervisor_ref.clone(),
                )
                .await
            {
                warn!(error = %e, "Failed to dispatch pending download");
            }
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
    /// Videos with an in-flight dispatch reservation that have not yet
    /// registered a worker (see `dispatching` on `DownloadSupervisor`).
    /// Quiescence for drain purposes requires this to be zero too, not just
    /// `active_downloads`: a video can sit reserved-but-not-active while its
    /// spawned task is still waiting out the rate-limit delay.
    pub dispatching: usize,
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
            dispatching: self.dispatching.len(),
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
        self.rate_limit_backoff_multiplier = self
            .rate_limit_backoff_multiplier
            .saturating_mul(2)
            .min(MAX_RATE_LIMIT_MULTIPLIER);

        let base_delay = self.config_rx.borrow().rate_limit_delay.value;
        warn!(
            backoff_multiplier = self.rate_limit_backoff_multiplier,
            effective_delay_secs = self.effective_rate_limit_delay(base_delay).as_secs(),
            "Rate limit backoff increased"
        );
    }
}

impl DownloadSupervisor {
    /// Dispatch a single video for download.
    ///
    /// Shared by the `EnqueueDownload` message handler and the
    /// `ProcessPendingDownloads` sweep. Called directly (not via the actor
    /// mailbox) so a large pending backlog is never dropped by bounded-mailbox
    /// backpressure. Checks the downloads-pause gate, runs the eligibility
    /// check, reserves a dispatch slot, and spawns the rate-limited download
    /// task.
    ///
    /// Kept `async` (though the current body has no top-level `.await`
    /// outside the spawned task) to match the call sites, which `.await`
    /// this as a natural extension of the `Message<EnqueueDownload>` handler
    /// it was extracted from.
    #[allow(clippy::unused_async)]
    async fn dispatch_download(
        &mut self,
        msg: EnqueueDownload,
        supervisor_ref: ActorRef<Self>,
    ) -> Result<(), String> {
        let video_id = msg.video.id;

        // Authoritative downloads-pause gate. This is the single choke point
        // every dispatch path passes through (`EnqueueDownload` — fired from
        // the indexer on every newly discovered video, and from manual
        // API/web actions — and the `ProcessPendingDownloads` sweep both
        // call this method), so it must live here rather than only in the
        // sweep. Placed before anything below is touched: no DB write, no
        // `self.dispatching` reservation, no `self.active_downloads` entry
        // has happened yet, so returning here leaves the video exactly as it
        // arrived (typically `pending`) and untracked — required for the
        // backlog to drain naturally once the pause lifts. `Ok(())` because
        // a deliberate pause is "accepted, not started," not a failure; an
        // `Err` here would make `EnqueueDownload`'s callers (indexer, API,
        // web) log or surface a spurious error for an operator action.
        if self.config_rx.borrow().downloads_paused(Utc::now()) || self.drain.is_draining() {
            debug!(video_id = %video_id, "Downloads paused or draining; leaving video pending");
            return Ok(());
        }

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
        let pool = self.pool.clone();
        let ytdlp = self.ytdlp.clone();
        let semaphore = self.semaphore.clone();
        let progress_tx = self.progress_tx.clone();
        let download_timeout = self.download_timeout;
        let base_delay = self.config_rx.borrow().rate_limit_delay.value;
        let rate_limit_delay = self.effective_rate_limit_delay(base_delay);
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
            let Ok(permit) = semaphore.acquire().await else {
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

            // Release the semaphore permit before reporting completion. This
            // guarantees that by the time the supervisor's `DownloadCompleted`
            // handler runs (and reconciles a pending concurrency-cap shrink
            // via `resize_semaphore`), the permit has already been returned
            // to the semaphore's free pool -- reclaiming it before this drop
            // would just no-op.
            drop(permit);

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
    ///
    /// `base` is the current `rate_limit_delay` read fresh from
    /// `EffectiveSettings` at the call site, rather than a value cached at
    /// actor startup, so a runtime change takes effect on the very next
    /// dispatch.
    fn effective_rate_limit_delay(&self, base: Duration) -> Duration {
        Duration::from_secs(
            base.as_secs()
                .saturating_mul(u64::from(self.rate_limit_backoff_multiplier)),
        )
    }

    /// Resize the download semaphore.
    ///
    /// Growing is immediate: `Semaphore::add_permits` wakes any tasks
    /// already parked on `acquire()`. Shrinking reclaims whatever permits
    /// are free at the moment of the call immediately (`forget_permits`
    /// keeps no debt bookkeeping for permits still in flight, so this alone
    /// cannot always reach `target`); the remainder converges as in-flight
    /// downloads finish, because `DownloadCompleted`'s handler re-applies
    /// the current target after releasing its permit. Idempotent: calling
    /// with `target == permits_total` (the common case, run on every
    /// completed download) is a no-op.
    fn resize_semaphore(&mut self, target: usize) {
        if target > self.permits_total {
            let delta = target.saturating_sub(self.permits_total);
            self.semaphore.add_permits(delta);
            self.permits_total = target;
        } else if target < self.permits_total {
            let delta = self.permits_total.saturating_sub(target);
            let removed = self.semaphore.forget_permits(delta);
            self.permits_total = self.permits_total.saturating_sub(removed);
        }
    }

    /// Handle a download failure with retry scheduling.
    #[allow(clippy::too_many_lines)]
    #[instrument(skip(self, failure), fields(video_id = %video_id))]
    async fn handle_failure(&mut self, video_id: Ulid, failure: FailureContext<'_>) {
        if failure.is_rate_limited {
            // Increase global rate limit backoff
            self.rate_limit_backoff_multiplier = self
                .rate_limit_backoff_multiplier
                .saturating_mul(2)
                .min(MAX_RATE_LIMIT_MULTIPLIER);
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
            let backoff_duration =
                chrono::Duration::seconds(i64::try_from(capped_backoff).unwrap_or(i64::MAX));
            // `checked_add_signed` avoids a panic on `DateTime` overflow; a
            // few thousand seconds from now will never overflow in practice,
            // but falling back to "now" is a safe, harmless degradation.
            let next_retry = Utc::now()
                .checked_add_signed(backoff_duration)
                .unwrap_or_else(Utc::now);

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
        let video_id = Ulid::generate();

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
        let other = Ulid::generate();
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
    fn semaphore_grows_immediately() {
        let sem = Arc::new(Semaphore::new(2));
        sem.add_permits(3);
        assert_eq!(sem.available_permits(), 5);
    }

    #[tokio::test]
    async fn semaphore_shrink_only_reclaims_free_permits() {
        let sem = Arc::new(Semaphore::new(3));
        let _held = sem.clone().acquire_owned().await.expect("permit");
        // 2 free, 1 held: asking to remove 3 can only remove the 2 free ones.
        let removed = sem.forget_permits(3);
        assert_eq!(removed, 2);
        assert_eq!(sem.available_permits(), 0);
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

    // NOTE: this exercises `EffectiveSettings::downloads_paused` directly via
    // `resolve`, not either of the actor-level gates that consume it
    // (`ProcessPendingDownloads`'s early-return optimisation, or the
    // authoritative check in `dispatch_download`) — there is no actor-level
    // assertion here that a paused dispatch actually leaves a video
    // undispatched.
    #[test]
    fn paused_downloads_leave_indexing_running() {
        use crate::db::RuntimeSettingsRow;
        use crate::runtime_config::{EnvOverrides, resolve};

        let row = RuntimeSettingsRow {
            downloads_paused_until: Some(Utc::now() + chrono::Duration::hours(1)),
            ..RuntimeSettingsRow::default()
        };
        let s = resolve(&row, &EnvOverrides::default());
        assert!(s.downloads_paused(Utc::now()));
        assert!(!s.indexing_paused(Utc::now()));
    }

    // ========================================================================
    // Actor-level gate tests (Ruling F).
    //
    // `paused_downloads_leave_indexing_running` above (and its Task 4
    // equivalent) only asserts on `EffectiveSettings`, never on the actor
    // itself — it would pass unchanged if either gate block in
    // `dispatch_download`/`ProcessPendingDownloads` were deleted. These two
    // tests spawn a real `DownloadSupervisor` and assert on its own state
    // instead.
    //
    // The assertion deliberately checks `dispatching`, not just
    // `active_downloads`: `reserve_dispatch` (which sets `dispatching`) runs
    // synchronously inside the `EnqueueDownload` handler, before the handler
    // returns and before any `tokio::spawn`'d work runs, and Kameo actors
    // process their mailbox one message at a time. So by the time the
    // `GetSupervisorStatus` that follows is handled, `dispatching` reflects
    // exactly what `dispatch_download` did — deterministically, no sleep, no
    // race against the spawned download task. `active_downloads` alone does
    // NOT have this property: it is only populated later, by a `tell` from
    // that spawned task, so a `GetSupervisorStatus` sent immediately after
    // `EnqueueDownload` can race it and read 0 either way. Both are asserted
    // below (matching the brief's shape and because a real regression would
    // eventually populate `active_downloads` too), but `dispatching` is the
    // assertion this test actually depends on to fail on a deleted gate.
    // ========================================================================

    /// Spawn a real `DownloadSupervisor` wired to the given settings/drain
    /// channel. `YtdlpClient::new` is path-only construction (no process
    /// spawn — see `ytdlp.rs:449`), so this needs no live yt-dlp binary.
    async fn spawn_test_supervisor(
        pool: PgPool,
        config_rx: watch::Receiver<Arc<EffectiveSettings>>,
        drain: DrainToken,
    ) -> ActorRef<DownloadSupervisor> {
        let ytdlp = Arc::new(
            YtdlpClient::new("yt-dlp", None, std::path::Path::new("/tmp"))
                .await
                .expect("YtdlpClient::new is path-only construction and cannot fail here"),
        );
        let (progress_tx, _progress_rx) = mpsc::channel(10);
        let config = AppDownloadConfig {
            max_concurrent: 2,
            timeout: Duration::from_hours(1),
            max_attempts: 3,
            rate_limit_delay: Duration::from_millis(50),
            ytdlp_path: PathBuf::from("yt-dlp"),
        };

        DownloadSupervisor::spawn(DownloadSupervisorArgs {
            pool,
            ytdlp,
            config,
            progress_tx,
            config_rx,
            broadcaster: ActivityBroadcaster::new(),
            drain,
        })
    }

    /// Seed a `pending` video with a linked profile/source, ready to hand to
    /// `EnqueueDownload`.
    async fn seed_pending_video(pool: &PgPool) -> (Video, Profile, Source) {
        use crate::domain::source::SourceType;

        let user = db::create_user(
            pool,
            db::CreateUser {
                email: "gate-test@example.com",
                name: "Gate Test",
                password_hash: None,
            },
        )
        .await
        .expect("create user");

        let profile = db::create_profile(
            pool,
            db::CreateProfile {
                user_id: user.id,
                name: "Gate Test Profile",
                quality: Quality::Q1080p,
                output_preset: OutputPreset::Auto,
                naming_template: "{title}.{ext}",
                output_dir: "/tmp/hof-gate-test",
                include_livestreams: false,
                include_shorts: false,
                storage_quota_bytes: 100_000_000_000,
                retention_days: None,
            },
        )
        .await
        .expect("create profile");

        let source = db::create_source(
            pool,
            db::CreateSource {
                profile_id: profile.id,
                url: "https://example.com/channel",
                source_type: SourceType::Channel,
                custom_name: None,
                index_frequency_secs: 3600,
                cutoff_date: chrono::NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date"),
                retention_days: None,
            },
        )
        .await
        .expect("create source");

        let video = db::create_video(
            pool,
            db::CreateVideo {
                platform: "youtube",
                platform_video_id: "gate-test-video",
                title: "Gate Test Video",
                description: None,
                duration_secs: Some(60),
                published_at: None,
                thumbnail_url: None,
            },
        )
        .await
        .expect("create video");

        (video, profile, source)
    }

    #[sqlx::test]
    async fn dispatch_download_respects_downloads_pause_gate(pool: PgPool) {
        use crate::db::RuntimeSettingsRow;
        use crate::runtime_config::{EnvOverrides, resolve};

        let row = RuntimeSettingsRow {
            downloads_paused_until: Some(Utc::now() + chrono::Duration::hours(1)),
            ..RuntimeSettingsRow::default()
        };
        let settings = Arc::new(resolve(&row, &EnvOverrides::default()));
        let (_settings_tx, config_rx) = watch::channel(settings);

        let supervisor = spawn_test_supervisor(pool.clone(), config_rx, DrainToken::new()).await;
        let (video, profile, source) = seed_pending_video(&pool).await;
        let video_id = video.id;

        supervisor
            .ask(EnqueueDownload {
                video,
                profile,
                source,
            })
            .await
            .expect("EnqueueDownload accepted");

        let status = supervisor
            .ask(GetSupervisorStatus)
            .await
            .expect("GetSupervisorStatus");
        assert_eq!(
            status.dispatching, 0,
            "a paused dispatch must not reserve a dispatch slot"
        );
        assert_eq!(status.active_downloads, 0);

        let reloaded = db::get_video(&pool, video_id).await.expect("get_video");
        assert_eq!(reloaded.status, VideoStatus::Pending);
    }

    #[sqlx::test]
    async fn dispatch_download_respects_drain_gate(pool: PgPool) {
        use crate::db::RuntimeSettingsRow;
        use crate::runtime_config::{EnvOverrides, resolve};

        let settings = Arc::new(resolve(
            &RuntimeSettingsRow::default(),
            &EnvOverrides::default(),
        ));
        let (_settings_tx, config_rx) = watch::channel(settings);

        let drain = DrainToken::new();
        drain.begin(Utc::now());

        let supervisor = spawn_test_supervisor(pool.clone(), config_rx, drain).await;
        let (video, profile, source) = seed_pending_video(&pool).await;
        let video_id = video.id;

        supervisor
            .ask(EnqueueDownload {
                video,
                profile,
                source,
            })
            .await
            .expect("EnqueueDownload accepted");

        let status = supervisor
            .ask(GetSupervisorStatus)
            .await
            .expect("GetSupervisorStatus");
        assert_eq!(
            status.dispatching, 0,
            "a draining supervisor must not reserve a dispatch slot"
        );
        assert_eq!(status.active_downloads, 0);

        let reloaded = db::get_video(&pool, video_id).await.expect("get_video");
        assert_eq!(reloaded.status, VideoStatus::Pending);
    }
}
