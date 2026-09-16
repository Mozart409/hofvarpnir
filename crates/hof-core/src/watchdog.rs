//! Self-healing watchdog: the last line of defense above `RootSupervisor`.
//!
//! # Why this exists
//!
//! `RootSupervisor` restarts a crashed actor in-process, up to a budget
//! (`RESTART_LIMIT` restarts per `RESTART_WINDOW` — see
//! `actors::root_supervisor`). If a child *exhausts* that budget, kameo
//! unlinks it permanently: `ActorHealthReport.unrecoverable` becomes `true`
//! and no in-process restart can ever revive it. Nothing before this module
//! did anything about that case — the process would sit there alive,
//! answering HTTP requests, with (say) downloading permanently dead, for as
//! long as the container kept running. That is a smaller-scale repeat of
//! the exact 27-hour incident this whole branch exists to fix: a healthcheck
//! can *report* unhealthy forever, but `containers/compose.yml`'s
//! `restart: unless-stopped` only restarts the container when the *process*
//! exits (see the comment on that policy there). A container that never
//! exits never gets restarted, no matter how loudly `/api/health` complains.
//!
//! So once in-process recovery is exhausted, the only actuator left is the
//! process terminating itself and letting `restart: unless-stopped` bring it
//! back with a clean slate. That is what this module does: poll
//! [`GetActorHealth`] on a slow cadence, and if something is
//! `unrecoverable`, self-exit — subject to the safety rails below.
//!
//! # Why the trip condition is `unrecoverable` and nothing else
//!
//! It would be tempting to also trip on "an actor is currently down" (mid
//! in-process restart) or on the `DownloadSupervisor`'s own DB-unavailability
//! backoff (see `actors::download_supervisor`). Both are wrong:
//!
//! - An actor that is down but *not* `unrecoverable` is, by definition,
//!   about to be restarted by kameo itself — that is the entire point of
//!   `RestartPolicy::Transient` with a restart budget. Tripping on it would
//!   turn an ordinary, already-handled restart into an unnecessary process
//!   exit.
//! - The DB backoff exists precisely so a transient Postgres fault no longer
//!   kills an actor at all (that fix is the other half of this branch). If
//!   the watchdog treated backoff as a trip condition, it would reintroduce
//!   the 27-hour bug's cause through a side door: a database blip would
//!   again cascade into a process restart, just via `watchdog` instead of
//!   via an actor crash. `unrecoverable` is the one signal that means
//!   in-process recovery has *actually* been exhausted — nothing upstream of
//!   it (a slow query, a pool timeout, a single crash under budget) can set
//!   it on its own.
//!
//! # Why the policy lives here and not in `RootSupervisor`
//!
//! `RootSupervisor` is deliberately the one thing in this codebase that
//! cannot die (`on_start`'s `Error = Infallible`, no IO, no panicking
//! operation — see its module doc). Teaching it to decide "should the whole
//! process exit" would mean teaching it to run a DB query
//! ([`count_activity_events_since`]) and call [`std::process::exit`], both
//! of which are exactly the kind of fallible, consequential operations that
//! module goes out of its way to avoid. Keeping the policy in a separate,
//! ordinary (fallible, panic-permitted) task means a bug in *this* module
//! can, at worst, fail to self-heal — it can never take down the supervisor
//! that every other actor's liveness depends on.
//!
//! # The crash-loop guard
//!
//! Self-exiting is a blunt instrument: if the *cause* of the
//! unrecoverability is not transient (a bad migration, a config error, a
//! genuinely broken dependency), restarting the container just reproduces
//! the same failure a restart-budget's worth of times later, forever. Two
//! independent guards bound the damage:
//!
//! - **Minimum uptime** ([`MIN_UPTIME_BEFORE_EXIT`]): refuse to self-exit
//!   until the process has been up for a while. A process that is not even
//!   ten minutes old is far more likely to be mid-startup-race than
//!   genuinely wedged.
//! - **Exit budget** ([`MAX_SELF_EXITS_PER_HOUR`]): once past the minimum
//!   uptime, only self-exit if fewer than this many self-exits have already
//!   happened in the trailing hour. At or above the limit, stay up degraded
//!   and log loudly instead of exiting again — an operator paged by the
//!   *first* self-restart is a much better outcome than a silent infinite
//!   crash loop that never surfaces anywhere.
//!
//! # Why the exit budget is persisted, and why an unreadable budget refuses to exit
//!
//! The budget must survive the very restarts it is counting, so it cannot
//! live in process memory — the counter would reset to zero on every
//! self-exit, making the "3 per hour" limit meaningless (it would allow one
//! exit, forever, every time, since the fresh process never remembers the
//! previous one). It is persisted by reusing the activity log: a
//! `SelfRestart` event is written immediately before every self-exit (see
//! [`ActivityEventType::SelfRestart`]), and the budget check is "how many
//! `SelfRestart` events landed in the last hour" ([`count_activity_events_since`]).
//! This also makes every self-restart visible in the UI activity timeline
//! for free — no separate reporting mechanism needed.
//!
//! Reading that count is itself a database query, which can fail for the
//! same reasons anything else touching Postgres can fail. If it fails, this
//! module refuses to self-exit rather than guessing. The alternative —
//! treating "budget unknown" as "budget available" — would mean a database
//! outage (the *exact* fault class that caused the original incident) could
//! cause the watchdog to self-exit on an unbounded loop, unable to ever read
//! how many times it already has. Refusing to act when the safety check
//! itself cannot be performed is the conservative default in both
//! directions this module cares about: never exit on a DB fault
//! (`unrecoverable`-only trip condition, above) and never exit when the exit
//! budget can't be verified (this).
//!
//! # Shutdown: bounded drain, then exit
//!
//! A self-exit still tries to let in-flight downloads finish first, by
//! reusing the existing [`DrainToken`] mechanism (`startup::spawn_drain_watcher`
//! already runs continuously) rather than yanking the process out from under
//! active work. But the *operator* drain timeout
//! (`EffectiveSettings::drain_timeout`, up to 30 minutes by default) is far
//! too long for a process that is, by definition, already broken — waiting
//! that long would just extend the outage this module exists to shorten. So
//! the watchdog's own drain is capped at [`MAX_WATCHDOG_DRAIN`] regardless of
//! what the operator setting is. And if the actor doing the work is itself
//! one of the unrecoverable ones, the drain returns almost immediately
//! anyway: `live_quiescence_counts` in `startup.rs` already treats an
//! unreachable supervisor/scheduler as quiescent (see its doc comment,
//! "Drain watcher could not reach supervisor; treating as quiescent"), so a
//! bounded drain against a dead actor does not hang for the full cap either.

use std::time::Duration;

use chrono::Utc;
use kameo::actor::ActorRef;
use sqlx::PgPool;
use tokio::time::Instant;
use tracing::{error, warn};

use crate::actors::root_supervisor::{ActorHealthReport, GetActorHealth, RootSupervisor};
use crate::db::{self, count_activity_events_since};
use crate::domain::activity::{ActivityEventType, ActivitySeverity};
use crate::liveness::LivenessFlag;
use crate::runtime_config::{DrainToken, RuntimeConfig};

/// How often the watchdog polls the root supervisor's health.
///
/// Deliberately slow: this is a coarse last-resort safety net layered above
/// kameo's own in-process supervision, not a substitute for it, and not a
/// liveness probe in its own right (`/api/health/live` reads the
/// [`LivenessFlag`] this task writes, with no mailbox round trip of its own
/// — see that module's doc comment).
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Crash-loop guard: refuse to self-exit until the process has been up at
/// least this long. See the module doc's "crash-loop guard" section.
const MIN_UPTIME_BEFORE_EXIT: Duration = Duration::from_mins(10);

/// Crash-loop guard: self-exits allowed in the trailing hour before the
/// watchdog gives up on self-healing this cycle and stays up degraded
/// instead. See the module doc's "crash-loop guard" section.
const MAX_SELF_EXITS_PER_HOUR: u32 = 3;

/// Upper bound on the drain this watchdog performs before exiting,
/// regardless of the operator-configured `drain_timeout`. See the module
/// doc's "Shutdown: bounded drain, then exit" section.
const MAX_WATCHDOG_DRAIN: Duration = Duration::from_mins(1);

/// Non-zero: `containers/compose.yml`'s `restart: unless-stopped` restarts
/// the container on any process exit regardless of code, but a distinct
/// non-zero code keeps this self-exit distinguishable from a clean shutdown
/// in process logs and exit-code metrics.
const WATCHDOG_EXIT_CODE: i32 = 1;

/// The watchdog's self-exit policy, decided fresh on every poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogDecision {
    /// Nothing is unrecoverable. Steady state; no action.
    Fine,
    /// Something is unrecoverable, but self-exiting is not safe right now:
    /// the process is too young, the recent self-exit budget is spent, or
    /// the budget could not be read at all. Stay up degraded and log loudly.
    StayDegraded,
    /// Something is unrecoverable, the process has cleared the minimum
    /// uptime, and the recent self-exit budget has room. Self-terminate.
    Exit,
}

/// Decide what the watchdog should do this cycle.
///
/// Pure and synchronous by design: every input the policy depends on is a
/// plain value (no `ActorRef`, no `PgPool`, no wall clock read inside the
/// function), so every threshold above is unit-testable without spawning
/// actors, hitting a database, or mocking time — see the tests below.
///
/// # Arguments
///
/// * `uptime` — wall-clock time since process start.
/// * `recent_self_exits` — how many `SelfRestart` activity events landed in
///   the trailing hour, or `None` if that count could not be read (a DB
///   fault). `None` is handled identically to "budget exhausted" — see the
///   module doc's fail-safe section for why.
/// * `any_unrecoverable` — whether [`GetActorHealth`] reported at least one
///   actor with `unrecoverable == true`. The *only* trip condition; see the
///   module doc.
#[must_use]
pub fn evaluate(
    uptime: Duration,
    recent_self_exits: Option<u32>,
    any_unrecoverable: bool,
) -> WatchdogDecision {
    if !any_unrecoverable {
        return WatchdogDecision::Fine;
    }
    if uptime < MIN_UPTIME_BEFORE_EXIT {
        return WatchdogDecision::StayDegraded;
    }
    match recent_self_exits {
        Some(count) if count < MAX_SELF_EXITS_PER_HOUR => WatchdogDecision::Exit,
        // Covers both "budget exhausted" (`Some(count >= MAX)`) and "budget
        // unreadable" (`None`) — see the module doc for why both fail safe
        // to the same, non-exiting outcome.
        _ => WatchdogDecision::StayDegraded,
    }
}

/// Spawn the watchdog task.
///
/// Runs for the lifetime of the process: polls [`GetActorHealth`] on
/// `root_supervisor` every [`POLL_INTERVAL`], updates `liveness` so
/// `/api/health/live` reflects the result with no mailbox round trip of its
/// own, and — only when [`evaluate`] says [`WatchdogDecision::Exit`] —
/// performs a bounded drain and calls [`std::process::exit`]. See the module
/// doc for the full design rationale.
pub fn spawn(
    pool: PgPool,
    root_supervisor: ActorRef<RootSupervisor>,
    runtime_config: RuntimeConfig,
    drain: DrainToken,
    liveness: LivenessFlag,
) {
    tokio::spawn(async move {
        // Approximates true process start closely enough: this task is
        // spawned from `startup::initialize`, which runs before the HTTP
        // listener even binds.
        let started_at = Instant::now();

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let reports = match root_supervisor.ask(GetActorHealth).await {
                Ok(reports) => reports,
                Err(error) => {
                    // The root supervisor cannot itself die (see its module
                    // doc) — an ask failure here means something upstream
                    // (an overloaded runtime, a process mid-shutdown) is
                    // wrong in a way this task cannot diagnose. Skip this
                    // cycle rather than guess; the flag keeps its last known
                    // value and the next poll tries again.
                    warn!(
                        %error,
                        "Watchdog could not reach root supervisor; skipping this cycle"
                    );
                    continue;
                }
            };

            let any_unrecoverable = reports.iter().any(|report| report.unrecoverable);
            liveness.set_alive(!any_unrecoverable);

            let recent_self_exits = if any_unrecoverable {
                self_exit_budget(&pool).await
            } else {
                // Not consulted by `evaluate` when nothing is unrecoverable
                // (see its `Fine` arm) — skip the query.
                None
            };

            let uptime = started_at.elapsed();
            match evaluate(uptime, recent_self_exits, any_unrecoverable) {
                WatchdogDecision::Fine => {}
                WatchdogDecision::StayDegraded => {
                    let unrecoverable_actors: Vec<&str> = reports
                        .iter()
                        .filter(|report| report.unrecoverable)
                        .map(|report| report.actor.as_str())
                        .collect();
                    error!(
                        ?uptime,
                        ?recent_self_exits,
                        actors = ?unrecoverable_actors,
                        "Watchdog: unrecoverable actor(s) detected but a self-restart is \
                         not safe right now (crash-loop guard); staying up degraded. \
                         Manual intervention (POST /api/v1/system/actors/{{name}}/restart, \
                         or a manual process restart) is required."
                    );
                }
                WatchdogDecision::Exit => {
                    self_exit(&reports, &pool, &runtime_config, &drain).await;
                }
            }
        }
    });
}

/// Read the self-exit budget: how many `SelfRestart` events landed in the
/// trailing hour. `None` means the read itself failed — see the module doc
/// for why that fails safe (does not exit) rather than assuming the budget
/// is available.
async fn self_exit_budget(pool: &PgPool) -> Option<u32> {
    let since = Utc::now() - chrono::Duration::hours(1);
    match count_activity_events_since(pool, ActivityEventType::SelfRestart, since).await {
        // A count this large overflowing `u32` is not a realistic outcome
        // (it would mean billions of self-restarts in one hour), but
        // saturating rather than panicking keeps this on the safe
        // (over-the-budget, non-exiting) side if it somehow ever happened.
        Ok(count) => Some(u32::try_from(count).unwrap_or(u32::MAX)),
        Err(error) => {
            error!(
                %error,
                "Watchdog could not read the self-exit budget; staying up degraded \
                 (fail-safe: an unreadable budget is never treated as an available one)"
            );
            None
        }
    }
}

/// Log, record, drain, and exit. Only reachable from [`WatchdogDecision::Exit`].
async fn self_exit(
    reports: &[ActorHealthReport],
    pool: &PgPool,
    runtime_config: &RuntimeConfig,
    drain: &DrainToken,
) {
    let unrecoverable_actors: Vec<&str> = reports
        .iter()
        .filter(|report| report.unrecoverable)
        .map(|report| report.actor.as_str())
        .collect();

    error!(
        actors = ?unrecoverable_actors,
        "Watchdog: self-exiting so `restart: unless-stopped` brings the container back \
         with a clean slate. Draining in-flight work first (bounded)."
    );

    // Written *before* exiting — see the module doc's "why the exit budget
    // is persisted" section. `log_activity` is fire-and-forget (logs and
    // swallows its own error) precisely so a DB fault here cannot prevent
    // the exit this event exists to make visible in the timeline.
    db::log_activity(
        pool,
        ActivityEventType::SelfRestart,
        ActivitySeverity::Error,
        &format!(
            "Watchdog self-restart: {} exhausted its in-process restart budget \
             and could not be recovered; exiting for a container restart",
            unrecoverable_actors.join(", ")
        ),
        None,
        None,
        None,
    )
    .await;

    // Bounded drain: reuse the existing `DrainToken` machinery
    // (`startup::spawn_drain_watcher` is already running and will observe
    // this `begin()`), capped well below the operator's own drain_timeout —
    // see the module doc for why.
    let operator_timeout = runtime_config.current().drain_timeout.value;
    drain.begin(Utc::now(), operator_timeout.min(MAX_WATCHDOG_DRAIN));
    drain.wait_complete().await;

    // The whole point of this module. `restart: unless-stopped` recovers a
    // container whose process *exits*; it does nothing for a process that
    // stays up with a dead actor inside it, which is precisely how the
    // 2026-09-15 outage ran for 27 hours. Exiting is the only actuator that
    // turns an unrecoverable actor back into a running one, and it is
    // reached only after `evaluate` has cleared the crash-loop guard.
    #[allow(clippy::exit)]
    std::process::exit(WATCHDOG_EXIT_CODE);
}

#[cfg(test)]
mod tests {
    use super::{MAX_SELF_EXITS_PER_HOUR, MIN_UPTIME_BEFORE_EXIT, WatchdogDecision, evaluate};
    use std::time::Duration;

    #[test]
    fn healthy_system_is_fine_regardless_of_other_inputs() {
        assert_eq!(
            evaluate(Duration::ZERO, None, false),
            WatchdogDecision::Fine
        );
        assert_eq!(
            evaluate(Duration::from_secs(999_999), Some(0), false),
            WatchdogDecision::Fine
        );
        assert_eq!(
            evaluate(Duration::from_secs(999_999), Some(999), false),
            WatchdogDecision::Fine,
            "budget state must not matter when nothing is unrecoverable"
        );
    }

    #[test]
    fn below_min_uptime_never_exits() {
        assert_eq!(
            evaluate(Duration::ZERO, Some(0), true),
            WatchdogDecision::StayDegraded
        );
        assert_eq!(
            evaluate(
                MIN_UPTIME_BEFORE_EXIT.saturating_sub(Duration::from_secs(1)),
                Some(0),
                true
            ),
            WatchdogDecision::StayDegraded,
            "one second short of the minimum uptime must still refuse to exit"
        );
    }

    #[test]
    fn at_min_uptime_with_budget_available_exits() {
        assert_eq!(
            evaluate(MIN_UPTIME_BEFORE_EXIT, Some(0), true),
            WatchdogDecision::Exit,
            "the minimum uptime boundary itself is eligible, not just strictly past it"
        );
    }

    #[test]
    fn budget_exhausted_stays_degraded() {
        assert_eq!(
            evaluate(MIN_UPTIME_BEFORE_EXIT, Some(MAX_SELF_EXITS_PER_HOUR), true),
            WatchdogDecision::StayDegraded,
            "hitting the limit exactly must not exit"
        );
        assert_eq!(
            evaluate(
                MIN_UPTIME_BEFORE_EXIT,
                Some(MAX_SELF_EXITS_PER_HOUR + 1),
                true
            ),
            WatchdogDecision::StayDegraded
        );
    }

    #[test]
    fn budget_just_under_the_limit_exits() {
        assert_eq!(
            evaluate(
                MIN_UPTIME_BEFORE_EXIT,
                Some(MAX_SELF_EXITS_PER_HOUR - 1),
                true
            ),
            WatchdogDecision::Exit
        );
    }

    #[test]
    fn unreadable_budget_does_not_exit() {
        assert_eq!(
            evaluate(MIN_UPTIME_BEFORE_EXIT, None, true),
            WatchdogDecision::StayDegraded,
            "a DB fault reading the budget must fail safe, not be treated as budget available"
        );
        assert_eq!(
            evaluate(Duration::from_secs(999_999), None, true),
            WatchdogDecision::StayDegraded,
            "unreadable budget must refuse to exit no matter how long the process has been up"
        );
    }
}
