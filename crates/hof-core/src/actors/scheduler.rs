//! The `SchedulerActor` is a singleton that fires indexing jobs on a per-source
//! schedule using `tokio::time`. On each tick it messages the appropriate
//! `SourceIndexerActor` to begin indexing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use kameo::Reply;
use kameo::prelude::*;
use sqlx::PgPool;
use tokio::time::{Instant, MissedTickBehavior, interval};
use tracing::{debug, error, info, instrument, warn};
use ulid::Ulid;

use crate::db;
use crate::db::ActivityBroadcaster;
use crate::domain::activity::{ActivityEventType, ActivitySeverity};
use crate::domain::source::Source;
use crate::ytdlp::YtdlpClient;

use super::download_supervisor::{DownloadSupervisor, ProcessPendingDownloads};
use super::source_indexer::{IndexingResult, SourceIndexerActor, SourceIndexerArgs};

/// Default interval for checking which sources need indexing.
const DEFAULT_CHECK_INTERVAL_SECS: u64 = 60;

/// Minimum interval between indexing the same source (rate limiting).
const MIN_INDEX_INTERVAL_SECS: u64 = 300; // 5 minutes

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
    /// Interval for checking sources.
    check_interval: Duration,
    /// Track when each source was last indexed (for rate limiting).
    last_indexed: HashMap<Ulid, Instant>,
    /// Active indexing tasks (`source_id` -> actor ref).
    active_indexers: HashMap<Ulid, ActorRef<SourceIndexerActor>>,
    /// Whether the scheduler is running.
    running: bool,
    /// Broadcaster for real-time SSE notifications.
    broadcaster: ActivityBroadcaster,
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
    /// Optional custom check interval.
    pub check_interval: Option<Duration>,
    pub broadcaster: ActivityBroadcaster,
}

impl Actor for SchedulerActor {
    type Args = SchedulerArgs;
    type Error = color_eyre::eyre::Error;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let check_interval = args
            .check_interval
            .unwrap_or(Duration::from_secs(DEFAULT_CHECK_INTERVAL_SECS));

        info!(
            check_interval_secs = check_interval.as_secs(),
            "Scheduler actor starting"
        );

        let scheduler = Self {
            pool: args.pool,
            ytdlp: args.ytdlp,
            supervisor: args.supervisor,
            check_interval,
            last_indexed: HashMap::new(),
            active_indexers: HashMap::new(),
            running: false,
            broadcaster: args.broadcaster,
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
        let check_interval = self.check_interval;

        // Spawn the scheduling loop
        tokio::spawn(async move {
            let mut interval = interval(check_interval);
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                // Check if we should continue
                if !actor_ref.is_alive() {
                    break;
                }

                // Trigger a check
                if let Err(e) = actor_ref.tell(CheckSources).try_send() {
                    error!(error = %e, "Failed to send CheckSources message");
                    break;
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

        debug!("Checking for sources due for indexing");

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
        } else {
            info!(count = sources.len(), "Found sources due for indexing");

            for source in sources {
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
                if let Err(e) = self.spawn_indexer(&source, ctx.actor_ref().clone()).await {
                    error!(
                        source_id = %source.id,
                        error = %e,
                        "Failed to spawn indexer"
                    );
                }
            }
        }

        // Always sweep pending/retry-ready downloads every scheduler tick,
        // even when no sources are due for indexing.
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
        // Check if already being indexed
        if self.active_indexers.contains_key(&msg.source_id) {
            return Err("Source is already being indexed".to_string());
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
            check_interval_secs: self.check_interval.as_secs(),
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
        const _: () = assert!(DEFAULT_CHECK_INTERVAL_SECS >= 30);
        const _: () = assert!(DEFAULT_CHECK_INTERVAL_SECS <= 300);
    }
}
