//! Runtime-mutable configuration: resolution and propagation.
#![deny(clippy::arithmetic_side_effects, clippy::string_slice)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use color_eyre::eyre::Result;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::watch;
use tracing::{error, info, warn};

pub use crate::config::EnvOverrides;
use crate::db::RuntimeSettingsRow;

pub const DEFAULT_MAX_CONCURRENT: u32 = 3;
pub const DEFAULT_MAX_INDEXERS_PER_TICK: u32 = 5;
pub const DEFAULT_RATE_LIMIT_DELAY_SECS: u64 = 5;
pub const DEFAULT_CHECK_INTERVAL_SECS: u64 = 60;
pub const DEFAULT_CLEANUP_INTERVAL_SECS: u64 = 60 * 60 * 3;
pub const DEFAULT_DRAIN_TIMEOUT_SECS: u64 = 1800;

/// Whole-microsecond Unix timestamp for [`indefinite_pause`]:
/// 9999-12-31T23:59:59Z.
const INDEFINITE_PAUSE_MICROS: i64 = 253_402_300_799_000_000;

/// A finite sentinel timestamp meaning "paused indefinitely".
///
/// `DateTime::<Utc>::MAX_UTC` cannot serve this role: it carries
/// sub-microsecond (nanosecond) precision that Postgres's
/// microsecond-resolution `timestamptz` truncates away on write. A value
/// written as `MAX_UTC` and read back from the database no longer equals
/// `MAX_UTC`, so an equality guard against it silently stops matching after
/// a single round trip. `indefinite_pause` is instead built entirely from a
/// whole microsecond count, so it round-trips through Postgres
/// bit-for-bit.
#[must_use]
pub fn indefinite_pause() -> DateTime<Utc> {
    // `from_timestamp_micros` only returns `None` on over/underflow of
    // chrono's internal representable range; `INDEFINITE_PAUSE_MICROS`
    // (year 9999) is nowhere near that boundary, so the fallback below is
    // unreachable in practice. `unwrap_or` (not `unwrap`/`expect`) keeps
    // this off the banned-panic surface; the fallback value is arbitrary
    // since it is never actually returned.
    DateTime::from_timestamp_micros(INDEFINITE_PAUSE_MICROS).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

/// Which layer supplied a resolved value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    Default,
    Env,
    Database,
}

/// A resolved value together with the layer it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Resolved<T> {
    pub value: T,
    pub provenance: Provenance,
}

/// Fully-resolved settings. Actors consume this and never see the layering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSettings {
    pub indexing_paused_until: Option<DateTime<Utc>>,
    pub downloads_paused_until: Option<DateTime<Utc>>,
    pub max_concurrent_downloads: Resolved<u32>,
    pub max_indexers_per_tick: Resolved<u32>,
    pub rate_limit_delay: Resolved<Duration>,
    pub check_interval: Resolved<Duration>,
    pub cleanup_interval: Resolved<Duration>,
    pub drain_timeout: Resolved<Duration>,
}

impl EffectiveSettings {
    #[must_use]
    pub fn indexing_paused(&self, now: DateTime<Utc>) -> bool {
        self.indexing_paused_until.is_some_and(|t| t > now)
    }

    #[must_use]
    pub fn downloads_paused(&self, now: DateTime<Utc>) -> bool {
        self.downloads_paused_until.is_some_and(|t| t > now)
    }

    /// The nearest future pause expiry, if any.
    ///
    /// Returns `None` for an indefinite pause ([`indefinite_pause`]),
    /// because such a pause never lapses on its own and must not arm a timer.
    #[must_use]
    pub fn next_pause_deadline(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        [self.indexing_paused_until, self.downloads_paused_until]
            .into_iter()
            .flatten()
            .filter(|t| *t > now && *t != indefinite_pause())
            .min()
    }
}

/// Resolve database row over environment over compiled-in defaults.
#[must_use]
pub fn resolve(row: &RuntimeSettingsRow, env: &EnvOverrides) -> EffectiveSettings {
    EffectiveSettings {
        indexing_paused_until: row.indexing_paused_until,
        downloads_paused_until: row.downloads_paused_until,
        max_concurrent_downloads: pick_u32(
            row.max_concurrent_downloads,
            env.max_concurrent_downloads,
            DEFAULT_MAX_CONCURRENT,
        ),
        max_indexers_per_tick: pick_u32(
            row.max_indexers_per_tick,
            env.max_indexers_per_tick,
            DEFAULT_MAX_INDEXERS_PER_TICK,
        ),
        rate_limit_delay: pick_secs(
            row.rate_limit_delay_secs,
            env.rate_limit_delay_secs,
            DEFAULT_RATE_LIMIT_DELAY_SECS,
        ),
        check_interval: pick_secs(
            row.check_interval_secs,
            env.check_interval_secs,
            DEFAULT_CHECK_INTERVAL_SECS,
        ),
        cleanup_interval: pick_secs(
            row.cleanup_interval_secs,
            env.cleanup_interval_secs,
            DEFAULT_CLEANUP_INTERVAL_SECS,
        ),
        drain_timeout: pick_secs(
            row.drain_timeout_secs,
            env.drain_timeout_secs,
            DEFAULT_DRAIN_TIMEOUT_SECS,
        ),
    }
}

fn pick_u32(db: Option<i32>, env: Option<u32>, default: u32) -> Resolved<u32> {
    if let Some(v) = db.and_then(|v| u32::try_from(v).ok()) {
        return Resolved {
            value: v,
            provenance: Provenance::Database,
        };
    }
    env.map_or(
        Resolved {
            value: default,
            provenance: Provenance::Default,
        },
        |v| Resolved {
            value: v,
            provenance: Provenance::Env,
        },
    )
}

fn pick_secs(db: Option<i32>, env: Option<u64>, default: u64) -> Resolved<Duration> {
    let (secs, provenance) = if let Some(v) = db.and_then(|v| u64::try_from(v).ok()) {
        (v, Provenance::Database)
    } else if let Some(v) = env {
        (v, Provenance::Env)
    } else {
        (default, Provenance::Default)
    };
    Resolved {
        value: Duration::from_secs(secs),
        provenance,
    }
}

/// Postgres NOTIFY channel carrying settings-change signals.
const NOTIFY_CHANNEL: &str = "runtime_settings_changed";

/// How long to wait before retrying after the listener drops its connection.
const LISTENER_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Saturating "how long until `deadline`", never negative.
///
/// `unchecked_time_subtraction` is denied workspace-wide, so this uses
/// `signed_duration_since` and clamps a past deadline to zero.
#[must_use]
pub fn sleep_duration_until(deadline: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    deadline
        .signed_duration_since(now)
        .to_std()
        .unwrap_or(Duration::ZERO)
}

/// Handle to the current runtime settings.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    tx: watch::Sender<Arc<EffectiveSettings>>,
    pool: PgPool,
    env: EnvOverrides,
}

impl RuntimeConfig {
    /// Load settings once and build the handle.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial settings read fails.
    pub async fn new(pool: PgPool, env: EnvOverrides) -> Result<Self> {
        let row = crate::db::get_runtime_settings(&pool).await?;
        let (tx, _) = watch::channel(Arc::new(resolve(&row, &env)));
        Ok(Self { tx, pool, env })
    }

    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Arc<EffectiveSettings>> {
        self.tx.subscribe()
    }

    #[must_use]
    pub fn current(&self) -> Arc<EffectiveSettings> {
        self.tx.borrow().clone()
    }

    async fn reload(&self) {
        match crate::db::get_runtime_settings(&self.pool).await {
            Ok(row) => {
                let next = Arc::new(resolve(&row, &self.env));
                // `send_replace` so a value is published even with no subscribers.
                self.tx.send_replace(next);
            }
            Err(error) => error!(%error, "Failed to reload runtime settings"),
        }
    }

    /// Spawn the listener. It republishes on NOTIFY and when a pause lapses.
    pub fn spawn_listener(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let mut listener = match PgListener::connect_with(&self.pool).await {
                    Ok(l) => l,
                    Err(error) => {
                        error!(%error, "Runtime settings listener failed to connect; retrying");
                        tokio::time::sleep(LISTENER_RETRY_DELAY).await;
                        continue;
                    }
                };
                if let Err(error) = listener.listen(NOTIFY_CHANNEL).await {
                    error!(%error, "Failed to LISTEN on runtime settings channel; retrying");
                    tokio::time::sleep(LISTENER_RETRY_DELAY).await;
                    continue;
                }

                // NOTIFY is fire-and-forget: anything sent while we were
                // disconnected is lost, so always full-resync on (re)connect.
                self.reload().await;
                info!("Runtime settings listener connected");

                loop {
                    // Recompute the deadline on EVERY iteration. If an operator
                    // shortens a 7-day pause to 1 hour, the notify arm wakes us
                    // and the stale deadline must be dropped and re-armed —
                    // otherwise the change would silently do nothing for days.
                    let deadline = self.current().next_pause_deadline(Utc::now());

                    let notified = if let Some(deadline) = deadline {
                        let wait = sleep_duration_until(deadline, Utc::now());
                        tokio::select! {
                            n = listener.recv() => n.is_ok(),
                            () = tokio::time::sleep(wait) => {
                                // Pause lapsed: republish so consumers re-read.
                                self.reload().await;
                                continue;
                            }
                        }
                    } else {
                        listener.recv().await.is_ok()
                    };

                    if notified {
                        self.reload().await;
                    } else {
                        warn!("Runtime settings listener disconnected; reconnecting");
                        break;
                    }
                }
            }
        })
    }
}

/// Process-local drain state. Deliberately NOT persisted (see ADR-0004): a
/// persisted drain would leave a restarted container refusing all work,
/// wedged, with no visible cause.
///
/// Built on `tokio::sync::watch` rather than `tokio::sync::Notify`:
/// `Notify::notify_waiters` wakes only waiters already parked and stores no
/// permit, so draining an already-idle instance — the common case, not a
/// corner case — fires the "complete" signal before `main`'s select arm is
/// ever polled and the wakeup is silently dropped, hanging shutdown forever.
/// `watch::Sender` retains its last value, so a receiver that starts
/// watching after the fact still observes it.
#[derive(Debug, Clone)]
pub struct DrainToken {
    started: Arc<watch::Sender<Option<DateTime<Utc>>>>,
    complete: Arc<watch::Sender<bool>>,
}

impl Default for DrainToken {
    fn default() -> Self {
        Self::new()
    }
}

impl DrainToken {
    #[must_use]
    pub fn new() -> Self {
        let (started, _) = watch::channel(None);
        let (complete, _) = watch::channel(false);
        Self {
            started: Arc::new(started),
            complete: Arc::new(complete),
        }
    }

    /// Begin draining. Idempotent within a process: the first start time
    /// wins, so a repeated call cannot extend the drain deadline.
    ///
    /// `send_if_modified` is infallible (unlike the poisoned-lock recovery
    /// a `RwLock`-based version would need), so there is nothing here to
    /// `unwrap`.
    pub fn begin(&self, now: DateTime<Utc>) {
        self.started.send_if_modified(|current| {
            if current.is_some() {
                false
            } else {
                *current = Some(now);
                true
            }
        });
    }

    #[must_use]
    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        *self.started.borrow()
    }

    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.started_at().is_some()
    }

    /// Absolute deadline after which shutdown proceeds regardless.
    #[must_use]
    pub fn deadline(&self, timeout: Duration) -> Option<DateTime<Utc>> {
        let started = self.started_at()?;
        let delta = chrono::Duration::from_std(timeout).ok()?;
        started.checked_add_signed(delta)
    }

    /// Signal that draining finished; wakes `main`'s shutdown arm.
    pub fn signal_complete(&self) {
        self.complete.send_replace(true);
    }

    /// Await drain completion — this is what `main.rs`'s third select arm uses.
    pub async fn wait_complete(&self) {
        // `wait_for` errs only once every sender has dropped; `self.complete`
        // keeps one alive for as long as this token (or a clone) exists, so
        // that arm is unreachable here. Nothing meaningful to do with the
        // error besides not propagating it further.
        let _ = self
            .complete
            .subscribe()
            .wait_for(|complete| *complete)
            .await;
    }

    /// Await drain start. The drain watcher blocks here until an operator
    /// (or a test) calls [`begin`](Self::begin).
    pub async fn wait_started(&self) {
        // See `wait_complete` above for why the `Err` arm is unreachable in
        // practice but still handled rather than unwrapped.
        let _ = self
            .started
            .subscribe()
            .wait_for(std::option::Option::is_some)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_row() -> RuntimeSettingsRow {
        RuntimeSettingsRow::default()
    }
    fn no_env() -> EnvOverrides {
        EnvOverrides::default()
    }

    #[test]
    fn falls_back_to_code_default() {
        let s = resolve(&empty_row(), &no_env());
        assert_eq!(s.max_concurrent_downloads.value, DEFAULT_MAX_CONCURRENT);
        assert_eq!(s.max_concurrent_downloads.provenance, Provenance::Default);
    }

    #[test]
    fn env_overrides_default() {
        let env = EnvOverrides {
            max_concurrent_downloads: Some(7),
            ..no_env()
        };
        let s = resolve(&empty_row(), &env);
        assert_eq!(s.max_concurrent_downloads.value, 7);
        assert_eq!(s.max_concurrent_downloads.provenance, Provenance::Env);
    }

    #[test]
    fn database_overrides_env() {
        let env = EnvOverrides {
            max_concurrent_downloads: Some(7),
            ..no_env()
        };
        let row = RuntimeSettingsRow {
            max_concurrent_downloads: Some(2),
            ..empty_row()
        };
        let s = resolve(&row, &env);
        assert_eq!(s.max_concurrent_downloads.value, 2);
        assert_eq!(s.max_concurrent_downloads.provenance, Provenance::Database);
    }

    #[test]
    fn null_pause_is_not_paused() {
        let s = resolve(&empty_row(), &no_env());
        assert!(!s.indexing_paused(Utc::now()));
    }

    // This test builds a deadline with plain `Utc::now() + chrono::Duration`
    // arithmetic. The file-level deny above exists to keep *production*
    // deadline arithmetic checked; this fixture is not production code, so
    // it is scoped out rather than rewritten into checked-arithmetic
    // ceremony.
    #[test]
    #[allow(clippy::arithmetic_side_effects)]
    fn future_pause_is_paused_and_yields_deadline() {
        let until = Utc::now() + chrono::Duration::hours(1);
        let row = RuntimeSettingsRow {
            indexing_paused_until: Some(until),
            ..empty_row()
        };
        let s = resolve(&row, &no_env());
        assert!(s.indexing_paused(Utc::now()));
        assert_eq!(s.next_pause_deadline(Utc::now()), Some(until));
    }

    // See `future_pause_is_paused_and_yields_deadline` above for why this
    // fixture's `Utc::now() - chrono::Duration` arithmetic is scoped out
    // rather than removing the file-level deny.
    #[test]
    #[allow(clippy::arithmetic_side_effects)]
    fn elapsed_pause_is_not_paused() {
        let past = Utc::now() - chrono::Duration::hours(1);
        let row = RuntimeSettingsRow {
            indexing_paused_until: Some(past),
            ..empty_row()
        };
        let s = resolve(&row, &no_env());
        assert!(!s.indexing_paused(Utc::now()));
        assert_eq!(s.next_pause_deadline(Utc::now()), None);
    }

    #[test]
    fn indefinite_pause_is_paused_but_schedules_no_deadline() {
        let row = RuntimeSettingsRow {
            downloads_paused_until: Some(indefinite_pause()),
            ..empty_row()
        };
        let s = resolve(&row, &no_env());
        assert!(s.downloads_paused(Utc::now()));
        assert_eq!(s.next_pause_deadline(Utc::now()), None);
    }

    // See `future_pause_is_paused_and_yields_deadline` above for why this
    // fixture's `Utc::now() + chrono::Duration` arithmetic is scoped out
    // rather than removing the file-level deny.
    #[test]
    #[allow(clippy::arithmetic_side_effects)]
    fn nearest_of_two_deadlines_wins() {
        let soon = Utc::now() + chrono::Duration::minutes(10);
        let later = Utc::now() + chrono::Duration::hours(5);
        let row = RuntimeSettingsRow {
            indexing_paused_until: Some(later),
            downloads_paused_until: Some(soon),
            ..empty_row()
        };
        let s = resolve(&row, &no_env());
        assert_eq!(s.next_pause_deadline(Utc::now()), Some(soon));
    }

    // This test builds a deadline with plain `Utc::now() + chrono::Duration`
    // arithmetic. See `future_pause_is_paused_and_yields_deadline` above for
    // why this fixture is scoped out rather than removing the file-level
    // deny.
    //
    // This exercises `next_pause_deadline` and `sleep_duration_until` only —
    // it never constructs a `RuntimeConfig`, never spawns the listener, and
    // never touches the watch channel or `reload`/`send_replace`. The
    // listener/`select!`/watch machinery itself is covered by the
    // `listener_republishes_on_notify` integration test below.
    #[tokio::test(start_paused = true)]
    #[allow(clippy::arithmetic_side_effects)]
    async fn pause_deadline_computes_sleep_and_lapses() {
        let until = Utc::now() + chrono::Duration::hours(1);
        let row = RuntimeSettingsRow {
            indexing_paused_until: Some(until),
            ..RuntimeSettingsRow::default()
        };
        let settings = resolve(&row, &EnvOverrides::default());
        assert!(settings.indexing_paused(Utc::now()));

        let deadline = settings.next_pause_deadline(Utc::now()).expect("deadline");
        let wait = sleep_duration_until(deadline, Utc::now());
        assert!(wait >= Duration::from_secs(3500) && wait <= Duration::from_hours(1));

        tokio::time::sleep(wait).await;
        // After the deadline the same settings value must read as un-paused.
        assert!(!settings.indexing_paused(deadline + chrono::Duration::seconds(1)));
    }

    // See `future_pause_is_paused_and_yields_deadline` above for why this
    // fixture's `Utc::now() - chrono::Duration` arithmetic is scoped out
    // rather than removing the file-level deny.
    #[test]
    #[allow(clippy::arithmetic_side_effects)]
    fn sleep_duration_is_zero_for_elapsed_deadline() {
        let past = Utc::now() - chrono::Duration::hours(1);
        assert_eq!(sleep_duration_until(past, Utc::now()), Duration::ZERO);
    }

    // Integration tests require a running database.
    // Run with: DATABASE_URL=postgres://... cargo test -p hof-core --all-features -- --include-ignored

    /// Exercises the propagation path this task actually delivers: build a
    /// real `RuntimeConfig` against Postgres, `subscribe()`, spawn the
    /// listener, then mutate the singleton row through
    /// `patch_runtime_settings` (which fires the trigger and hence
    /// `pg_notify`) and confirm the watch receiver observes the new value.
    /// This is the keystone case `deadline_republishes_when_pause_lapses`
    /// (renamed `pause_deadline_computes_sleep_and_lapses` above) never
    /// covered, because that test never touches `RuntimeConfig`, the
    /// listener, or the watch channel at all.
    ///
    /// Deliberately not covered here: a virtual-clock deadline-expiry test
    /// driving this live listener. `tokio::time::pause` does not compose
    /// reliably with real Postgres IO (the listener's connection and the
    /// test's own queries both need real time to make progress), so forcing
    /// it would trade an honest gap for a flaky test. The deadline-expiry
    /// path stays covered by `pause_deadline_computes_sleep_and_lapses`
    /// (the pure `sleep_duration_until`/`next_pause_deadline` helpers) plus
    /// manual review of `spawn_listener`'s inner loop.
    #[tokio::test]
    #[ignore = "requires a running database (run with --include-ignored)"]
    async fn listener_republishes_on_notify() {
        let pool = crate::db::create_pool()
            .await
            .expect("Failed to create pool");
        crate::db::run_migrations(&pool)
            .await
            .expect("Failed to run migrations");

        let config = RuntimeConfig::new(pool.clone(), EnvOverrides::default())
            .await
            .expect("Failed to build RuntimeConfig");
        let mut rx = config.subscribe();
        let listener_handle = config.spawn_listener();

        let target = Duration::from_secs(1234);

        // The listener connects and LISTENs asynchronously in its spawned
        // task, so there is an unavoidable race between "the test patches
        // the row" and "the listener is ready to receive the resulting
        // NOTIFY" — NOTIFY is fire-and-forget, so a patch that lands before
        // the listener is ready is simply missed. Retry the *write*, not
        // just the read: keep re-patching until a change is observed with
        // the expected value, bounded overall so a genuine regression fails
        // fast instead of hanging the suite.
        let observed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let patch = crate::db::RuntimeSettingsPatch {
                    rate_limit_delay_secs: Some(Some(1234)),
                    // `updated_by` is `TEXT REFERENCES users (id)`, so it
                    // must be a real user id or NULL — the marker string
                    // used here previously violated that FK. `None` nulls
                    // the column (this field is not a nested `Option`, so
                    // it is unconditionally overwritten every call); fine
                    // for this test, since nothing here depends on the
                    // audit value.
                    updated_by: None,
                    ..crate::db::RuntimeSettingsPatch::default()
                };
                crate::db::patch_runtime_settings(&pool, &patch)
                    .await
                    .expect("Failed to patch runtime settings");

                if matches!(
                    tokio::time::timeout(Duration::from_millis(250), rx.changed()).await,
                    Ok(Ok(()))
                ) {
                    let settings = rx.borrow().clone();
                    if settings.rate_limit_delay.value == target {
                        break settings;
                    }
                }
            }
        })
        .await
        .expect("Timed out waiting for listener to republish settings after NOTIFY");

        assert_eq!(observed.rate_limit_delay.value, target);

        listener_handle.abort();

        // Cleanup: leave the singleton row as the other `#[ignore]`d tests
        // in this binary expect to find it.
        let cleanup = crate::db::RuntimeSettingsPatch {
            rate_limit_delay_secs: Some(None),
            updated_by: None,
            ..crate::db::RuntimeSettingsPatch::default()
        };
        crate::db::patch_runtime_settings(&pool, &cleanup)
            .await
            .expect("Failed to reset runtime settings");
    }

    #[test]
    fn drain_token_starts_not_draining() {
        let t = DrainToken::new();
        assert!(!t.is_draining());
        t.begin(Utc::now());
        assert!(t.is_draining());
    }

    // See `future_pause_is_paused_and_yields_deadline` above for why this
    // fixture's plain `DateTime + chrono::Duration` arithmetic is scoped out
    // rather than removing the file-level deny.
    #[test]
    #[allow(clippy::arithmetic_side_effects)]
    fn drain_begin_keeps_first_start_time() {
        let t = DrainToken::new();
        let first = Utc::now();
        t.begin(first);
        // A later `begin` call must not push the deadline out.
        t.begin(first + chrono::Duration::seconds(30));
        assert_eq!(t.started_at(), Some(first));
    }

    #[test]
    fn drain_deadline_is_start_plus_timeout() {
        let t = DrainToken::new();
        let now = Utc::now();
        t.begin(now);
        let timeout = Duration::from_mins(30);
        let expected = now
            .checked_add_signed(chrono::Duration::from_std(timeout).expect("valid duration"))
            .expect("no overflow");
        assert_eq!(t.deadline(timeout), Some(expected));
    }

    #[test]
    fn drain_deadline_is_none_before_begin() {
        let t = DrainToken::new();
        assert_eq!(t.deadline(Duration::from_mins(1)), None);
    }

    #[tokio::test]
    async fn drain_wait_complete_resolves_after_signal() {
        let t = DrainToken::new();
        let waiter = t.clone();
        let handle = tokio::spawn(async move { waiter.wait_complete().await });
        // Give the spawned task a chance to start waiting before signalling —
        // not required for correctness (a `watch` receiver sees a value set
        // before it subscribes too), but keeps this test honest about the
        // "wakes an already-parked waiter" case rather than only the
        // already-set-before-subscribe case exercised by
        // `drain_wait_complete_is_immediate_if_already_signalled` below.
        tokio::task::yield_now().await;
        t.signal_complete();
        handle.await.expect("waiter task panicked");
    }

    #[tokio::test]
    async fn drain_wait_complete_is_immediate_if_already_signalled() {
        let t = DrainToken::new();
        t.signal_complete();
        // Must not hang: this is exactly the lost-wakeup scenario Ruling A
        // exists to avoid (draining an already-quiescent/idle instance).
        t.wait_complete().await;
    }
}
