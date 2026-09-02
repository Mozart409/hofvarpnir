//! Runtime-mutable configuration: resolution and propagation.
#![deny(clippy::arithmetic_side_effects, clippy::string_slice)]

use std::time::Duration;

use chrono::{DateTime, Utc};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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
}
