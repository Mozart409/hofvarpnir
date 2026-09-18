//! The `DownloadSupervisor` is a singleton that manages download concurrency.
//!
//! It holds a `tokio::sync::Semaphore` with a configurable number of permits
//! (default 3) and spawns short-lived `DownloadWorker` actors when permits
//! are available. It also handles retry logic with exponential backoff.
//!
//! # Invariant: a handler reachable via `tell()` must never return `Err`
//!
//! This is not a style preference — it is the single most expensive lesson
//! this file has taught us. Kameo delivers a `tell()` with **no reply
//! channel**, so when a handler returns `Err` there is nowhere to send it.
//! Kameo's response is to escalate: `kameo::message` wraps the error in a
//! `PanicError` with `PanicReason::OnMessage` and routes it through
//! [`Actor::on_panic`], whose documented contract is *"Called when the actor
//! encounters a panic **or an error during 'tell' message handling**"*. The
//! default `on_panic` returns `ControlFlow::Break(ActorStopReason::Panicked)`
//! — **the actor stops**.
//!
//! In production (2026-09-15) a single transient Postgres pool-acquire
//! timeout inside the `ProcessPendingDownloads` handler did exactly this:
//! `db::list_videos_ready_for_download(...).map_err(|e| e.to_string())?`
//! returned `Err`, kameo killed the supervisor, nothing restarted it, and
//! downloads stopped for 27 hours with videos wedged in `pending`. The only
//! trace was one line: `Download supervisor stopping | reason: Panicked {
//! err: "Failed to connect to database: pool timed out ..." }`.
//!
//! So, concretely, for every `impl Message<M> for DownloadSupervisor` whose
//! `type Reply` is a `Result`:
//!
//! - If **any** caller anywhere sends `M` via `tell()` (including
//!   `try_send()` / `mailbox_timeout(..).send()`, which are all `tell`
//!   flavours), the handler must handle its own failures — log, record
//!   state, back off — and return `Ok`. A transient infrastructure fault
//!   must degrade the *sweep*, never the *actor*.
//! - If `M` is `ask()`-only, returning `Err` is legitimate (the caller gets
//!   it back over the reply channel and can surface it), but the handler
//!   must carry a doc comment saying so, because the constraint is invisible
//!   at the definition site — it lives in the call sites of other crates.
//!
//! Current audit (see each handler's doc comment for detail):
//!
//! | Message                    | Reply                 | `tell`-reachable | Treatment |
//! |----------------------------|-----------------------|------------------|-----------|
//! | `ProcessPendingDownloads`  | `Result<usize, String>` | yes (`startup`, `scheduler`) | non-fatal, DB backoff |
//! | `EnqueueDownload`          | `Result<(), String>`  | yes (`source_indexer`, `hof-api`, `hof-web`) | non-fatal, logs + `Ok` |
//! | `CancelDownload`           | `Result<(), String>`  | no (`ask`-only)  | `Err` retained, documented `ask`-only |
//!
//! [`Actor::on_panic`]: kameo::actor::Actor::on_panic

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
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

/// Upper bound on the database-unavailability backoff, in seconds.
///
/// Ten minutes. The backoff exists to stop a `ProcessPendingDownloads` sweep
/// from hammering a pool that is already exhausted (every sweep that waits
/// out `acquire_timeout` and fails holds a waiter slot for the duration), not
/// to give up. The clamp matters because the bound is also the worst-case
/// recovery latency: once Postgres comes back, the scheduler's very next tick
/// past this deadline resumes the sweep, so downloads restart within at most
/// ten minutes of the database becoming healthy — with no operator action and
/// no process restart.
const DB_BACKOFF_MAX_SECS: u64 = 600;

/// Backoff to apply after `consecutive_failures` consecutive failures of the
/// `ProcessPendingDownloads` database query.
///
/// One-based: the *first* failure yields 1s, then 2s, 4s, 8s, ... doubling
/// until clamped at [`DB_BACKOFF_MAX_SECS`]. `consecutive_failures == 0`
/// (never produced by the caller, which increments before calling) is treated
/// as the first failure rather than as "no backoff", so a future caller
/// cannot accidentally disable the backoff by passing a zero.
///
/// Deliberately a free function over a plain `u32` rather than a method on
/// `DownloadSupervisor`: the schedule is the part worth pinning down in a
/// test, and a pure function can be tested without a pool, a runtime, or a
/// spawned actor.
///
/// Overflow-free by construction: the shift is clamped to 63 before it is
/// applied to a `u64`, and `checked_shl` covers the remainder, so a failure
/// count of `u32::MAX` returns the clamp rather than panicking in debug
/// builds or wrapping to a near-zero delay in release builds. That case is
/// not hypothetical hygiene — a wrapped shift producing a 1-nanosecond
/// backoff would restore exactly the tight-loop behaviour this guards.
fn db_backoff(consecutive_failures: u32) -> Duration {
    let shift = consecutive_failures.saturating_sub(1).min(63);
    let secs = 1_u64.checked_shl(shift).unwrap_or(u64::MAX);
    Duration::from_secs(secs.min(DB_BACKOFF_MAX_SECS))
}

/// Project a monotonic `tokio::time::Instant` onto the wall clock.
///
/// Needed only at the API boundary: `db_retry_after` is deliberately
/// monotonic internally (immune to NTP steps and host suspend/resume), but
/// `SupervisorStatus` is serialised for the HTTP API and rendered in the web
/// UI, both of which need a `DateTime<Utc>`.
///
/// Computed as `now + remaining` rather than by any absolute correspondence
/// between the two clocks, because none exists. Every step is fallible-safe:
/// `saturating_duration_since` yields zero for an already-elapsed deadline
/// (reported as "now", which is true — the backoff has expired),
/// `Duration::from_std` cannot overflow for a bounded
/// [`DB_BACKOFF_MAX_SECS`] window, and `checked_add_signed` degrades to
/// "now" instead of panicking on a `DateTime` overflow.
fn instant_to_wall_clock(instant: Instant) -> DateTime<Utc> {
    let remaining = instant.saturating_duration_since(Instant::now());
    chrono::Duration::from_std(remaining)
        .ok()
        .and_then(|d| Utc::now().checked_add_signed(d))
        .unwrap_or_else(Utc::now)
}

/// Classify an [`ActorStopReason`] into a stable, low-cardinality label.
///
/// Exists so the `on_stop` log line can be filtered in Loki without parsing
/// the `Debug` rendering of the reason: `stop_class="fault"` is an alert
/// condition, `stop_class="graceful"` is a deploy. The 2026-09-15 incident
/// was invisible for 27 hours precisely because the only signal was an
/// `info!` line whose `reason` field happened to say `Panicked`.
const fn stop_class(reason: &ActorStopReason) -> &'static str {
    match reason {
        ActorStopReason::Normal => "graceful",
        ActorStopReason::Killed => "killed",
        ActorStopReason::SupervisorRestart => "supervisor_restart",
        ActorStopReason::Panicked(_) => "fault",
        ActorStopReason::LinkDied { .. } => "link_died",
        // `ActorStopReason` grows a `PeerDisconnected` variant under kameo's
        // (non-default) `remote` feature. The catch-all keeps this compiling
        // if that feature is ever switched on; it is unreachable today.
        #[allow(unreachable_patterns)]
        _ => "unknown",
    }
}

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
    /// Whether downloaded files are verified before publishing.
    verify_downloads: bool,
    /// Maximum download attempts before marking as permanently failed.
    max_attempts: u32,
    /// Broadcaster for real-time SSE notifications.
    broadcaster: ActivityBroadcaster,
    /// Process-local drain signal. A second source (alongside the
    /// `downloads_paused` pause gate) for the same "stop dispatching new
    /// work" refusal path — see `dispatch_download` and
    /// `ProcessPendingDownloads`.
    drain: DrainToken,
    /// Consecutive failures of the `ProcessPendingDownloads` database query.
    /// Reset to zero by the first success. Drives `db_backoff`.
    db_failures: u32,
    /// Earliest instant at which the next `ProcessPendingDownloads` sweep may
    /// touch the database again. `None` means "healthy, sweep freely".
    ///
    /// A `tokio::time::Instant` (monotonic) rather than a `DateTime<Utc>`
    /// because it gates a *duration since the last failure*, which must not
    /// be perturbed by a wall-clock step (NTP correction, host suspend/resume
    /// — the latter routine for this workload). The wall-clock projection
    /// needed by the API lives only at the `GetSupervisorStatus` boundary.
    db_retry_after: Option<Instant>,
    /// The most recent database error that forced a sweep to be skipped,
    /// surfaced through `SupervisorStatus` so the UI can say *why* nothing is
    /// downloading instead of silently showing an idle queue.
    last_db_error: Option<String>,
}

impl std::fmt::Debug for DownloadSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadSupervisor")
            .field("active_downloads", &self.active_downloads.len())
            .field("rate_limit_backoff", &self.rate_limit_backoff_multiplier)
            .field("db_failures", &self.db_failures)
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
            verify_downloads: args.config.verify_downloads,
            max_attempts: args.config.max_attempts,
            broadcaster: args.broadcaster,
            drain: args.drain,
            db_failures: 0,
            db_retry_after: None,
            last_db_error: None,
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

    /// Log the death and tear down in-flight workers.
    ///
    /// The log is deliberately split by severity. A graceful stop (deploy,
    /// shutdown, operator `kill`) is `info!`; anything else — a fault, a dead
    /// link — is `error!`, and both carry a `stop_class` field (see
    /// [`stop_class`]) so the two are separable in Loki by label rather than
    /// by substring-matching a `Debug` rendering.
    ///
    /// This asymmetry is the whole point: during the 2026-09-15 incident the
    /// supervisor's death was recorded, correctly, with the reason attached —
    /// but at `info!`, indistinguishable at a glance from the dozens of
    /// benign restart lines around it. Nothing alerted, and nobody read it
    /// for 27 hours. A fault must announce itself as a fault.
    async fn on_stop(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        reason: ActorStopReason,
    ) -> Result<(), Self::Error> {
        let class = stop_class(&reason);
        let graceful = matches!(reason, ActorStopReason::Normal | ActorStopReason::Killed);

        if graceful {
            info!(
                actor = "DownloadSupervisor",
                stop_class = class,
                reason = ?reason,
                active_downloads = self.active_downloads.len(),
                dispatching = self.dispatching.len(),
                "Download supervisor stopping"
            );
        } else {
            error!(
                actor = "DownloadSupervisor",
                stop_class = class,
                reason = ?reason,
                active_downloads = self.active_downloads.len(),
                dispatching = self.dispatching.len(),
                consecutive_db_failures = self.db_failures,
                last_db_error = self.last_db_error.as_deref().unwrap_or("none"),
                "Download supervisor stopping ABNORMALLY; no new downloads will \
                 be dispatched until it is restarted"
            );
        }

        // Stop all active workers
        for (video_id, worker_ref) in self.active_downloads.drain() {
            debug!(video_id = %video_id, "Stopping download worker");
            worker_ref.stop_gracefully().await.ok();
        }
        self.dispatching.clear();

        Ok(())
    }

    /// Log the fault that is about to stop this actor, then stop.
    ///
    /// Kameo routes two distinct things through `on_panic`: a genuine
    /// unwinding panic in a handler (`PanicReason::HandlerPanic`) and an
    /// `Err` returned from a handler invoked by `tell()`
    /// (`PanicReason::OnMessage`) — see the module-level invariant. The
    /// second is the 2026-09-15 bug class, and distinguishing them in the log
    /// is the difference between "we have a real panic to debug" and "a
    /// handler broke the `tell`/`Err` invariant".
    ///
    /// The stop behaviour is *unchanged* from kameo's default
    /// (`ControlFlow::Break`). Restarting is not this hook's job: swallowing
    /// the fault here with `ControlFlow::Continue` would keep a possibly
    /// inconsistent actor alive and hide the fault from the supervision tree,
    /// which owns restart policy. This override exists purely so that the
    /// next fault names itself.
    async fn on_panic(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        err: PanicError,
    ) -> Result<ControlFlow<ActorStopReason>, Self::Error> {
        error!(
            actor = "DownloadSupervisor",
            panic_reason = ?err.reason(),
            is_real_panic = err.is_panic(),
            error = %err,
            error_detail = ?err,
            active_downloads = self.active_downloads.len(),
            dispatching = self.dispatching.len(),
            consecutive_db_failures = self.db_failures,
            last_db_error = self.last_db_error.as_deref().unwrap_or("none"),
            "Download supervisor FAULTED and is stopping; a handler either \
             panicked or returned Err from a tell-delivered message (see the \
             tell/Err invariant in this module's docs)"
        );

        Ok(ControlFlow::Break(ActorStopReason::Panicked(err)))
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
    /// `Result` is retained for source compatibility with the `ask` call
    /// sites in the tests, but this handler is **infallible by
    /// construction** — see the `handle` doc comment.
    type Reply = Result<(), String>;

    /// Accept a video for dispatch.
    ///
    /// # This handler must never return `Err`
    ///
    /// `EnqueueDownload` is the most widely `tell`-delivered message on this
    /// actor — `source_indexer` fires one per newly discovered video, and
    /// `hof-api`/`hof-web` fire them from manual retry/download actions, six
    /// `tell` sites in all and not one `ask` in production code. Per the
    /// module-level invariant, a single `Err` from any of those would be
    /// escalated by kameo to `on_panic` and would stop the supervisor,
    /// killing *all* downloading — from a fault affecting one video.
    ///
    /// `dispatch_download` happens to have no failing path today (every
    /// refusal it makes — paused, draining, ineligible, already dispatching —
    /// is deliberately `Ok`, see its doc comment), so this is currently a
    /// latent rather than live instance of the 2026-09-15 bug. The guard
    /// below is here precisely so it stays latent: it converts any `Err` a
    /// future edit of `dispatch_download` introduces into a logged,
    /// per-video failure instead of an actor death. Do not "simplify" it back
    /// into `self.dispatch_download(..).await`.
    #[instrument(skip_all, fields(video_id = %msg.video.id))]
    async fn handle(
        &mut self,
        msg: EnqueueDownload,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let video_id = msg.video.id;
        let supervisor_ref = ctx.actor_ref().clone();

        if let Err(e) = self.dispatch_download(msg, supervisor_ref).await {
            error!(
                video_id = %video_id,
                error = %e,
                "Failed to dispatch enqueued download; dropping this enqueue \
                 (supervisor stays alive — see the tell/Err invariant)"
            );
        }

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
    /// `Result` is kept for source compatibility with existing callers, but
    /// this handler is **infallible by construction**: every return path is
    /// `Ok`. See the handler body and the module-level `tell`/`Err`
    /// invariant.
    type Reply = Result<usize, String>;

    /// Sweep the `pending`/retry-ready backlog and dispatch what is eligible.
    ///
    /// # This handler must never return `Err`
    ///
    /// `ProcessPendingDownloads` is delivered exclusively by `tell()` — from
    /// `startup::run` (the one-shot catch-up sweep on boot) and from the
    /// scheduler's periodic tick. Per the module-level invariant, an `Err`
    /// from a `tell`-delivered handler is escalated by kameo to `on_panic`
    /// and **stops the supervisor**. On 2026-09-15 a single transient
    /// pool-acquire timeout here did exactly that and downloads stayed dead
    /// for 27 hours. Every failure path below therefore logs, records state,
    /// and returns `Ok`.
    ///
    /// # Degradation strategy
    ///
    /// A database failure is treated as *the sweep is impossible right now*,
    /// never as *this actor is broken*. Failures are counted, and the next
    /// `db_backoff(count)` window is skipped outright without touching the
    /// pool — deliberately, because the failure mode we actually hit is pool
    /// *exhaustion*: each sweep that waits out `acquire_timeout` and fails
    /// occupies a waiter slot for the whole timeout, so retrying at full
    /// scheduler cadence actively prolongs the outage it is reacting to. One
    /// success clears the counter and the window, so recovery needs no
    /// operator action.
    ///
    /// The return value is the number of videos found and processed by a
    /// complete sweep, `0` for a skipped or failed sweep, and the number
    /// dispatched so far for a sweep that aborted partway.
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

        // Database-unavailability backoff gate. Checked before the pause gate
        // would matter and before any pool access, so a sweep inside the
        // backoff window costs nothing at all — no connection acquire, no
        // waiter slot held on an already-exhausted pool.
        if let Some(until) = self.db_retry_after {
            let now = Instant::now();
            if now < until {
                debug!(
                    consecutive_db_failures = self.db_failures,
                    retry_in_secs = until.saturating_duration_since(now).as_secs(),
                    last_db_error = self.last_db_error.as_deref().unwrap_or("none"),
                    "Skipping pending-downloads sweep: database backoff active"
                );
                return Ok(0);
            }
        }

        // Get all videos ready for download.
        let videos = match db::list_videos_ready_for_download(&self.pool).await {
            Ok(videos) => {
                self.note_db_recovered();
                videos
            }
            Err(e) => {
                // THE FIX. This used to be `.map_err(|e| e.to_string())?`,
                // which killed the actor (see this handler's doc comment).
                self.note_db_failure("list_videos_ready_for_download", &e.to_string());
                return Ok(0);
            }
        };

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
        let mut dispatched = 0_usize;
        for video in videos {
            let video_id = video.id;

            // The three lookups below all follow ids that a foreign key
            // already guarantees resolve (`source_ids` comes from the join
            // table, `source.profile_id` from a FK-constrained column), so a
            // failure here means infrastructure, not data — the same pool
            // that just succeeded above has gone away mid-sweep. Aborting the
            // rest of the sweep and entering backoff is therefore right
            // (matching the pre-fix control flow, which `?`-returned here),
            // and is strictly better than continuing to issue N more doomed
            // queries. What changed is only the *reply*: `Ok(dispatched)`
            // instead of an actor-killing `Err`.

            // Get the source(s) for this video to find the profile
            let source_ids = match db::get_sources_for_video(&self.pool, video_id).await {
                Ok(ids) => ids,
                Err(e) => {
                    self.note_db_failure("get_sources_for_video", &e.to_string());
                    return Ok(dispatched);
                }
            };

            let Some(&first_source_id) = source_ids.first() else {
                warn!(video_id = %video_id, "Video has no linked sources, skipping");
                continue;
            };

            // Get the first source's profile (in a real app, might need better logic)
            let source = match db::get_source(&self.pool, first_source_id).await {
                Ok(source) => source,
                Err(e) => {
                    self.note_db_failure("get_source", &e.to_string());
                    return Ok(dispatched);
                }
            };

            let profile = match db::get_profile(&self.pool, source.profile_id).await {
                Ok(profile) => profile,
                Err(e) => {
                    self.note_db_failure("get_profile", &e.to_string());
                    return Ok(dispatched);
                }
            };

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
            } else {
                dispatched = dispatched.saturating_add(1);
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
    /// Wall-clock instant until which the `ProcessPendingDownloads` sweep is
    /// backing off from the database, or `None` when the database is healthy.
    ///
    /// Wall clock, not the monotonic `Instant` the actor stores internally,
    /// because this crosses an API/UI boundary where "in 4 minutes" has to be
    /// rendered against the reader's clock. The projection is computed from
    /// the *remaining* duration at the moment of the read (see
    /// `instant_to_wall_clock`), so it is accurate when read and is not
    /// expected to be stable across reads.
    pub db_backoff_until: Option<DateTime<Utc>>,
    /// Consecutive `ProcessPendingDownloads` database failures; `0` when
    /// healthy. Non-zero with an empty queue is the signal that distinguishes
    /// "nothing to download" from "cannot see what to download" — the
    /// ambiguity that made the 2026-09-15 outage invisible in the UI.
    pub consecutive_db_failures: u32,
    /// Message from the most recent database failure, retained until the next
    /// success so the UI can explain the stall rather than just report it.
    pub last_db_error: Option<String>,
}

impl Message<GetSupervisorStatus> for DownloadSupervisor {
    type Reply = SupervisorStatus;

    async fn handle(
        &mut self,
        _msg: GetSupervisorStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.status_snapshot()
    }
}

/// Cancel an active or pending download.
///
/// # `ask`-only — never send this with `tell`
///
/// Unlike the other two `Result`-replying messages on this actor, this one
/// keeps a meaningful `Err`: all four call sites (`hof-api`'s single and bulk
/// cancel routes, `hof-web`'s two cancel handlers) use `ask` and surface the
/// error to the operator who clicked cancel, so swallowing it would silently
/// report success for a cancellation that did not happen.
///
/// That makes the `tell` prohibition load-bearing rather than advisory: per
/// the module-level invariant, a `tell(CancelDownload { .. })` whose DB write
/// failed would be escalated to `on_panic` and would stop the supervisor,
/// turning one failed cancel into a total downloading outage. If a
/// fire-and-forget cancel is ever genuinely wanted, do not reach for `tell`
/// here — add a separate message whose handler logs and returns `Ok`.
pub struct CancelDownload {
    pub video_id: Ulid,
}

impl Message<CancelDownload> for DownloadSupervisor {
    /// A real, caller-visible `Result`. Legitimate only because this message
    /// is `ask`-only; see [`CancelDownload`].
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
    /// Record a failed database access from the `ProcessPendingDownloads`
    /// sweep, arm the backoff window, and log it.
    ///
    /// `operation` names the `db::` call that failed so the log distinguishes
    /// "could not even list the backlog" from "lost the pool partway through
    /// dispatching it" — different blast radii, same remedy.
    ///
    /// This is the *entire* replacement for what used to be a `?`. It
    /// deliberately returns `()` rather than anything `?`-able, so that no
    /// future edit can accidentally reintroduce an actor-killing early return
    /// through it.
    fn note_db_failure(&mut self, operation: &str, error: &str) {
        self.db_failures = self.db_failures.saturating_add(1);
        let backoff = db_backoff(self.db_failures);
        // `checked_add` rather than `+`: `Instant` addition can overflow the
        // underlying representation, and a panic here would be the very
        // failure mode this function exists to prevent. Falling back to the
        // un-armed state just means the next sweep retries immediately.
        self.db_retry_after = Instant::now().checked_add(backoff);
        self.last_db_error = Some(error.to_owned());

        warn!(
            operation,
            error,
            consecutive_db_failures = self.db_failures,
            retry_in_secs = backoff.as_secs(),
            "Pending-downloads sweep failed on a database error; backing off \
             (supervisor stays alive — see the tell/Err invariant)"
        );
    }

    /// Clear the database backoff state after a successful access.
    ///
    /// Logs at `info!` only on an actual recovery (i.e. when there was
    /// something to clear), so the healthy path stays silent while the
    /// outage-ended transition is recorded exactly once.
    fn note_db_recovered(&mut self) {
        if self.db_failures > 0 {
            info!(
                previous_consecutive_failures = self.db_failures,
                last_db_error = self.last_db_error.as_deref().unwrap_or("none"),
                "Database recovered; resuming pending-downloads sweeps"
            );
        }
        self.db_failures = 0;
        self.db_retry_after = None;
        self.last_db_error = None;
    }

    /// Project the supervisor's current state into the status shape the API
    /// and web panel read.
    ///
    /// Deliberately a plain method on `&self` rather than inline in the
    /// `GetSupervisorStatus` handler: a `Context` cannot be constructed
    /// outside kameo, so a handler-only projection is unreachable from a unit
    /// test. Keeping it here lets tests assert on exactly what the UI will
    /// render without spawning an actor.
    fn status_snapshot(&self) -> SupervisorStatus {
        SupervisorStatus {
            active_downloads: self.active_downloads.len(),
            dispatching: self.dispatching.len(),
            available_permits: self.semaphore.available_permits(),
            rate_limit_backoff: self.rate_limit_backoff_multiplier,
            db_backoff_until: self.db_retry_after.map(instant_to_wall_clock),
            consecutive_db_failures: self.db_failures,
            last_db_error: self.last_db_error.clone(),
        }
    }

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
        let verify_downloads = self.verify_downloads;
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
                verify_downloads,
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

    // ========================================================================
    // Database-unavailability backoff (the 2026-09-15 incident).
    // ========================================================================

    #[test]
    fn db_backoff_doubles_then_clamps() {
        // One-based: the first failure already backs off, and it backs off by
        // the smallest useful amount rather than waiting out a long window
        // for what is usually a one-off blip.
        assert_eq!(db_backoff(1), Duration::from_secs(1));
        assert_eq!(db_backoff(2), Duration::from_secs(2));
        assert_eq!(db_backoff(3), Duration::from_secs(4));
        assert_eq!(db_backoff(4), Duration::from_secs(8));
        assert_eq!(db_backoff(5), Duration::from_secs(16));
        assert_eq!(db_backoff(6), Duration::from_secs(32));

        // 2^9 = 512 < 600 is the last uncapped step; 2^10 = 1024 clamps.
        assert_eq!(db_backoff(10), Duration::from_secs(512));
        assert_eq!(db_backoff(11), Duration::from_secs(DB_BACKOFF_MAX_SECS));
        assert_eq!(db_backoff(12), Duration::from_secs(DB_BACKOFF_MAX_SECS));

        // Zero is never produced by `note_db_failure` (which increments
        // first), but must not read as "no backoff" if some future caller
        // passes it.
        assert_eq!(db_backoff(0), Duration::from_secs(1));
    }

    #[test]
    fn db_backoff_does_not_overflow_on_absurd_failure_counts() {
        // A naive `1u64 << (n - 1)` panics in debug and wraps in release well
        // before these counts. Wrapping is the dangerous outcome: a
        // near-zero backoff silently restores the tight retry loop against an
        // already-exhausted pool.
        for n in [63_u32, 64, 65, 1_000, u32::MAX - 1, u32::MAX] {
            assert_eq!(
                db_backoff(n),
                Duration::from_secs(DB_BACKOFF_MAX_SECS),
                "db_backoff({n}) must clamp, not overflow"
            );
        }
    }

    #[test]
    fn stop_class_separates_faults_from_graceful_stops() {
        assert_eq!(stop_class(&ActorStopReason::Normal), "graceful");
        assert_eq!(stop_class(&ActorStopReason::Killed), "killed");
        assert_eq!(
            stop_class(&ActorStopReason::SupervisorRestart),
            "supervisor_restart"
        );
        assert_eq!(
            stop_class(&ActorStopReason::Panicked(PanicError::new(
                Box::new("pool timed out".to_owned()),
                PanicReason::OnMessage,
            ))),
            "fault"
        );
    }

    #[test]
    fn instant_to_wall_clock_projects_forwards_and_saturates() {
        let before = Utc::now();
        let projected = instant_to_wall_clock(
            Instant::now()
                .checked_add(Duration::from_mins(2))
                .expect("Instant::now() + 120s is representable"),
        );
        // ~2 minutes out, with generous slack for a slow CI scheduler.
        let delta = projected.signed_duration_since(before).num_seconds();
        assert!(
            (115..=125).contains(&delta),
            "expected ~120s in the future, got {delta}s"
        );

        // An already-expired deadline reports "now", not a past timestamp
        // from an underflowed subtraction.
        let expired = instant_to_wall_clock(
            Instant::now()
                .checked_sub(Duration::from_mins(1))
                .expect("Instant::now() - 60s is representable in a live runtime"),
        );
        assert!(expired >= before);
        assert!(expired <= Utc::now());
    }

    /// A `PgPool` that is guaranteed to fail every query, with no live
    /// database anywhere in sight.
    ///
    /// `connect_lazy` builds the pool without dialing, and `close()` then
    /// makes every subsequent `acquire()` fail immediately with
    /// `PoolClosed`. That is the same *shape* of failure as the production
    /// `PoolTimedOut` — a `DbError` out of the `db::` layer, before any SQL
    /// runs — but it is instantaneous and deterministic, which is why these
    /// tests need neither a database nor an `#[ignore]`.
    async fn unusable_pool() -> PgPool {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(1))
            .connect_lazy("postgres://hof-test-unreachable/hof")
            .expect("a syntactically valid URL builds a lazy pool without connecting");
        pool.close().await;
        pool
    }

    /// Default (nothing paused) runtime settings channel.
    ///
    /// Returns the sender too: the caller must keep it bound for the
    /// supervisor's lifetime, or the resize watcher spawned in `on_start`
    /// sees a closed channel and exits. Matches the
    /// `let (_settings_tx, config_rx) = ...` shape of the tests below.
    fn default_settings_channel() -> (
        watch::Sender<Arc<EffectiveSettings>>,
        watch::Receiver<Arc<EffectiveSettings>>,
    ) {
        use crate::db::RuntimeSettingsRow;
        use crate::runtime_config::{EnvOverrides, resolve};

        watch::channel(Arc::new(resolve(
            &RuntimeSettingsRow::default(),
            &EnvOverrides::default(),
        )))
    }

    /// Build (but do not spawn) a `DownloadSupervisor` for tests that need to
    /// drive its private bookkeeping directly.
    ///
    /// Spawning would only let us reach that bookkeeping through a message,
    /// and there is no message that *injects* failure state — so the
    /// recovery transition would be untestable against the real methods.
    async fn unspawned_supervisor(
        pool: PgPool,
        config_rx: watch::Receiver<Arc<EffectiveSettings>>,
    ) -> DownloadSupervisor {
        let ytdlp = Arc::new(
            YtdlpClient::new("yt-dlp", None, std::path::Path::new("/tmp"))
                .await
                .expect("YtdlpClient::new is path-only construction and cannot fail here"),
        );
        let (progress_tx, _progress_rx) = mpsc::channel(10);

        DownloadSupervisor {
            pool,
            ytdlp,
            semaphore: Arc::new(Semaphore::new(2)),
            permits_total: 2,
            config_rx,
            last_download_start: None,
            rate_limit_backoff_multiplier: 1,
            active_downloads: HashMap::new(),
            dispatching: HashSet::new(),
            progress_tx,
            download_timeout: Duration::from_hours(1),
            verify_downloads: false,
            max_attempts: 3,
            broadcaster: ActivityBroadcaster::new(),
            drain: DrainToken::new(),
            db_failures: 0,
            db_retry_after: None,
            last_db_error: None,
        }
    }

    /// REGRESSION TEST for the 2026-09-15 27-hour download outage.
    ///
    /// A transient `db::` failure inside `ProcessPendingDownloads` used to be
    /// returned as `Err`. Because both callers deliver that message with
    /// `tell()`, kameo escalated the `Err` to `on_panic` and stopped the
    /// supervisor; nothing restarted it and downloads stayed dead until the
    /// process was restarted by hand.
    ///
    /// The two assertions below are the whole incident:
    ///   (a) the sweep degrades to `Ok(0)` instead of `Err`, and
    ///   (b) the actor is still alive to try again on the next tick.
    ///
    /// This test fails on the pre-fix code: `ask` surfaces the `Err` and (a)
    /// trips. Note that it is sent with `ask`, not `tell`, purely so the
    /// reply is observable — the failure mode under test is a property of the
    /// *handler's return value*, and `tell` would give us nothing to assert
    /// on (b) beyond a race against the actor's own death.
    #[tokio::test]
    async fn process_pending_downloads_survives_database_failure() {
        let (_settings_tx, config_rx) = default_settings_channel();
        let supervisor =
            spawn_test_supervisor(unusable_pool().await, config_rx, DrainToken::new()).await;

        let processed = supervisor
            .ask(ProcessPendingDownloads)
            .await
            .expect("a database failure must NOT be reported as an Err reply");
        assert_eq!(processed, 0, "a failed sweep dispatches nothing");

        assert!(
            supervisor.is_alive(),
            "THE BUG: a transient database error must not kill the download \
             supervisor — this is the 27-hour outage of 2026-09-15"
        );

        // Still alive means still serving messages, not merely un-reaped.
        let status = supervisor
            .ask(GetSupervisorStatus)
            .await
            .expect("a live supervisor still answers GetSupervisorStatus");
        assert_eq!(status.consecutive_db_failures, 1);
        assert!(status.db_backoff_until.is_some());
        assert!(
            status.last_db_error.is_some_and(|e| !e.is_empty()),
            "the UI needs the reason the queue is stalled, not just that it is"
        );
    }

    /// Repeated failures accumulate, and the backoff window suppresses the
    /// sweeps in between — each of them still non-fatally.
    #[tokio::test]
    async fn repeated_database_failures_back_off_without_dying() {
        let (_settings_tx, config_rx) = default_settings_channel();
        let supervisor =
            spawn_test_supervisor(unusable_pool().await, config_rx, DrainToken::new()).await;

        for _ in 0..5_u32 {
            assert_eq!(
                supervisor
                    .ask(ProcessPendingDownloads)
                    .await
                    .expect("every sweep replies Ok, however many have failed"),
                0
            );
            assert!(supervisor.is_alive());
        }

        let status = supervisor
            .ask(GetSupervisorStatus)
            .await
            .expect("GetSupervisorStatus");
        // Only the first sweep reached the pool; the remaining four were
        // skipped by the 1s backoff armed by that first failure, which is the
        // behaviour that stops a failing sweep from monopolising pool waiter
        // slots. So the counter is 1, not 5.
        assert_eq!(
            status.consecutive_db_failures, 1,
            "sweeps inside the backoff window must not touch the pool at all"
        );
        assert!(status.db_backoff_until.is_some());
    }

    /// A success after failures clears the counter and the backoff window,
    /// so recovery needs no operator action and no process restart.
    ///
    /// Drives the real `note_db_failure`/`note_db_recovered` on a real (if
    /// unspawned) `DownloadSupervisor`, rather than a pool made to fail and
    /// then succeed: a live pool cannot be flipped from broken to healthy
    /// in-process, and the state transition is the contract under test.
    #[tokio::test]
    async fn db_success_resets_failure_state() {
        let (_settings_tx, config_rx) = default_settings_channel();
        let mut supervisor = unspawned_supervisor(unusable_pool().await, config_rx).await;

        supervisor.note_db_failure("list_videos_ready_for_download", "pool timed out");
        assert_eq!(supervisor.db_failures, 1);
        supervisor.note_db_failure("list_videos_ready_for_download", "pool timed out");
        supervisor.note_db_failure("get_source", "pool timed out");
        assert_eq!(supervisor.db_failures, 3);
        assert!(supervisor.db_retry_after.is_some());
        assert_eq!(supervisor.last_db_error.as_deref(), Some("pool timed out"));

        supervisor.note_db_recovered();
        assert_eq!(
            supervisor.db_failures, 0,
            "one success must reset the schedule to 1s, not resume mid-ramp"
        );
        assert!(
            supervisor.db_retry_after.is_none(),
            "a stale backoff deadline would keep suppressing sweeps after recovery"
        );
        assert!(supervisor.last_db_error.is_none());

        // And the status projection agrees, since that is what the UI reads.
        let status = supervisor.status_snapshot();
        assert_eq!(status.consecutive_db_failures, 0);
        assert!(status.db_backoff_until.is_none());
        assert!(status.last_db_error.is_none());
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
            verify_downloads: false,
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
        drain.begin(Utc::now(), Duration::from_mins(30));

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
