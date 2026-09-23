//! `RootSupervisor`: in-process supervision for the four singleton actors.
//!
//! # The bug this exists to fix
//!
//! Production stopped downloading for 27 hours (2026-09-15T10:17Z →
//! 2026-09-16T13:24Z) with 9 videos stuck in `pending`. The
//! `DownloadSupervisor`'s `ProcessPendingDownloads` handler returned `Err` on
//! a transient Postgres pool-acquire timeout. That message is delivered via
//! `tell()` (no reply channel), so kameo escalated the `Err` to `on_panic`,
//! whose default is `ControlFlow::Break(ActorStopReason::Panicked)` — the
//! actor stopped. Nothing restarted it, because this codebase had no
//! supervision at all: every singleton actor was a bare `T::spawn(args)`
//! with no supervisor above it. A peer's fix (`download_supervisor.rs`) made
//! that specific handler non-fatal. This module is the general fix: even a
//! *correctly-written* handler can still panic on something nobody thought
//! of, so the four singleton actors (`DownloadSupervisor`, `SchedulerActor`,
//! `CleanupActor`, `JellyfinMetadataActor`) are now supervised children of a
//! `RootSupervisor`, restarted automatically in-process, with a manual
//! restart endpoint as a second line of defense.
//!
//! # Why `RootSupervisor` itself is never supervised
//!
//! It is spawned as a bare, unsupervised actor
//! (`RootSupervisor::spawn(args)`, not `supervise_with`). It deliberately
//! holds no `PgPool`, performs no IO, and its `on_start` cannot fail (`type
//! Error = Infallible`) — the only thing it does is spawn its four children
//! and answer read-only status/control messages about them. There is
//! nothing left for it to fail *on*. Layering another supervisor above it
//! would just move the "what if the supervisor dies" question up one level
//! without answering it.
//!
//! # Why the four children use `RestartPolicy::Transient`, not `Permanent`
//!
//! `Permanent` restarts a child no matter how it stopped, including a clean
//! `ctx.stop()` / `stop_gracefully()`. `Transient` only restarts on an
//! *abnormal* stop (`ActorStopReason::is_normal() == false` — panics,
//! `kill()`, a linked-actor death) and leaves a normally-stopped child
//! stopped. This is what makes `startup::shutdown` work at all: it calls
//! `stop_gracefully()` on each child directly, which stops that child with
//! reason `Normal`. Under `Permanent`, kameo would immediately restart it —
//! shutdown would never converge, or would race a fresh actor's startup
//! against the process exiting. Under `Transient`, a graceful stop is
//! respected and the child stays down. See kameo's
//! `links.rs::ErasedChildSpec::should_restart`, which special-cases exactly
//! this: `RestartPolicy::Transient if reason.is_normal() => Break(..)`.
//!
//! `kill()` (kameo's `AbortHandle::abort()`) produces
//! `ActorStopReason::Killed`, and `Killed.is_normal() == false` (see
//! kameo's `error.rs::ActorStopReason::is_normal`). So `kill()` on a
//! `Transient`-policy child *does* trigger a restart — this is the manual
//! restart mechanism `RestartActor` below relies on: killing a child is,
//! from the supervisor's point of view, indistinguishable from it having
//! crashed, and gets the same automatic recovery.
//!
//! # Why the cached `ActorRef<T>` clones scattered across the codebase
//! (`hof-api`'s `AppState`, `SchedulerArgs::supervisor`,
//! `SourceIndexerArgs::supervisor`, `startup::ActorSystem`) stay valid
//! across a restart
//!
//! kameo's `SupervisedActorBuilder::spawn_inner`
//! (`kameo-0.22.2/src/supervision.rs`) generates the child's `ActorId`
//! **once**, at the initial `spawn()` call, and captures it (along with the
//! mailbox sender/receiver pair) inside the restart factory closure:
//!
//! ```text
//! // supervision.rs:641,706
//! let actor_id = ActorId::generate();
//! ...
//! let prepared = PreparedActor::new_with(actor_id, (mailbox_tx, mailbox_rx), links);
//! ```
//!
//! Every subsequent automatic restart reuses that same `actor_id` and
//! `mailbox_tx` to build the next incarnation. An `ActorRef<T>` is just a
//! handle wrapping a `mailbox_tx` (plus the id/links) — since restarts reuse
//! the same sender, a clone taken before a restart still delivers to the new
//! instance after one. This is the entire reason this design is a
//! contained addition rather than a refactor: nothing holding an
//! `ActorRef<DownloadSupervisor>` needs to change, or even know a restart
//! happened. [`tests::killed_child_actor_ref_survives_restart`] pins this
//! behavior directly so a future kameo upgrade that changes it breaks
//! loudly here instead of silently leaving every cached ref pointing at a
//! dead mailbox.
//!
//! # Why `on_link_died` must be overridden to never stop the root
//!
//! kameo's default `Actor::on_link_died` (`actor.rs`) stops the actor for
//! any linked-actor death other than `Normal`/`SupervisorRestart`. Naively,
//! that sounds irrelevant here — surely a *supervised child's* death is
//! kameo's own business, not something that reaches this hook? It is more
//! subtle than that. Tracing kameo's `handle_link_died`
//! (`actor/kind.rs:237`): when the dead actor is a registered child
//! (`links.children.get(&id)`), kameo checks `should_restart`. If it
//! decides to restart (the common case — panic or `kill()`, budget
//! available), it calls the respawn factory and **returns before ever
//! calling `Actor::on_link_died`** — the override below is not even invoked
//! for an ordinary successful restart. But if it decides *not* to restart
//! (`NoRestartReason::MaxRestartsExceeded`, or `NormalExitUnderTransientPolicy`
//! during a graceful shutdown), it falls through to call
//! `Actor::on_link_died` after all. So this hook fires in exactly the cases
//! where kameo has already given up on the child — and the default
//! implementation's response to that is to stop *this* actor too. For any
//! other actor that might be fine; for `RootSupervisor` it would mean one
//! child permanently exhausting its restart budget takes the *entire
//! supervision tree* down with it — the exact cascading failure this module
//! exists to prevent. The override below always returns `Continue`,
//! recording the failure in `health` instead of dying from it.
//!
//! One consequence of this tracing: `RootSupervisor` cannot use
//! `on_link_died` to count *every* restart (successful automatic restarts
//! never reach it, as noted above). `restart_count`/`last_restart_at` in
//! [`ActorHealthReport`] are therefore populated by two disjoint paths: this
//! hook running (when a restart was declined — sets `unrecoverable`), and
//! [`RestartActor`]'s own end-to-end kill/poll (when an operator explicitly
//! requests one). A single successful *spontaneous* automatic restart
//! in between is not separately counted; `alive` in the health report is
//! always live (`ActorRef::is_alive()`, read fresh on every
//! [`GetActorHealth`] ask), so a currently-dead actor is never hidden by
//! this gap even if its restart history under-counts.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use kameo::Reply;
use kameo::error::Infallible;
use kameo::prelude::*;
use kameo::supervision::RestartPolicy;
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};
use utoipa::ToSchema;

use crate::actors::cleanup::{CleanupActor, CleanupActorArgs};
use crate::actors::download_supervisor::{
    DownloadSupervisor, DownloadSupervisorArgs, ProcessPendingDownloads,
};
use crate::actors::jellyfin_metadata::{JellyfinMetadataActor, JellyfinMetadataActorArgs};
use crate::actors::scheduler::{SchedulerActor, SchedulerArgs};
use crate::config::DownloadConfig;
use crate::db::ActivityBroadcaster;
use crate::domain::video::DownloadProgress;
use crate::runtime_config::{DrainToken, RuntimeConfig};
use crate::ytdlp::YtdlpClient;

/// Restart budget per child, shared by all four. Wide enough that an
/// operator clicking "restart" repeatedly (human-paced — seconds apart, not
/// milliseconds) never trips it, while a genuine crash loop (the same fault
/// recurring immediately after every restart) still does. See
/// `RestartTracker::record_restart` in kameo's `links.rs`: the window resets
/// on the first restart *outside* it, so this is "at most 10 restarts within
/// any trailing 30s", not a lifetime cap.
const RESTART_LIMIT: u32 = 10;
const RESTART_WINDOW: Duration = Duration::from_secs(30);

/// How long [`RestartActor`] waits, after `kill()`ing a child, for kameo's
/// supervision machinery to respawn it before giving up and reporting
/// failure back to the caller.
const RESTART_POLL_BOUND: Duration = Duration::from_secs(5);
const RESTART_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Delay before the *first* `is_alive()` read after `kill()`, in
/// [`restart_and_wait`].
///
/// `kill()` (`AbortHandle::abort()`) only signals the actor's task to stop
/// at its next scheduling opportunity — it does not take effect
/// synchronously. Reading `is_alive()` in the same instant as `kill()` (no
/// `.await` in between) observes pre-kill state and reads `true`
/// regardless of outcome, including for a restart that is about to be
/// declined. Empirically (see this module's tests) that stale-`true`
/// reading is reliable — it is not a rare race, it happens on effectively
/// every call — so the very first read must not be trusted; this delay
/// gives kameo's internal teardown a real chance to run first. See
/// [`restart_and_wait`] for why polling afterward is still needed on top of
/// this.
const KILL_SETTLE_DELAY: Duration = Duration::from_millis(200);

/// Everything [`RootSupervisor::on_start`] needs to build fresh `Args` for
/// each of its four children.
///
/// Used both at initial spawn and on every subsequent automatic restart:
/// `supervise_with`'s factory closure is called again each time, not just
/// once.
///
/// Every field here is cheap to clone: `PgPool` and `Arc<YtdlpClient>` are
/// `Arc`-backed, `mpsc::Sender` and `RuntimeConfig` are shared channel
/// handles, `ActivityBroadcaster`/`DrainToken` are the same handles already
/// threaded everywhere else in `startup.rs`. That is exactly what a
/// `supervise_with` args factory needs, since `DownloadSupervisorArgs`
/// itself is not `Clone` (it owns a one-shot `mpsc::Sender` and a
/// `watch::Receiver`, and — being a distinct `watch::Receiver` per
/// restart — each restart should see the *current* runtime settings, not a
/// stale snapshot from process startup).
pub struct RootSupervisorArgs {
    pub pool: PgPool,
    pub ytdlp: Arc<YtdlpClient>,
    pub download_config: DownloadConfig,
    pub progress_tx: mpsc::Sender<DownloadProgress>,
    /// Cloned into each child's factory closure, which calls
    /// `.subscribe()` fresh on every invocation rather than sharing one
    /// long-lived `watch::Receiver`.
    pub runtime_config: RuntimeConfig,
    pub broadcaster: ActivityBroadcaster,
    pub drain: DrainToken,
    pub global_retention_days: Option<u32>,
    /// Whether the timer-driven children start their loops on spawn.
    ///
    /// Always `true` in production. Tests default to `false`: every test
    /// spawns the full tree to get the child refs `AppState` needs, and a
    /// scheduler or cleanup pass firing on spawn mutates the same rows the
    /// test just seeded.
    pub autostart: bool,
}

/// One of the four actors `RootSupervisor` supervises.
///
/// The string form (see [`Self::as_str`]) is a URL path segment
/// (`POST /api/v1/system/actors/{name}/restart`) and therefore effectively
/// public API — never change or reuse a spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SupervisedActor {
    DownloadSupervisor,
    Scheduler,
    Cleanup,
    JellyfinMetadata,
}

impl SupervisedActor {
    /// Every supervised actor, in the order [`GetActorHealth`] reports them.
    pub const ALL: [Self; 4] = [
        Self::DownloadSupervisor,
        Self::Scheduler,
        Self::Cleanup,
        Self::JellyfinMetadata,
    ];

    /// URL / log spelling. See the type-level doc for why this is
    /// effectively frozen.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::DownloadSupervisor => "download_supervisor",
            Self::Scheduler => "scheduler",
            Self::Cleanup => "cleanup",
            Self::JellyfinMetadata => "jellyfin_metadata",
        }
    }
}

/// Restart/failure bookkeeping `RootSupervisor` keeps per child. Mutated
/// from exactly two places: [`RootSupervisor::on_link_died`] (a restart was
/// declined) and [`Message<RecordRestartOutcome>`]'s handler (an operator
/// restart via [`RestartActor`] finished, one way or the other). See the
/// module doc for why an ordinary spontaneous automatic restart touches
/// neither.
#[derive(Debug, Clone, Default)]
struct ActorHealthState {
    restart_count: u32,
    last_restart_at: Option<DateTime<Utc>>,
    last_failure: Option<String>,
    unrecoverable: bool,
}

/// Point-in-time health of one supervised actor, as reported by
/// [`GetActorHealth`].
#[derive(Debug, Serialize, ToSchema)]
pub struct ActorHealthReport {
    pub actor: SupervisedActor,
    pub alive: bool,
    pub restart_count: u32,
    pub last_restart_at: Option<DateTime<Utc>>,
    pub last_failure: Option<String>,
    pub unrecoverable: bool,
}

/// Ask for the current health of all four supervised actors.
///
/// Deliberately synchronous-cheap on `RootSupervisor`'s side: `alive` is a
/// live `ActorRef::is_alive()` read (lock-free, cannot hang), and everything
/// else is a plain struct-field read — this handler never awaits anything
/// and cannot itself become the reason a health probe stalls.
pub struct GetActorHealth;

impl Message<GetActorHealth> for RootSupervisor {
    type Reply = Vec<ActorHealthReport>;

    async fn handle(
        &mut self,
        _msg: GetActorHealth,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        SupervisedActor::ALL
            .into_iter()
            .map(|which| {
                let alive = match which {
                    SupervisedActor::DownloadSupervisor => self.supervisor.is_alive(),
                    SupervisedActor::Scheduler => self.scheduler.is_alive(),
                    SupervisedActor::Cleanup => self.cleanup.is_alive(),
                    SupervisedActor::JellyfinMetadata => self.jellyfin_metadata.is_alive(),
                };
                let state = self.health.get(&which).cloned().unwrap_or_default();
                ActorHealthReport {
                    actor: which,
                    alive,
                    restart_count: state.restart_count,
                    last_restart_at: state.last_restart_at,
                    last_failure: state.last_failure,
                    unrecoverable: state.unrecoverable,
                }
            })
            .collect()
    }
}

/// Restart one supervised actor on request (the manual recovery path for an
/// actor that died and was not — or could not be — restarted automatically).
pub struct RestartActor {
    pub which: SupervisedActor,
}

/// Internal report-back from a detached restart task (see
/// [`Message<RestartActor>`]'s handler) to `RootSupervisor`'s own mailbox,
/// where the actual `health` mutation happens. Not part of the public
/// contract — nothing outside this module constructs one.
struct RecordRestartOutcome {
    which: SupervisedActor,
    outcome: Result<(), String>,
}

impl Message<RecordRestartOutcome> for RootSupervisor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RecordRestartOutcome,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let entry = self.health.entry(msg.which).or_default();
        match msg.outcome {
            Ok(()) => {
                entry.restart_count += 1;
                entry.last_restart_at = Some(Utc::now());
                // A successful restart is definitive proof recovery still
                // works; clear any stale flag a previous failed attempt left.
                entry.unrecoverable = false;
            }
            Err(reason) => {
                entry.last_failure = Some(reason);
                // Deliberately NOT forcing `unrecoverable = true` here: this
                // handler only knows the poll in `restart_and_wait` timed
                // out, not *why*. `on_link_died` is the authoritative source
                // for that — it observes kameo's actual restart-budget
                // decision, and (since `RootSupervisor`'s mailbox is
                // otherwise idle right after a `kill()`) will typically have
                // already set this flag by the time this message is
                // processed, if the budget really is exhausted.
            }
        }
    }
}

/// Pull the freshly-spawned child `ActorRef`s back out of `RootSupervisor`.
///
/// Asked once right after start so `startup::initialize` can populate
/// `ActorSystem`'s `supervisor`/`scheduler`/`cleanup`/`jellyfin_metadata`
/// fields, which the rest of the codebase already depends on. Public so
/// out-of-crate test harnesses can wire a `TestApp` the same way
/// `startup::initialize` does, rather than spawning a second, divergent set.
pub struct GetChildRefs;

#[derive(Reply)]
pub struct ChildRefs {
    pub supervisor: ActorRef<DownloadSupervisor>,
    pub scheduler: ActorRef<SchedulerActor>,
    pub cleanup: ActorRef<CleanupActor>,
    pub jellyfin_metadata: ActorRef<JellyfinMetadataActor>,
}

impl Message<GetChildRefs> for RootSupervisor {
    type Reply = ChildRefs;

    async fn handle(
        &mut self,
        _msg: GetChildRefs,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        ChildRefs {
            supervisor: self.supervisor.clone(),
            scheduler: self.scheduler.clone(),
            cleanup: self.cleanup.clone(),
            jellyfin_metadata: self.jellyfin_metadata.clone(),
        }
    }
}

/// The parent of the four singleton actors. See the module doc for the full
/// design rationale.
pub struct RootSupervisor {
    supervisor: ActorRef<DownloadSupervisor>,
    scheduler: ActorRef<SchedulerActor>,
    cleanup: ActorRef<CleanupActor>,
    jellyfin_metadata: ActorRef<JellyfinMetadataActor>,
    /// Reverse lookup from a dying child's `ActorId` (all kameo tells
    /// `on_link_died` is the id) back to which of the four it was.
    child_by_id: HashMap<ActorId, SupervisedActor>,
    health: HashMap<SupervisedActor, ActorHealthState>,
}

/// Spawn the supervised Jellyfin metadata child.
///
/// Lifted out of [`RootSupervisor::on_start`] purely to keep that function
/// under the `clippy::too_many_lines` bound; it is spawned exactly the same
/// way as its three siblings.
async fn spawn_jellyfin_metadata(
    parent: &ActorRef<RootSupervisor>,
    pool: PgPool,
    broadcaster: ActivityBroadcaster,
    autostart: bool,
) -> ActorRef<JellyfinMetadataActor> {
    JellyfinMetadataActor::supervise_with(parent, move || JellyfinMetadataActorArgs {
        pool: pool.clone(),
        check_interval: None, // use the actor's own default
        broadcaster: broadcaster.clone(),
        autostart,
    })
    .restart_policy(RestartPolicy::Transient)
    .restart_limit(RESTART_LIMIT, RESTART_WINDOW)
    .spawn()
    .await
}

impl Actor for RootSupervisor {
    type Args = RootSupervisorArgs;
    /// `on_start` below only spawns children and builds plain data
    /// structures — there is no fallible operation in it, and no `PgPool`
    /// use of its own to fail. Keeping this uninhabited (rather than the
    /// `color_eyre::eyre::Error` the other four actors use) means the
    /// compiler itself enforces "this actor cannot fail to start", not just
    /// a doc comment's say-so.
    type Error = Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let RootSupervisorArgs {
            pool,
            ytdlp,
            download_config,
            progress_tx,
            runtime_config,
            broadcaster,
            drain,
            global_retention_days,
            autostart,
        } = args;

        // Spawn order matches the pre-supervision `startup.rs`: the
        // scheduler's args need the supervisor's `ActorRef`, so the
        // supervisor must exist first. That ref, once captured in the
        // scheduler's factory closure below, stays valid across any number
        // of the *supervisor's own* independent restarts (see the module
        // doc's `ActorRef` stability section) — it never needs to be
        // re-fetched.
        let supervisor = {
            let pool = pool.clone();
            let ytdlp = ytdlp.clone();
            let download_config = download_config.clone();
            let progress_tx = progress_tx.clone();
            let runtime_config = runtime_config.clone();
            let broadcaster = broadcaster.clone();
            let drain = drain.clone();
            DownloadSupervisor::supervise_with(&actor_ref, move || DownloadSupervisorArgs {
                pool: pool.clone(),
                ytdlp: ytdlp.clone(),
                config: download_config.clone(),
                progress_tx: progress_tx.clone(),
                config_rx: runtime_config.subscribe(),
                broadcaster: broadcaster.clone(),
                drain: drain.clone(),
            })
            .restart_policy(RestartPolicy::Transient)
            .restart_limit(RESTART_LIMIT, RESTART_WINDOW)
            .spawn()
            .await
        };

        let scheduler = {
            let pool = pool.clone();
            let ytdlp = ytdlp.clone();
            let supervisor = supervisor.clone();
            let runtime_config = runtime_config.clone();
            let broadcaster = broadcaster.clone();
            let drain = drain.clone();
            SchedulerActor::supervise_with(&actor_ref, move || SchedulerArgs {
                pool: pool.clone(),
                ytdlp: ytdlp.clone(),
                supervisor: supervisor.clone(),
                config_rx: runtime_config.subscribe(),
                broadcaster: broadcaster.clone(),
                drain: drain.clone(),
                autostart,
            })
            .restart_policy(RestartPolicy::Transient)
            .restart_limit(RESTART_LIMIT, RESTART_WINDOW)
            .spawn()
            .await
        };

        let cleanup = {
            let pool = pool.clone();
            let runtime_config = runtime_config.clone();
            let broadcaster = broadcaster.clone();
            CleanupActor::supervise_with(&actor_ref, move || CleanupActorArgs {
                pool: pool.clone(),
                global_retention_days,
                config_rx: runtime_config.subscribe(),
                broadcaster: broadcaster.clone(),
                autostart,
            })
            .restart_policy(RestartPolicy::Transient)
            .restart_limit(RESTART_LIMIT, RESTART_WINDOW)
            .spawn()
            .await
        };

        let jellyfin_metadata =
            spawn_jellyfin_metadata(&actor_ref, pool.clone(), broadcaster.clone(), autostart).await;

        let child_by_id = HashMap::from([
            (supervisor.id(), SupervisedActor::DownloadSupervisor),
            (scheduler.id(), SupervisedActor::Scheduler),
            (cleanup.id(), SupervisedActor::Cleanup),
            (jellyfin_metadata.id(), SupervisedActor::JellyfinMetadata),
        ]);

        info!("Root supervisor started; all four singleton actors supervised");

        Ok(Self {
            supervisor,
            scheduler,
            cleanup,
            jellyfin_metadata,
            child_by_id,
            health: HashMap::new(),
        })
    }

    /// See the module doc's "Why `on_link_died` must be overridden" section.
    /// Never stops the root — a child exhausting its restart budget (or
    /// stopping normally during shutdown) must not cascade into taking down
    /// the supervisor that every other actor's liveness depends on.
    async fn on_link_died(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        id: ActorId,
        reason: ActorStopReason,
    ) -> Result<ControlFlow<ActorStopReason>, Self::Error> {
        let Some(&which) = self.child_by_id.get(&id) else {
            // Not one of our four — RootSupervisor links to nothing else,
            // but the safe default for an unrecognized link dying is still
            // "don't stop yourself over it".
            return Ok(ControlFlow::Continue(()));
        };

        if reason.is_normal() {
            // Reaching this hook with a `Normal` reason means the child was
            // deliberately `stop_gracefully()`'d (e.g. system shutdown) and
            // `Transient` correctly declined to restart it. Not a failure.
            debug!(
                actor = which.as_str(),
                "supervised actor stopped normally (graceful shutdown)"
            );
        } else {
            // Reaching this hook with anything else means kameo declined an
            // automatic restart — almost certainly `MaxRestartsExceeded`
            // (see the module doc). This is the case the 27-hour outage
            // needs to never repeat silently: log loudly and mark it.
            error!(
                actor = which.as_str(),
                reason = %reason,
                "supervised actor was NOT restarted in-process (restart budget \
                 exhausted); a process restart is required to recover it"
            );
            let entry = self.health.entry(which).or_default();
            entry.last_failure = Some(reason.to_string());
            entry.unrecoverable = true;
        }

        Ok(ControlFlow::Continue(()))
    }
}

impl Message<RestartActor> for RootSupervisor {
    /// Wrapped in an outer `Result<_, Infallible>` — rather than the bare
    /// `Result<(), String>` every other `Result`-replying message in this
    /// crate uses — so kameo's blanket `Reply for Result<T, E>` impl binds
    /// `Ok = Result<(), String>` for `ask()`'s purposes, instead of the
    /// usual `Ok = T, Error = E` split (see e.g. `CancelDownload`, whose
    /// callers match a single-level `Ok(())`/`Err(e)`). Callers of *this*
    /// message need the full inner `Result` back as `ask()`'s `Ok` payload —
    /// matching `Ok(Ok(()))` vs `Ok(Err(reason))` — because "the root
    /// supervisor declined to restart it" (an expected, answerable outcome)
    /// and "the root supervisor could not be reached at all" (a transport
    /// failure) are different problems and callers need to tell them apart.
    /// `Infallible` documents that the outer error arm can never actually be
    /// constructed: this handler always resolves through the inner
    /// `Result`.
    ///
    /// `DelegatedReply` (rather than returning the value directly from an
    /// `async fn handle`) is not a style choice — see the big comment on
    /// [`restart_and_wait`] for why this handler would deadlock against its
    /// own actor's mailbox loop without it.
    type Reply = DelegatedReply<Result<Result<(), String>, Infallible>>;

    async fn handle(
        &mut self,
        msg: RestartActor,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let which = msg.which;
        let root_ref = ctx.actor_ref().clone();

        info!(actor = which.as_str(), "restart requested");

        match which {
            SupervisedActor::DownloadSupervisor => {
                let child_ref = self.supervisor.clone();
                ctx.spawn(async move {
                    let outcome = restart_and_wait(&child_ref, which).await;
                    if outcome.is_ok() {
                        // Mirror the startup kick (`startup::initialize`,
                        // right after the actor system comes up) so a
                        // backlog drains immediately instead of waiting up
                        // to a full scheduler tick.
                        if let Err(error) = child_ref.tell(ProcessPendingDownloads).await {
                            warn!(%error, "could not kick pending downloads after restart");
                        }
                    }
                    root_ref
                        .tell(RecordRestartOutcome {
                            which,
                            outcome: outcome.clone(),
                        })
                        .await
                        .ok();
                    Ok(outcome)
                })
            }
            SupervisedActor::Scheduler => {
                let child_ref = self.scheduler.clone();
                ctx.spawn(async move { Ok(report_restart(child_ref, which, root_ref).await) })
            }
            SupervisedActor::Cleanup => {
                let child_ref = self.cleanup.clone();
                ctx.spawn(async move { Ok(report_restart(child_ref, which, root_ref).await) })
            }
            SupervisedActor::JellyfinMetadata => {
                let child_ref = self.jellyfin_metadata.clone();
                ctx.spawn(async move { Ok(report_restart(child_ref, which, root_ref).await) })
            }
        }
    }
}

/// Shared tail of [`Message<RestartActor>`]'s handler for the three children
/// that don't need a post-restart kick: wait for the restart, report the
/// outcome back to `RootSupervisor` for bookkeeping, and return it.
async fn report_restart<A: Actor>(
    child_ref: ActorRef<A>,
    which: SupervisedActor,
    root_ref: ActorRef<RootSupervisor>,
) -> Result<(), String> {
    let outcome = restart_and_wait(&child_ref, which).await;
    root_ref
        .tell(RecordRestartOutcome {
            which,
            outcome: outcome.clone(),
        })
        .await
        .ok();
    outcome
}

/// `kill()` a supervised child, then poll for kameo's own supervision
/// machinery to bring it back.
///
/// # Why this must run detached from `RootSupervisor`'s own mailbox loop
///
/// kameo processes exactly one signal at a time per actor — see kameo's
/// `actor/spawn.rs::recv_mailbox_loop`: a `Message` handler runs to full
/// completion before the *next* signal, including a `Signal::LinkDied`, is
/// even dequeued. The respawn this function polls for is performed by
/// `RootSupervisor`'s own `handle_link_died` (kameo internal, not
/// [`RootSupervisor::on_link_died`] above — see the module doc), triggered
/// by the `Signal::LinkDied` that `kill()` causes the child to send back to
/// `RootSupervisor`'s mailbox. If this function ran directly inside
/// `Message<RestartActor>::handle` — the same execution the mailbox loop is
/// waiting on to finish before it can process anything else — it would be
/// polling for an event that structurally cannot occur until the very
/// handler doing the polling returns. Every restart would silently spin for
/// the full `RESTART_POLL_BOUND` and report a false failure, even though the
/// child would come back the instant the handler returned. This is why
/// `Message<RestartActor>::handle` runs this via `ctx.spawn` (a detached
/// tokio task, decoupled from the actor's own mailbox loop) rather than
/// awaiting it inline.
///
/// # Why this does NOT wait for `is_alive() == false` before polling for `true`
///
/// The obvious-looking approach — wait for the kill to take visible effect,
/// then wait for it to come back — does not work against kameo's actual
/// behavior here, and this module's tests
/// (`sleep_then_check_declined`/`_success`-shaped cases folded into
/// [`tests`]) pin why: for a restart kameo actually performs, the same
/// mailbox receiver is hand-carried from the dying instance straight to the
/// reborn one without ever being fully dropped, so `is_alive()` can stay
/// `true` continuously through the entire cycle — there is no reliable
/// "and it dipped false in between" window to wait for. A *declined*
/// restart (budget exhausted) is the opposite: the receiver genuinely is
/// dropped once kameo's `handle_link_died` falls through without reusing
/// it, and `is_alive()` then reads `false` and stays `false` for good. So
/// the two outcomes are correctly told apart by whether `is_alive()` ever
/// reads `true` *after* [`KILL_SETTLE_DELAY`] has given kameo a real chance
/// to reach one outcome or the other — not by watching for a transition.
async fn restart_and_wait<A: Actor>(
    child_ref: &ActorRef<A>,
    which: SupervisedActor,
) -> Result<(), String> {
    child_ref.kill();

    // Never trust an `is_alive()` read taken in the same instant as
    // `kill()` — see `KILL_SETTLE_DELAY`'s doc for why it is reliably stale
    // (reads `true`) rather than a rare race.
    tokio::time::sleep(KILL_SETTLE_DELAY).await;

    let restart_by = Instant::now() + RESTART_POLL_BOUND;
    loop {
        if child_ref.is_alive() {
            return Ok(());
        }
        if Instant::now() >= restart_by {
            break;
        }
        tokio::time::sleep(RESTART_POLL_INTERVAL).await;
    }

    Err(format!(
        "{} did not come back within {}s of being killed; the restart budget is \
         likely exhausted and a process restart is required",
        which.as_str(),
        RESTART_POLL_BOUND.as_secs(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ------------------------------------------------------------------
    // A minimal toy actor pair, independent of the crate's real actors (no
    // `PgPool`, no yt-dlp binary, no `#[ignore]` needed) — its only purpose
    // is to pin kameo's own supervised-restart mechanics, which is a
    // property of kameo's supervision machinery in general, not of anything
    // `DownloadSupervisor`-specific.
    // ------------------------------------------------------------------

    struct ToyRoot;

    impl Actor for ToyRoot {
        type Args = Self;
        type Error = Infallible;

        async fn on_start(state: Self::Args, _: ActorRef<Self>) -> Result<Self, Self::Error> {
            Ok(state)
        }
    }

    /// Counts how many times it has been (re)started, via a shared counter
    /// so the test can observe restarts from outside.
    struct ToyChild {
        starts: Arc<AtomicU32>,
    }

    impl Actor for ToyChild {
        type Args = Self;
        type Error = Infallible;

        async fn on_start(state: Self::Args, _: ActorRef<Self>) -> Result<Self, Self::Error> {
            state.starts.fetch_add(1, Ordering::SeqCst);
            Ok(state)
        }
    }

    struct Ping;

    impl Message<Ping> for ToyChild {
        type Reply = u32;

        async fn handle(&mut self, _msg: Ping, _ctx: &mut Context<Self, Self::Reply>) -> u32 {
            self.starts.load(Ordering::SeqCst)
        }
    }

    /// Pins the exact guarantee the whole `RootSupervisor` design rests on
    /// (see the module doc's "Why the cached `ActorRef<T>` clones ... stay
    /// valid" section): an `ActorRef` taken *before* a supervised restart
    /// still successfully delivers messages *after* one, and reaches the
    /// new instance (not a dead mailbox). If a future kameo upgrade changes
    /// the actor-id/mailbox-sender reuse this relies on, this test fails
    /// loudly instead of every cached `ActorRef<DownloadSupervisor>` in
    /// production silently going dead on the first restart.
    #[tokio::test]
    async fn killed_child_actor_ref_survives_restart() {
        let root = ToyRoot::spawn(ToyRoot);
        let starts = Arc::new(AtomicU32::new(0));

        let child = ToyChild::supervise_with(&root, {
            let starts = starts.clone();
            move || ToyChild {
                starts: starts.clone(),
            }
        })
        .restart_policy(RestartPolicy::Transient)
        .restart_limit(5, Duration::from_secs(5))
        .spawn()
        .await;

        // The clone under test: taken once, held across the restart, used
        // both before and after. This mirrors how `hof-api`'s `AppState`
        // and friends hold a single long-lived `ActorRef<DownloadSupervisor>`
        // clone for the life of the process.
        let held_ref = child.clone();

        assert_eq!(
            held_ref.ask(Ping).await.expect("ping before kill"),
            1,
            "expected exactly one on_start before any restart"
        );

        held_ref.kill();

        // Deliberately NOT waiting for `is_alive() == false` here before
        // waiting for `true`: `restart_and_wait`'s doc comment (and
        // `kill_then_check_*` below) pin why that transition is not
        // reliably observable for a restart kameo actually performs — the
        // mailbox receiver is handed straight to the reborn instance
        // without ever being fully dropped. `starts` (this test's own
        // independent signal, outside kameo's `is_alive()` bookkeeping
        // entirely) is the trustworthy way to detect "has a restart
        // actually happened" here.
        let restart_by = Instant::now() + Duration::from_secs(5);
        while starts.load(Ordering::SeqCst) < 2 && Instant::now() < restart_by {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            starts.load(Ordering::SeqCst),
            2,
            "child was not automatically restarted within 5s of being killed"
        );
        assert!(
            held_ref.is_alive(),
            "child should be alive again once the restart has landed"
        );

        // The pre-restart clone still delivers, and reaches the NEW
        // instance (start count 2) rather than a stale or dead mailbox.
        let starts_after = held_ref.ask(Ping).await.expect("ping after restart");
        assert_eq!(
            starts_after, 2,
            "the held ActorRef must route to the respawned instance"
        );
    }

    /// Pins the specific race `restart_and_wait` in the parent module must
    /// not fall for: `is_alive()` read in the same instant as `kill()` (no
    /// `.await` in between) is stale and reads `true` regardless of the
    /// eventual outcome — including for a restart about to be declined.
    /// Naively returning `Ok` on the first `true` reading would report
    /// success for restarts that are actually about to fail. A settle delay
    /// before the first real read (`KILL_SETTLE_DELAY`) is what makes the
    /// two outcomes distinguishable at all.
    #[tokio::test]
    async fn immediate_is_alive_after_kill_is_stale() {
        let root = ToyRoot::spawn(ToyRoot);
        let starts = Arc::new(AtomicU32::new(0));

        let child = ToyChild::supervise_with(&root, {
            let starts = starts.clone();
            move || ToyChild {
                starts: starts.clone(),
            }
        })
        .restart_policy(RestartPolicy::Transient)
        .restart_limit(0, Duration::from_secs(30)) // budget already exhausted
        .spawn()
        .await;

        assert_eq!(child.ask(Ping).await.expect("ping before kill"), 1);

        child.kill();
        // No `.await` between `kill()` and this read.
        assert!(
            child.is_alive(),
            "is_alive() immediately after kill() is expected to still read \
             stale pre-kill state, even though this restart will be declined"
        );

        // The settle delay `restart_and_wait` uses is enough to observe the
        // real (declined) outcome.
        tokio::time::sleep(KILL_SETTLE_DELAY).await;
        assert!(
            !child.is_alive(),
            "after the settle delay, a declined restart must read is_alive() == false"
        );
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "a declined restart must not have run on_start again"
        );
    }
}
