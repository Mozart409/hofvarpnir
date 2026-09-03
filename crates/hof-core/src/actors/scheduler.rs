//! The `SchedulerActor` is a singleton that fires indexing jobs on a per-source
//! schedule using `tokio::time`. On each tick it messages the appropriate
//! `SourceIndexerActor` to begin indexing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use kameo::Reply;
use kameo::prelude::*;
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior, interval};
use tracing::{debug, error, info, instrument, warn};
use ulid::Ulid;

use crate::db;
use crate::db::ActivityBroadcaster;
use crate::domain::activity::{ActivityEventType, ActivitySeverity};
use crate::domain::source::Source;
use crate::runtime_config::{DrainToken, EffectiveSettings, indefinite_pause};
use crate::ytdlp::YtdlpClient;

use super::download_supervisor::{DownloadSupervisor, ProcessPendingDownloads};
use super::source_indexer::{IndexingResult, SourceIndexerActor, SourceIndexerArgs};

/// Minimum interval between indexing the same source (rate limiting).
/// Floor on how often a single source may be re-indexed.
///
/// Applies regardless of the source's configured `index_frequency_secs`.
/// Public so the runtime control panel can display it: it is one of the
/// read-only timings an operator is actually subject to (design 7.1).
pub const MIN_INDEX_INTERVAL_SECS: u64 = 300; // 5 minutes

/// How long the scheduling loop waits for mailbox space before giving up on
/// a single tick. Bounded so the loop can never stall indefinitely, but long
/// enough to ride out a transient mailbox-full burst (e.g. a wave of
/// `IndexingCompleted` replies from a large indexing backlog) without
/// dropping the tick outright.
const TICK_SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Default cap on how many new indexers a single `CheckSources` tick will
/// spawn. Without this, a large accumulated backlog (e.g. after downtime)
/// spawns every due source's indexer concurrently in one tick — dozens or
/// hundreds of simultaneous yt-dlp processes hitting the platform at once,
/// which is exactly what triggered the mass-timeout incident this file's
/// mailbox fix addresses. Staggering across ticks (one batch every
/// `check_interval`) ramps up gradually instead.
///
/// Also serves as the fallback when the resolved `max_indexers_per_tick`
/// (a `u32` from `EffectiveSettings`) fails to convert to `usize` — which
/// cannot happen on any real platform, but the conversion must still fall
/// back to a small bounded value here, never `usize::MAX`, since an
/// unbounded cap is exactly the runaway-indexer load this feature exists to
/// prevent. Mirrors `runtime_config::DEFAULT_MAX_INDEXERS_PER_TICK`.
const DEFAULT_MAX_INDEXERS_PER_TICK: usize = 5;

/// Exact message returned by [`IndexSource`]'s refusal when the system is
/// draining for shutdown (see `SchedulerActor::indexing_refusal_for`).
///
/// `hof-api`'s `routes::sources::trigger_index` maps this refusal to an HTTP
/// status by matching against this constant rather than duplicating the
/// literal string, so the producer here and the matcher there cannot drift
/// apart.
pub const DRAINING_REFUSAL_MESSAGE: &str =
    "Indexing is unavailable: the system is draining for shutdown";

/// Shared prefix of every "indexing is paused" refusal message returned by
/// [`IndexSource`] ("Indexing is paused", "...indefinitely", "...until
/// {timestamp}"; see `SchedulerActor::indexing_refusal_for`).
///
/// `hof-api`'s `routes::sources::trigger_index` matches refusal messages
/// against this constant with `starts_with` rather than duplicating the
/// literal string.
pub const PAUSED_REFUSAL_PREFIX: &str = "Indexing is paused";

/// The scheduler actor.
///
/// Manages periodic checks for sources that need indexing and spawns
/// `SourceIndexerActor` instances to handle the actual indexing.
pub struct SchedulerActor {
    /// Database pool.
    pool: PgPool,
    /// yt-dlp client.
    ytdlp: Arc<YtdlpClient>,
    /// Reference to the download supervisor.
    supervisor: ActorRef<DownloadSupervisor>,
    /// Live runtime settings (check interval, indexer cap, ...).
    config_rx: watch::Receiver<Arc<EffectiveSettings>>,
    /// Track when each source was last indexed (for rate limiting).
    last_indexed: HashMap<Ulid, Instant>,
    /// Active indexing tasks (`source_id` -> actor ref).
    active_indexers: HashMap<Ulid, ActorRef<SourceIndexerActor>>,
    /// Whether the scheduler is running.
    running: bool,
    /// Broadcaster for real-time SSE notifications.
    broadcaster: ActivityBroadcaster,
    /// Process-local drain signal. A second source (alongside the
    /// `indexing_paused` pause gate) for the same "stop starting new work"
    /// refusal path — see `CheckSources`.
    drain: DrainToken,
}

impl std::fmt::Debug for SchedulerActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchedulerActor")
            .field("running", &self.running)
            .field("active_indexers", &self.active_indexers.len())
            .finish_non_exhaustive()
    }
}

/// Arguments for spawning the scheduler.
pub struct SchedulerArgs {
    pub pool: PgPool,
    pub ytdlp: Arc<YtdlpClient>,
    pub supervisor: ActorRef<DownloadSupervisor>,
    /// Live runtime settings, shared across all actors that consume
    /// pacing/concurrency knobs.
    pub config_rx: watch::Receiver<Arc<EffectiveSettings>>,
    pub broadcaster: ActivityBroadcaster,
    /// Process-local drain signal, threaded in from `ActorSystem`.
    pub drain: DrainToken,
}

impl Actor for SchedulerActor {
    type Args = SchedulerArgs;
    type Error = color_eyre::eyre::Error;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let (check_interval_secs, max_indexers_per_tick) = {
            let settings = args.config_rx.borrow();
            (
                settings.check_interval.value.as_secs(),
                settings.max_indexers_per_tick.value,
            )
        };

        info!(
            check_interval_secs,
            max_indexers_per_tick, "Scheduler actor starting"
        );

        let scheduler = Self {
            pool: args.pool,
            ytdlp: args.ytdlp,
            supervisor: args.supervisor,
            config_rx: args.config_rx,
            last_indexed: HashMap::new(),
            active_indexers: HashMap::new(),
            running: false,
            broadcaster: args.broadcaster,
            drain: args.drain,
        };

        // Start the scheduling loop.
        // Use try_send() to avoid potential deadlock from self-tell with bounded mailbox.
        if let Err(e) = actor_ref.tell(StartScheduler).try_send() {
            error!(error = %e, "Failed to start scheduler loop");
            return Err(e.into());
        }

        Ok(scheduler)
    }

    async fn on_stop(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        reason: ActorStopReason,
    ) -> Result<(), Self::Error> {
        info!(
            reason = ?reason,
            active_indexers = self.active_indexers.len(),
            "Scheduler actor stopping"
        );

        self.running = false;

        // Stop all active indexers
        for (source_id, indexer_ref) in self.active_indexers.drain() {
            debug!(source_id = %source_id, "Stopping indexer");
            indexer_ref.stop_gracefully().await.ok();
        }

        Ok(())
    }
}

/// Message to start the scheduler loop.
pub struct StartScheduler;

impl Message<StartScheduler> for SchedulerActor {
    type Reply = ();

    async fn handle(&mut self, _msg: StartScheduler, ctx: &mut Context<Self, Self::Reply>) {
        if self.running {
            debug!("Scheduler already running");
            return;
        }

        self.running = true;
        info!("Starting scheduler loop");

        let actor_ref = ctx.actor_ref().clone();
        let mut config_rx = self.config_rx.clone();
        let mut current_interval = config_rx.borrow().check_interval.value;

        // Spawn the scheduling loop
        tokio::spawn(async move {
            let mut ticker = interval(current_interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            // Once the `RuntimeConfig` sender is dropped, `changed()` resolves
            // immediately (as `Err`) forever. Left unguarded, a `select!`
            // branch on it would never again suspend on the ticker branch
            // reliably re-arming, so once closed we stop selecting on it.
            let mut config_alive = true;

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        // Check if we should continue
                        if !actor_ref.is_alive() {
                            break;
                        }

                        // Trigger a check. A single try_send() failure here used to
                        // permanently kill this ticker on any transient mailbox-full
                        // condition (e.g. a burst of IndexingCompleted replies from a
                        // large indexing backlog) — the scheduler would then silently
                        // stop indexing forever while still reporting `running: true`.
                        // Wait (bounded) for mailbox space instead: a full mailbox
                        // just skips this tick and retries on the next one; only a
                        // truly stopped actor ends the loop.
                        match actor_ref
                            .tell(CheckSources)
                            .mailbox_timeout(TICK_SEND_TIMEOUT)
                            .send()
                            .await
                        {
                            Ok(()) => {}
                            Err(SendError::Timeout(_)) => {
                                warn!(
                                    timeout_secs = TICK_SEND_TIMEOUT.as_secs(),
                                    "Scheduler mailbox still full after wait, skipping this tick"
                                );
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to send CheckSources, actor has stopped");
                                break;
                            }
                        }
                    }
                    changed = config_rx.changed(), if config_alive => {
                        if changed.is_err() {
                            debug!(
                                "Runtime config channel closed; scheduler keeps its last known interval"
                            );
                            config_alive = false;
                        } else {
                            let next = config_rx.borrow_and_update().check_interval.value;
                            if next != current_interval {
                                current_interval = next;
                                ticker = interval(current_interval);
                                ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
                                info!(
                                    interval_secs = current_interval.as_secs(),
                                    "Scheduler check interval updated"
                                );
                            }
                        }
                    }
                }
            }

            debug!("Scheduler loop ended");
        });
    }
}

/// Message to stop the scheduler loop.
pub struct StopScheduler;

impl Message<StopScheduler> for SchedulerActor {
    type Reply = ();

    async fn handle(&mut self, _msg: StopScheduler, _ctx: &mut Context<Self, Self::Reply>) {
        info!("Stopping scheduler");
        self.running = false;
    }
}

/// Internal message to check for sources that need indexing.
struct CheckSources;

impl Message<CheckSources> for SchedulerActor {
    type Reply = ();

    #[instrument(skip_all)]
    async fn handle(&mut self, _msg: CheckSources, ctx: &mut Context<Self, Self::Reply>) {
        if !self.running {
            return;
        }

        if let Some(reason) = self.indexing_refusal(Utc::now()) {
            debug!(reason = %reason, "Skipping this tick");
        } else {
            self.spawn_due_indexers(ctx.actor_ref()).await;
        }

        // Always sweep pending/retry-ready downloads every scheduler tick,
        // even when no sources are due for indexing, indexing errored, or
        // indexing is paused — the indexing and downloads pause switches are
        // independent, so a paused indexer must not also stall the
        // downloads-supervisor sweep. `ProcessPendingDownloads` applies its
        // own `downloads_paused` gate.
        match self.supervisor.tell(ProcessPendingDownloads).await {
            Ok(()) => {
                debug!("Triggered pending download processing from scheduler tick");
            }
            Err(error) => {
                error!(%error, "Failed to contact supervisor for pending sweep");
            }
        }
    }
}

/// Message to manually trigger indexing of a specific source.
#[derive(Debug, Clone)]
pub struct IndexSource {
    pub source_id: Ulid,
}

impl Message<IndexSource> for SchedulerActor {
    type Reply = Result<(), String>;

    #[instrument(skip_all, fields(source_id = %msg.source_id))]
    async fn handle(
        &mut self,
        msg: IndexSource,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Check if already being indexed. This is a more specific answer than
        // a pause or drain refusal, and it must be checked first: an index
        // already in flight for this source shouldn't be reported as
        // "paused" or "draining" when it is neither — it is already running.
        if self.active_indexers.contains_key(&msg.source_id) {
            return Err("Source is already being indexed".to_string());
        }

        // Refuse before any side effects. In particular, this must run
        // before `reset_source_indexing_errors` below: that call mutates the
        // source's row, and a request refused here must not have quietly
        // reset the source's error count anyway.
        if let Some(reason) = self.indexing_refusal(Utc::now()) {
            return Err(reason);
        }

        // Reset error count for manual indexing (gives fresh retry attempts)
        db::reset_source_indexing_errors(&self.pool, msg.source_id)
            .await
            .map_err(|e| e.to_string())?;

        // Get the source
        let source = db::get_source(&self.pool, msg.source_id)
            .await
            .map_err(|e| e.to_string())?;

        // Spawn indexer
        self.spawn_indexer(&source, ctx.actor_ref().clone())
            .await
            .map_err(|e| e.to_string())
    }
}

/// Message to get scheduler status.
pub struct GetSchedulerStatus;

/// Status information for the scheduler.
#[derive(Debug, Clone, Reply)]
pub struct SchedulerStatus {
    pub running: bool,
    pub active_indexers: usize,
    pub check_interval_secs: u64,
}

impl Message<GetSchedulerStatus> for SchedulerActor {
    type Reply = SchedulerStatus;

    async fn handle(
        &mut self,
        _msg: GetSchedulerStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        SchedulerStatus {
            running: self.running,
            active_indexers: self.active_indexers.len(),
            check_interval_secs: self.config_rx.borrow().check_interval.value.as_secs(),
        }
    }
}

/// Internal message: indexing completed for a source.
struct IndexingCompleted {
    source_id: Ulid,
    result: IndexingResult,
}

impl Message<IndexingCompleted> for SchedulerActor {
    type Reply = ();

    #[instrument(skip_all, fields(source_id = %msg.source_id))]
    async fn handle(&mut self, msg: IndexingCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        self.active_indexers.remove(&msg.source_id);
        self.last_indexed.insert(msg.source_id, Instant::now());

        info!(
            source_id = %msg.source_id,
            new = msg.result.new_videos,
            existing = msg.result.existing_videos,
            filtered = msg.result.filtered_out,
            filtered_before_cutoff = msg.result.filtered_before_cutoff,
            filtered_shorts = msg.result.filtered_shorts,
            filtered_livestreams = msg.result.filtered_livestreams,
            filtered_unavailable = msg.result.filtered_unavailable,
            filtered_private = msg.result.filtered_private,
            filtered_other = msg.result.filtered_other,
            errors = msg.result.errors.len(),
            "Indexing completed"
        );

        // Emit activity event
        if msg.result.errors.is_empty() {
            let message = format!(
                "Indexed successfully — {} new, {} existing, {} filtered ({})",
                msg.result.new_videos,
                msg.result.existing_videos,
                msg.result.filtered_out,
                msg.result.filtered_summary()
            );
            self.broadcaster
                .log_and_broadcast(
                    &self.pool,
                    ActivityEventType::SourceIndexed,
                    ActivitySeverity::Success,
                    &message,
                    Some(msg.source_id),
                    None,
                    None,
                )
                .await;
        } else {
            for err in &msg.result.errors {
                warn!(source_id = %msg.source_id, error = %err, "Indexing error");
            }
            let message = format!(
                "Indexing had {} error(s): {}",
                msg.result.errors.len(),
                msg.result.errors.first().unwrap_or(&String::new())
            );
            self.broadcaster
                .log_and_broadcast(
                    &self.pool,
                    ActivityEventType::SourceError,
                    ActivitySeverity::Error,
                    &message,
                    Some(msg.source_id),
                    None,
                    None,
                )
                .await;
        }
    }
}

impl SchedulerActor {
    /// Why new indexing work must not start right now, if it must not.
    ///
    /// Both the periodic `CheckSources` sweep and the manual `IndexSource`
    /// trigger consult this; a manual index that skipped it would keep
    /// spawning indexers during a drain and hold up quiescence.
    fn indexing_refusal(&self, now: DateTime<Utc>) -> Option<String> {
        let settings = self.config_rx.borrow();
        let settings: &EffectiveSettings = &settings;
        Self::indexing_refusal_for(settings, self.drain.is_draining(), now)
    }

    /// Pure decision behind [`Self::indexing_refusal`], split out so it can
    /// be unit-tested directly against constructed `EffectiveSettings` and a
    /// `draining` flag, without standing up a full `SchedulerActor` (which
    /// would need a live `PgPool`, a spawned download supervisor, etc.).
    ///
    /// Drain takes priority when both a drain and a pause are in effect:
    /// once the process is draining for shutdown it is going away regardless
    /// of when the pause would have lifted, so the drain is the more useful
    /// (and more urgent) thing to tell the operator.
    fn indexing_refusal_for(
        settings: &EffectiveSettings,
        draining: bool,
        now: DateTime<Utc>,
    ) -> Option<String> {
        if draining {
            return Some(DRAINING_REFUSAL_MESSAGE.to_string());
        }

        if settings.indexing_paused(now) {
            return Some(match settings.indexing_paused_until {
                Some(until) if until == indefinite_pause() => {
                    format!("{PAUSED_REFUSAL_PREFIX} indefinitely")
                }
                Some(until) => format!("{PAUSED_REFUSAL_PREFIX} until {until}"),
                None => PAUSED_REFUSAL_PREFIX.to_string(),
            });
        }

        None
    }

    /// Find sources due for indexing and spawn indexers for them, staggered
    /// across ticks by `max_indexers_per_tick`.
    ///
    /// Only called from `CheckSources` when indexing is not paused. Every
    /// exit path here (no sources due, a DB error, or the per-tick cap) is a
    /// plain early return from *this* method — it must never early-return
    /// out of the caller's `handle`, since the caller still needs to run the
    /// downloads-supervisor sweep afterward regardless of what happens here.
    async fn spawn_due_indexers(&mut self, scheduler_ref: &ActorRef<Self>) {
        debug!("Checking for sources due for indexing");

        let max_indexers_per_tick =
            usize::try_from(self.config_rx.borrow().max_indexers_per_tick.value)
                .unwrap_or(DEFAULT_MAX_INDEXERS_PER_TICK);

        // Get sources that are due for indexing
        let sources = match db::list_sources_due_for_indexing(&self.pool).await {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "Failed to list sources due for indexing");
                return;
            }
        };

        if sources.is_empty() {
            debug!("No sources due for indexing");
            return;
        }

        info!(count = sources.len(), "Found sources due for indexing");

        let total_due = sources.len();
        let mut spawned_this_tick = 0usize;

        for source in sources {
            // Stagger a large backlog across ticks instead of spawning
            // every due source's indexer at once — a burst of dozens of
            // concurrent yt-dlp processes is what caused every source to
            // time out simultaneously after an outage.
            if spawned_this_tick >= max_indexers_per_tick {
                info!(
                    spawned_this_tick,
                    remaining = total_due.saturating_sub(spawned_this_tick),
                    max_indexers_per_tick,
                    "Reached per-tick indexer cap, remaining due sources deferred to next tick"
                );
                break;
            }

            // Skip if already being indexed
            if self.active_indexers.contains_key(&source.id) {
                debug!(source_id = %source.id, "Source already being indexed");
                continue;
            }

            // Rate limit: don't index too frequently
            if let Some(last) = self.last_indexed.get(&source.id)
                && last.elapsed() < Duration::from_secs(MIN_INDEX_INTERVAL_SECS)
            {
                debug!(
                    source_id = %source.id,
                    elapsed_secs = last.elapsed().as_secs(),
                    "Source indexed too recently, skipping"
                );
                continue;
            }

            // Spawn indexer for this source
            if let Err(e) = self.spawn_indexer(&source, scheduler_ref.clone()).await {
                error!(
                    source_id = %source.id,
                    error = %e,
                    "Failed to spawn indexer"
                );
            }
            spawned_this_tick += 1;
        }
    }

    /// Spawn an indexer for a source.
    #[instrument(skip(self, scheduler_ref), fields(source_id = %source.id))]
    async fn spawn_indexer(
        &mut self,
        source: &Source,
        scheduler_ref: ActorRef<Self>,
    ) -> color_eyre::Result<()> {
        // Get the profile for this source
        let profile = db::get_profile(&self.pool, source.profile_id).await?;

        info!(
            source_id = %source.id,
            url = %source.url,
            "Spawning source indexer"
        );

        // Create oneshot channel to receive the indexing result
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        let args = SourceIndexerArgs {
            pool: self.pool.clone(),
            ytdlp: self.ytdlp.clone(),
            source: source.clone(),
            profile,
            supervisor: self.supervisor.clone(),
            result_tx,
        };

        let indexer_ref = SourceIndexerActor::spawn(args);

        // Track the active indexer
        self.active_indexers.insert(source.id, indexer_ref);

        // Spawn a task to wait for the result and notify the scheduler
        let source_id = source.id;
        tokio::spawn(async move {
            // Wait for the indexing result from the oneshot channel
            let result = if let Ok(result) = result_rx.await {
                result
            } else {
                // Channel was dropped without sending - actor probably panicked
                warn!(source_id = %source_id, "Indexer result channel closed unexpectedly");
                IndexingResult {
                    source_id,
                    new_videos: 0,
                    existing_videos: 0,
                    filtered_out: 0,
                    filtered_before_cutoff: 0,
                    filtered_shorts: 0,
                    filtered_livestreams: 0,
                    filtered_unavailable: 0,
                    filtered_private: 0,
                    filtered_other: 0,
                    errors: vec!["Indexer terminated unexpectedly".to_string()],
                }
            };

            let _ = scheduler_ref
                .tell(IndexingCompleted { source_id, result })
                .await;
        });

        Ok(())
    }
}

/// Message to add a new source to be tracked.
#[derive(Debug, Clone)]
pub struct AddSource {
    pub source: Source,
}

impl Message<AddSource> for SchedulerActor {
    type Reply = ();

    #[instrument(skip_all, fields(source_id = %msg.source.id))]
    async fn handle(&mut self, msg: AddSource, ctx: &mut Context<Self, Self::Reply>) {
        info!(source_id = %msg.source.id, url = %msg.source.url, "Source added to scheduler");

        // Immediately trigger indexing for new sources
        if let Err(e) = self
            .spawn_indexer(&msg.source, ctx.actor_ref().clone())
            .await
        {
            error!(
                source_id = %msg.source.id,
                error = %e,
                "Failed to spawn initial indexer for new source"
            );
        }
    }
}

/// Message to remove a source from tracking.
#[derive(Debug, Clone)]
pub struct RemoveSource {
    pub source_id: Ulid,
}

impl Message<RemoveSource> for SchedulerActor {
    type Reply = ();

    #[instrument(skip_all, fields(source_id = %msg.source_id))]
    async fn handle(&mut self, msg: RemoveSource, _ctx: &mut Context<Self, Self::Reply>) {
        info!(source_id = %msg.source_id, "Source removed from scheduler");

        // Stop any active indexer for this source
        if let Some(indexer_ref) = self.active_indexers.remove(&msg.source_id) {
            indexer_ref.stop_gracefully().await.ok();
        }

        // Remove from tracking
        self.last_indexed.remove(&msg.source_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_min_index_interval() {
        // Ensure minimum interval is reasonable
        const _: () = assert!(MIN_INDEX_INTERVAL_SECS >= 60);
        const _: () = assert!(MIN_INDEX_INTERVAL_SECS <= 600);
    }

    #[test]
    fn test_default_check_interval() {
        use crate::runtime_config::DEFAULT_CHECK_INTERVAL_SECS;
        const _: () = assert!(DEFAULT_CHECK_INTERVAL_SECS >= 30);
        const _: () = assert!(DEFAULT_CHECK_INTERVAL_SECS <= 300);
    }

    #[test]
    fn test_default_max_indexers_per_tick() {
        // Must be small enough to avoid a burst of concurrent yt-dlp
        // processes, but not so small that a large backlog never drains.
        const _: () = assert!(DEFAULT_MAX_INDEXERS_PER_TICK >= 1);
        const _: () = assert!(DEFAULT_MAX_INDEXERS_PER_TICK <= 20);
    }

    // NOTE: this exercises `EffectiveSettings::indexing_paused` directly via
    // `resolve`, not the `CheckSources` handler's gate itself — there is no
    // actor-level assertion here that a paused tick actually skips spawning.
    #[test]
    fn paused_indexing_blocks_new_indexers() {
        use crate::db::RuntimeSettingsRow;
        use crate::runtime_config::{EnvOverrides, resolve};

        let row = RuntimeSettingsRow {
            indexing_paused_until: Some(Utc::now() + chrono::Duration::hours(1)),
            ..RuntimeSettingsRow::default()
        };
        let s = resolve(&row, &EnvOverrides::default());
        assert!(s.indexing_paused(Utc::now()));
    }

    // These test `SchedulerActor::indexing_refusal_for` — the pure decision
    // function behind `indexing_refusal` — directly against constructed
    // `EffectiveSettings` and a `draining` flag. This is real coverage of
    // the gate logic itself (unlike `paused_indexing_blocks_new_indexers`
    // above), but it is still not an actor-level test: it does not drive
    // `SchedulerActor::handle` for `CheckSources` or `IndexSource`, which
    // would require spawning a full actor with a live `PgPool` and download
    // supervisor. The `IndexSource` handler path remains unverified beyond
    // this predicate; see the task report for why.
    mod indexing_refusal_tests {
        use crate::db::RuntimeSettingsRow;
        use crate::runtime_config::{EnvOverrides, resolve};

        use super::*;

        fn settings_with_indexing_paused_until(
            until: Option<chrono::DateTime<Utc>>,
        ) -> EffectiveSettings {
            let row = RuntimeSettingsRow {
                indexing_paused_until: until,
                ..RuntimeSettingsRow::default()
            };
            resolve(&row, &EnvOverrides::default())
        }

        #[test]
        fn not_paused_not_draining_allows_indexing() {
            let now = Utc::now();
            let settings = settings_with_indexing_paused_until(None);
            assert_eq!(
                SchedulerActor::indexing_refusal_for(&settings, false, now),
                None
            );
        }

        #[test]
        fn paused_refuses_naming_the_pause() {
            let now = Utc::now();
            let until = now
                .checked_add_signed(chrono::Duration::hours(1))
                .expect("in range");
            let settings = settings_with_indexing_paused_until(Some(until));

            let reason = SchedulerActor::indexing_refusal_for(&settings, false, now)
                .expect("paused settings must refuse");
            assert!(reason.contains("paused"), "reason was: {reason}");
            assert!(!reason.contains("drain"), "reason was: {reason}");
        }

        #[test]
        fn indefinite_pause_refuses_without_leaking_the_sentinel() {
            let now = Utc::now();
            let settings = settings_with_indexing_paused_until(Some(indefinite_pause()));

            let reason = SchedulerActor::indexing_refusal_for(&settings, false, now)
                .expect("indefinitely paused settings must refuse");
            assert!(reason.contains("indefinitely"), "reason was: {reason}");
            assert!(!reason.contains("9999"), "reason was: {reason}");
        }

        #[test]
        fn draining_refuses_naming_the_drain() {
            let now = Utc::now();
            let settings = settings_with_indexing_paused_until(None);

            let reason = SchedulerActor::indexing_refusal_for(&settings, true, now)
                .expect("draining must refuse");
            assert!(reason.contains("drain"), "reason was: {reason}");
            assert!(!reason.contains("paused"), "reason was: {reason}");
        }

        #[test]
        fn both_paused_and_draining_the_drain_reason_wins() {
            // Drain takes priority: once the process is shutting down, the
            // pause deadline is moot, so the drain refusal is the more
            // useful thing to tell the operator.
            let now = Utc::now();
            let until = now
                .checked_add_signed(chrono::Duration::hours(1))
                .expect("in range");
            let settings = settings_with_indexing_paused_until(Some(until));

            let reason = SchedulerActor::indexing_refusal_for(&settings, true, now)
                .expect("both paused and draining must refuse");
            assert!(reason.contains("drain"), "reason was: {reason}");
            assert!(!reason.contains("paused"), "reason was: {reason}");
        }
    }
}
