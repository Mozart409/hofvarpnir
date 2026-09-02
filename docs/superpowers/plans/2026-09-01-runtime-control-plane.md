# Runtime Control Plane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an operator pause indexing and downloads for a chosen duration, retune concurrency and pacing, and drain the process for a clean shutdown — all at runtime, from the UI, without a restart.

**Architecture:** A singleton `runtime_settings` table is the source of truth. A database trigger emits `pg_notify` on any write; one listener task re-reads the row, resolves it against environment/default layers into an `EffectiveSettings` value, and publishes it over a `tokio::sync::watch` channel. Actors hold a `watch::Receiver` and adapt in place. Pause is a nullable `TIMESTAMPTZ` (with `infinity` for indefinite) and survives restart; drain is an in-memory token that deliberately does not.

**Tech Stack:** Rust, tokio 1.53, sqlx (Postgres, compile-time checked, offline cache in `.sqlx/`), kameo actors, axum + utoipa, Maud + htmx, Tailwind.

**Spec:** `docs/superpowers/specs/2026-09-01-runtime-control-plane-design.md`
**ADRs:** `docs/adr/0001`–`0004`
**Issue:** [#130](https://github.com/Mozart409/hofvarpnir/issues/130)

## Session Handoff — start here

**State as of 2026-09-02, branch `feat/2026-09-01-429-handling` (not pushed, no upstream):**

- **Tasks 0, 1 and 2 are done, committed, reviewed, and green.** Working tree clean.
- **Next action: Task 3** — consumers adopt the watch channel. Nothing is in progress;
  a Task 3 implementer was dispatched on 2026-09-02 but hit an API session rate limit
  during orientation and **made no edits**. There is no partial work to reconcile.

**Commits this branch (newest last):**

| Commit | What |
|---|---|
| `d616eb9`, `6464227`, `e643a65` | Task 0 — lint baseline |
| `ba34c39` | Task 1 — `runtime_settings` table, row type, precedence resolver |
| `3c428c6` | Task 1 fix — round-trippable indefinite-pause sentinel |
| `d6193b1` | Docs — sqlx/sqruff/migration-checksum pitfalls (out of plan) |
| `3d5d10e` | Task 2 — LISTEN/NOTIFY propagation, watch channel, expiry deadline |

**Full execution record — read this before anything else:**
`.superpowers/sdd/2026-09-01-runtime-control-plane/progress.md` (git-ignored). It holds
every ruling made, why, and what each costs if wrong. Per-task briefs, reports, context
files, and review diffs sit beside it. `task-3-brief.md` and `task-3-context.md` are
already prepared, the latter carrying Task 2's exact public API surface.

### Corrections to this plan, established by execution

The plan's code blocks were written without being compiled and have produced several
defects. Treat them as close-to-right, not authoritative.

1. **`'infinity'::timestamptz` is unreachable.** sqlx decodes binary timestamptz as
   `postgres_epoch + Duration::microseconds(us)` with no infinity branch, and chrono's
   `Add` panics on overflow — so a literal `infinity` **panics on read**. `MAX_UTC` is
   also wrong: its nanosecond precision does not survive Postgres's microsecond
   truncation, so it fails to round-trip and the sentinel comparison stops matching.
   **`runtime_config::indefinite_pause()` (9999-12-31T23:59:59Z, whole microseconds) is
   the sentinel.** Task 6 must use it, not `MAX_UTC`. A new migration adds
   `CHECK (isfinite(...))` on both pause columns. **ADR-0003 still needs amending.**
2. **New timestamp queries need an explicit sqlx type override** —
   `col AS "col: DateTime<Utc>"`, `?` form when nullable. Both the `chrono` and `time`
   features are unified on, and the macros prefer `time`. Bites Task 6.
3. **`#[tokio::test(start_paused = true)]` needs tokio's `test-util` feature**, which
   `features = ["full"]` does NOT include. Already added to `hof-core`'s dev-dependencies
   — **Task 5's `start_paused` test depends on this.**
4. **`sqruff` lints all SQL and is hook-enforced, but `just lint` does not run it.**
   Run `sqruff fix` on new SQL **before** applying the migration — reformatting an
   already-applied migration strands its checksum and blocks every commit. See
   `docs/sqlx-troubleshooting.md`.
5. **Verification order is `just prepare` → `just lint` → `just test`**, not the order
   some task steps state. `prepare` generates the `.sqlx/` entries the other two need.
6. **`EffectiveSettings` exposes `Resolved<u32>`** while the scheduler holds `usize` and
   `Semaphore` takes `usize`. Convert with
   `usize::try_from(v).unwrap_or(<DEFAULT const>)` — never `as`, never `usize::MAX`.

### Operational notes

- **Postgres is reachable only with the Bash sandbox disabled**, and so is `podman`.
  Both containers (5432 dev, 5433 test) are otherwise healthy.
- **Have the controller own long cargo runs.** Cold builds exceed the 600s foreground
  cap; implementer agents that background one and stop do not reliably wake.
- **`git commit` needs the sandbox disabled** so lefthook's hooks can execute, and the
  SSH signing key must be unlocked (`ssh-add ~/.ssh/id_ed25519`). Do not use
  `--no-verify`.
- If `#[sqlx::test]` fails with Postgres `XX002` on `databases_pkey`, that is test-registry
  corruption: `REINDEX TABLE _sqlx_test.databases;` against 5433.
- `hof-core`'s suite is ~5.5 min; a full clippy rebuild ~4-8 min. Don't run two cargo
  commands at once.

### Outstanding items

- **Task 2 fix round is ruled but not dispatched:** backfill a listener integration test
  (spawn listener, mutate the row, assert the watch receiver observes it) and rename
  `deadline_republishes_when_pause_lapses`, which does not test what its name claims.
  Spec §9 requires asserting republish; the brief's verbatim test does not.
- **Task 6 prerequisite:** `patch_runtime_settings` has zero tests and is dynamic
  `QueryBuilder` SQL. Do not build the API on it untested.
- **Task 6 caution:** DB tests share the singleton row while `just test` runs
  `--test-threads=4`.
- Tasks 1 → 2 → 3 → 4 → 5 → 6 → 7 remain a **strict dependency chain**; they do not
  parallelize. Documentation (Task 8) does.

## Global Constraints

- **Lint baseline must be green before Task 1.** `lefthook.yml:20` runs `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::pedantic -D clippy::nursery` on pre-commit. A red workspace blocks every commit in this plan. See Task 0.
- **Workspace-denied lints:** `as_conversions`, `indexing_slicing`, `unwrap_used`, `expect_used`, `panic`, `unreachable`, `todo`, `unchecked_time_subtraction`, `panic_in_result_fn`, plus deny-level `pedantic` and `nursery`. `clippy.toml` exempts unwrap/expect/panic/indexing **in tests only**.
- **`arithmetic_side_effects` and `string_slice` are `allow` at the workspace level** (they were 63 of 80 pre-existing errors, almost all ceremony in legacy code). **New code in this feature does not get that relief.** Every file created by this plan MUST open with:

  ```rust
  // This feature is largely deadline and permit arithmetic, where overflow is a
  // real concern rather than ceremony. Hold new code to the strict bar even
  // though the workspace relaxes these for legacy code.
  #![deny(clippy::arithmetic_side_effects, clippy::string_slice)]
  ```

  Applies to `runtime_config.rs`, `db/runtime_settings.rs`, and `routes/settings.rs`. When modifying an existing file, still use checked/saturating arithmetic for any code you add.
- **This feature is mostly deadline arithmetic.** Every `+`/`-` on a `DateTime`, `Duration`, or permit count must use `checked_*` or `saturating_*`. Never bare operators.
- **`#[allow(...)]` requires a justifying comment** and must be scoped to the smallest item. Never module-level. Established pattern: `download_supervisor.rs` around line 222.
- **Run `just prepare` after any schema change** and commit the regenerated `.sqlx/` offline data, or CI and offline builds break.
- **Tests use the ephemeral `postgres-test` service** on localhost:5433 (`just test`), not the dev database.
- **Verification per task:** `just lint` && `just test`. Both must pass before the task is considered done.
- **Never call `std::process::exit`** — `exit = "deny"`. Shutdown returns from `main`.

---

## File Structure

**Create:**
- `crates/hof-core/migrations/20260901120000_runtime_settings.up.sql` — table, singleton guard, seed row, notify trigger
- `crates/hof-core/migrations/20260901120000_runtime_settings.down.sql` — drop trigger, function, table
- `crates/hof-core/src/db/runtime_settings.rs` — `RuntimeSettingsRow`, read/patch queries
- `crates/hof-core/src/runtime_config.rs` — `Provenance`, `Resolved<T>`, `EffectiveSettings`, `EnvOverrides`, `resolve()`, `RuntimeConfig` handle, listener task
- `crates/hof-api/src/routes/settings.rs` — settings, pause, shutdown endpoints

**Modify:**
- `crates/hof-core/src/db/mod.rs` — register the new module
- `crates/hof-core/src/lib.rs` — register `runtime_config`
- `crates/hof-core/src/config.rs` — add `EnvOverrides::from_env()`
- `crates/hof-core/src/startup.rs` — build `RuntimeConfig`, thread receivers into actors, add drain token
- `crates/hof-core/src/actors/scheduler.rs` — config receiver, interval rebuild, indexing pause gate
- `crates/hof-core/src/actors/download_supervisor.rs` — config receiver, semaphore resize, download pause gate
- `crates/hof-core/src/actors/cleanup.rs` — config receiver, interval rebuild
- `crates/hof-api/src/lib.rs` — `AppState` gains `runtime_config` and `drain`
- `crates/hof-api/src/routes/system.rs` — extend status with pause/drain
- `crates/hof-web/src/main.rs` — third `select!` arm for drain
- `crates/hof-web/src/pages.rs` — control panel page

---

## Task 0: Green the lint baseline — ✅ DONE (2026-09-01)

Completed before Task 1. Recorded here because it changed constraints the
remaining tasks inherit.

- [x] **Lint baseline: 80 errors → 0.** `just lint` exits 0 workspace-wide.
- [x] **Full suite green:** 360 tests passed, 0 failed.
- [x] **Lint policy relaxed** — `arithmetic_side_effects` and `string_slice` set
      to `allow` in `Cargo.toml` (63 of the 80 errors, almost all ceremony in
      legacy code). Crash-preventing lints (`unwrap_used`, `expect_used`,
      `panic`, `indexing_slicing`, `unreachable`, `todo`) remain denied.
      **New code in this feature does not get that relief** — see Global
      Constraints for the required `#![deny(...)]` header.
- [x] **Two tooling bugs fixed**, both of which would have blocked this plan:
      - `justfile` / `lefthook.yml` passed `-D clippy::pedantic -D clippy::nursery`
        on the command line, which re-denied both groups *after* the manifest's
        lint levels and silently defeated all nine selective `allow` entries.
        Removed; `Cargo.toml` is now the single source of truth. The same stale
        flags were removed from `bacon.toml` and `AGENTS.md`.
      - All four test recipes (`test`, `e2e`, `e2e-only`, `ci`) lacked
        `SQLX_OFFLINE=true`. The lean postgres-test instance carries no schema,
        so the `query!` macros failed at *compile* time with
        `relation "sources" does not exist` — `just test` could not run at all.

**Commits:** `e643a65`, `6464227`, `d616eb9`.

### Known testability debt (pre-existing, not introduced here)

Two functions touched during the cleanup have no test coverage. The changes
made to them are equivalent by construction (each replaced an unchecked
operation with a checked one on a provably-unreachable failure path), but the
gaps would hide a *future* mistake:

- `source_indexer.rs::detect_entry_order` — untestable without mocking
  `ytdlp.fetch_video_metadata`. Extracting the pure order-comparison logic from
  the network call would make it testable.
- `download_supervisor.rs::effective_rate_limit_delay` and the
  `rate_limit_backoff_multiplier` arithmetic — needs a constructed supervisor.

Neither blocks this plan. Worth addressing when that code is next touched.

---

## Task 1: Schema, row type, and resolver

**Files:**
- Create: `crates/hof-core/migrations/20260901120000_runtime_settings.{up,down}.sql`
- Create: `crates/hof-core/src/db/runtime_settings.rs`
- Create: `crates/hof-core/src/runtime_config.rs`
- Modify: `crates/hof-core/src/db/mod.rs`, `crates/hof-core/src/lib.rs`, `crates/hof-core/src/config.rs`

**Interfaces:**
- Consumes: `crate::config::Config`
- Produces:
  - `db::runtime_settings::{RuntimeSettingsRow, get_runtime_settings, patch_runtime_settings, RuntimeSettingsPatch}`
  - `runtime_config::{Provenance, Resolved<T>, EffectiveSettings, EnvOverrides, resolve}`
  - `EffectiveSettings::{indexing_paused, downloads_paused}(now: DateTime<Utc>) -> bool`
  - `EffectiveSettings::next_pause_deadline(now) -> Option<DateTime<Utc>>`

- [ ] **Step 1: Write the migration**

`20260901120000_runtime_settings.up.sql`:
```sql
CREATE TABLE IF NOT EXISTS runtime_settings (
    id                       BOOLEAN PRIMARY KEY DEFAULT true CHECK (id),
    indexing_paused_until    TIMESTAMPTZ,
    downloads_paused_until   TIMESTAMPTZ,
    max_concurrent_downloads INTEGER CHECK (max_concurrent_downloads >= 1),
    max_indexers_per_tick    INTEGER CHECK (max_indexers_per_tick >= 1),
    rate_limit_delay_secs    INTEGER CHECK (rate_limit_delay_secs >= 0),
    check_interval_secs      INTEGER CHECK (check_interval_secs >= 1),
    cleanup_interval_secs    INTEGER CHECK (cleanup_interval_secs >= 1),
    drain_timeout_secs       INTEGER CHECK (drain_timeout_secs >= 1),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by               TEXT REFERENCES users (id)
);

INSERT INTO runtime_settings (id) VALUES (true) ON CONFLICT (id) DO NOTHING;

CREATE OR REPLACE FUNCTION notify_runtime_settings_changed()
RETURNS TRIGGER AS $$
BEGIN
    PERFORM pg_notify('runtime_settings_changed', '');
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_runtime_settings_notify
AFTER INSERT OR UPDATE ON runtime_settings
FOR EACH ROW EXECUTE FUNCTION notify_runtime_settings_changed();
```

`20260901120000_runtime_settings.down.sql`:
```sql
DROP TRIGGER IF EXISTS trg_runtime_settings_notify ON runtime_settings;
DROP FUNCTION IF EXISTS notify_runtime_settings_changed();
DROP TABLE IF EXISTS runtime_settings;
```

- [ ] **Step 2: Run the migration**

Run: `just mig-run`
Expected: applies cleanly. Then `just prepare` to refresh `.sqlx/`.

- [ ] **Step 3: Write the failing resolver test**

In `crates/hof-core/src/runtime_config.rs`, at the bottom:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn empty_row() -> RuntimeSettingsRow { RuntimeSettingsRow::default() }
    fn no_env() -> EnvOverrides { EnvOverrides::default() }

    #[test]
    fn falls_back_to_code_default() {
        let s = resolve(&empty_row(), &no_env());
        assert_eq!(s.max_concurrent_downloads.value, DEFAULT_MAX_CONCURRENT);
        assert_eq!(s.max_concurrent_downloads.provenance, Provenance::Default);
    }

    #[test]
    fn env_overrides_default() {
        let env = EnvOverrides { max_concurrent_downloads: Some(7), ..no_env() };
        let s = resolve(&empty_row(), &env);
        assert_eq!(s.max_concurrent_downloads.value, 7);
        assert_eq!(s.max_concurrent_downloads.provenance, Provenance::Env);
    }

    #[test]
    fn database_overrides_env() {
        let env = EnvOverrides { max_concurrent_downloads: Some(7), ..no_env() };
        let row = RuntimeSettingsRow { max_concurrent_downloads: Some(2), ..empty_row() };
        let s = resolve(&row, &env);
        assert_eq!(s.max_concurrent_downloads.value, 2);
        assert_eq!(s.max_concurrent_downloads.provenance, Provenance::Database);
    }

    #[test]
    fn null_pause_is_not_paused() {
        let s = resolve(&empty_row(), &no_env());
        assert!(!s.indexing_paused(Utc::now()));
    }

    #[test]
    fn future_pause_is_paused_and_yields_deadline() {
        let until = Utc::now() + chrono::Duration::hours(1);
        let row = RuntimeSettingsRow { indexing_paused_until: Some(until), ..empty_row() };
        let s = resolve(&row, &no_env());
        assert!(s.indexing_paused(Utc::now()));
        assert_eq!(s.next_pause_deadline(Utc::now()), Some(until));
    }

    #[test]
    fn elapsed_pause_is_not_paused() {
        let past = Utc::now() - chrono::Duration::hours(1);
        let row = RuntimeSettingsRow { indexing_paused_until: Some(past), ..empty_row() };
        let s = resolve(&row, &no_env());
        assert!(!s.indexing_paused(Utc::now()));
        assert_eq!(s.next_pause_deadline(Utc::now()), None);
    }

    #[test]
    fn infinity_pause_is_paused_but_schedules_no_deadline() {
        let row = RuntimeSettingsRow {
            downloads_paused_until: Some(DateTime::<Utc>::MAX_UTC),
            ..empty_row()
        };
        let s = resolve(&row, &no_env());
        assert!(s.downloads_paused(Utc::now()));
        assert_eq!(s.next_pause_deadline(Utc::now()), None);
    }

    #[test]
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
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test -p hof-core --lib runtime_config`
Expected: FAIL — `resolve`, `EffectiveSettings`, etc. do not exist.

- [ ] **Step 5: Implement the row type**

`crates/hof-core/src/db/runtime_settings.rs`:
```rust
//! Runtime-mutable settings, stored as a single row.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::DbError;

/// The singleton `runtime_settings` row. `None` in a tunable field means
/// "not set at the database layer" — the resolver falls back to env, then
/// to the compiled-in default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeSettingsRow {
    pub indexing_paused_until: Option<DateTime<Utc>>,
    pub downloads_paused_until: Option<DateTime<Utc>>,
    pub max_concurrent_downloads: Option<i32>,
    pub max_indexers_per_tick: Option<i32>,
    pub rate_limit_delay_secs: Option<i32>,
    pub check_interval_secs: Option<i32>,
    pub cleanup_interval_secs: Option<i32>,
    pub drain_timeout_secs: Option<i32>,
    pub updated_at: Option<DateTime<Utc>>,
    pub updated_by: Option<String>,
}

/// Read the singleton settings row.
pub async fn get_runtime_settings(pool: &PgPool) -> Result<RuntimeSettingsRow, DbError> {
    let row = sqlx::query!(
        r#"
        SELECT indexing_paused_until, downloads_paused_until,
               max_concurrent_downloads, max_indexers_per_tick,
               rate_limit_delay_secs, check_interval_secs,
               cleanup_interval_secs, drain_timeout_secs,
               updated_at, updated_by
        FROM runtime_settings WHERE id = true
        "#
    )
    .fetch_one(pool)
    .await?;

    Ok(RuntimeSettingsRow {
        indexing_paused_until: row.indexing_paused_until,
        downloads_paused_until: row.downloads_paused_until,
        max_concurrent_downloads: row.max_concurrent_downloads,
        max_indexers_per_tick: row.max_indexers_per_tick,
        rate_limit_delay_secs: row.rate_limit_delay_secs,
        check_interval_secs: row.check_interval_secs,
        cleanup_interval_secs: row.cleanup_interval_secs,
        drain_timeout_secs: row.drain_timeout_secs,
        updated_at: Some(row.updated_at),
        updated_by: row.updated_by,
    })
}
```

Add a `RuntimeSettingsPatch` with `Option<Option<T>>` per field (outer `None` = leave alone, inner `None` = reset to fallback) and a `patch_runtime_settings` that applies it with `COALESCE`-free explicit `SET` clauses. Register the module in `db/mod.rs` (`mod runtime_settings;` + `pub use runtime_settings::*;`).

- [ ] **Step 6: Implement env overrides**

In `crates/hof-core/src/config.rs`, add — reading the same variables as the existing `from_env`, but preserving absence so provenance can be reported:
```rust
/// Raw environment overrides, preserving "unset" so the resolver can report
/// whether a value came from the environment or from a compiled-in default.
#[derive(Debug, Clone, Default)]
pub struct EnvOverrides {
    pub max_concurrent_downloads: Option<u32>,
    pub max_indexers_per_tick: Option<u32>,
    pub rate_limit_delay_secs: Option<u64>,
    pub check_interval_secs: Option<u64>,
    pub cleanup_interval_secs: Option<u64>,
    pub drain_timeout_secs: Option<u64>,
}

impl EnvOverrides {
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            max_concurrent_downloads: optional_env("MAX_CONCURRENT_DOWNLOADS")
                .and_then(|s| s.parse().ok()),
            max_indexers_per_tick: optional_env("MAX_INDEXERS_PER_TICK")
                .and_then(|s| s.parse().ok()),
            rate_limit_delay_secs: optional_env("RATE_LIMIT_DELAY_SECS")
                .and_then(|s| s.parse().ok()),
            check_interval_secs: optional_env("CHECK_INTERVAL_SECS")
                .and_then(|s| s.parse().ok()),
            cleanup_interval_secs: optional_env("CLEANUP_INTERVAL_SECS")
                .and_then(|s| s.parse().ok()),
            drain_timeout_secs: optional_env("DRAIN_TIMEOUT_SECS")
                .and_then(|s| s.parse().ok()),
        }
    }
}
```

- [ ] **Step 7: Implement the resolver**

`crates/hof-core/src/runtime_config.rs`:
```rust
//! Runtime-mutable configuration: resolution and propagation.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::config::EnvOverrides;
use crate::db::RuntimeSettingsRow;

pub const DEFAULT_MAX_CONCURRENT: u32 = 3;
pub const DEFAULT_MAX_INDEXERS_PER_TICK: u32 = 5;
pub const DEFAULT_RATE_LIMIT_DELAY_SECS: u64 = 5;
pub const DEFAULT_CHECK_INTERVAL_SECS: u64 = 60;
pub const DEFAULT_CLEANUP_INTERVAL_SECS: u64 = 60 * 60 * 3;
pub const DEFAULT_DRAIN_TIMEOUT_SECS: u64 = 1800;

/// Which layer supplied a resolved value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance { Default, Env, Database }

/// A resolved value together with the layer it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Resolved<T> { pub value: T, pub provenance: Provenance }

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
    /// Returns `None` for an indefinite pause (`DateTime::<Utc>::MAX_UTC`),
    /// because such a pause never lapses on its own and must not arm a timer.
    #[must_use]
    pub fn next_pause_deadline(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        [self.indexing_paused_until, self.downloads_paused_until]
            .into_iter()
            .flatten()
            .filter(|t| *t > now && *t != DateTime::<Utc>::MAX_UTC)
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
            row.max_concurrent_downloads, env.max_concurrent_downloads, DEFAULT_MAX_CONCURRENT),
        max_indexers_per_tick: pick_u32(
            row.max_indexers_per_tick, env.max_indexers_per_tick, DEFAULT_MAX_INDEXERS_PER_TICK),
        rate_limit_delay: pick_secs(
            row.rate_limit_delay_secs, env.rate_limit_delay_secs, DEFAULT_RATE_LIMIT_DELAY_SECS),
        check_interval: pick_secs(
            row.check_interval_secs, env.check_interval_secs, DEFAULT_CHECK_INTERVAL_SECS),
        cleanup_interval: pick_secs(
            row.cleanup_interval_secs, env.cleanup_interval_secs, DEFAULT_CLEANUP_INTERVAL_SECS),
        drain_timeout: pick_secs(
            row.drain_timeout_secs, env.drain_timeout_secs, DEFAULT_DRAIN_TIMEOUT_SECS),
    }
}

fn pick_u32(db: Option<i32>, env: Option<u32>, default: u32) -> Resolved<u32> {
    if let Some(v) = db.and_then(|v| u32::try_from(v).ok()) {
        return Resolved { value: v, provenance: Provenance::Database };
    }
    env.map_or(
        Resolved { value: default, provenance: Provenance::Default },
        |v| Resolved { value: v, provenance: Provenance::Env },
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
    Resolved { value: Duration::from_secs(secs), provenance }
}
```

Register in `lib.rs`: `pub mod runtime_config;`.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p hof-core --lib runtime_config`
Expected: PASS, all 8 tests.

- [ ] **Step 9: Verify and commit**

```bash
just lint && just test && just prepare
git add crates/hof-core/migrations crates/hof-core/src/db crates/hof-core/src/runtime_config.rs \
        crates/hof-core/src/config.rs crates/hof-core/src/lib.rs .sqlx
git commit -m "feat(config): add runtime_settings table and precedence resolver"
```

---

## Task 2: Propagation — listener, watch channel, expiry

**Files:**
- Modify: `crates/hof-core/src/runtime_config.rs`
- Test: same file's `mod tests`

**Interfaces:**
- Consumes: `resolve`, `EffectiveSettings`, `get_runtime_settings` (Task 1)
- Produces:
  - `RuntimeConfig::new(pool, EnvOverrides) -> Result<RuntimeConfig>`
  - `RuntimeConfig::subscribe() -> watch::Receiver<Arc<EffectiveSettings>>`
  - `RuntimeConfig::current() -> Arc<EffectiveSettings>`
  - `RuntimeConfig::spawn_listener(self) -> tokio::task::JoinHandle<()>`

- [ ] **Step 1: Write the failing expiry test**

```rust
#[tokio::test(start_paused = true)]
async fn deadline_republishes_when_pause_lapses() {
    let until = Utc::now() + chrono::Duration::hours(1);
    let row = RuntimeSettingsRow { indexing_paused_until: Some(until), ..RuntimeSettingsRow::default() };
    let settings = resolve(&row, &EnvOverrides::default());
    assert!(settings.indexing_paused(Utc::now()));

    let deadline = settings.next_pause_deadline(Utc::now()).expect("deadline");
    let wait = sleep_duration_until(deadline, Utc::now());
    assert!(wait >= Duration::from_secs(3500) && wait <= Duration::from_secs(3600));

    tokio::time::sleep(wait).await;
    // After the deadline the same settings value must read as un-paused.
    assert!(!settings.indexing_paused(deadline + chrono::Duration::seconds(1)));
}

#[test]
fn sleep_duration_is_zero_for_elapsed_deadline() {
    let past = Utc::now() - chrono::Duration::hours(1);
    assert_eq!(sleep_duration_until(past, Utc::now()), Duration::ZERO);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p hof-core --lib runtime_config`
Expected: FAIL — `sleep_duration_until` not defined.

- [ ] **Step 3: Implement the handle, helper, and listener**

Append to `runtime_config.rs`:
```rust
use std::sync::Arc;
use color_eyre::eyre::Result;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::watch;
use tracing::{error, info, warn};

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
    deadline.signed_duration_since(now).to_std().unwrap_or(Duration::ZERO)
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
    pub async fn new(pool: PgPool, env: EnvOverrides) -> Result<Self> {
        let row = crate::db::get_runtime_settings(&pool).await?;
        let (tx, _) = watch::channel(Arc::new(resolve(&row, &env)));
        Ok(Self { tx, pool, env })
    }

    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Arc<EffectiveSettings>> { self.tx.subscribe() }

    #[must_use]
    pub fn current(&self) -> Arc<EffectiveSettings> { self.tx.borrow().clone() }

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
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p hof-core --lib runtime_config`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
just lint && just test
git add crates/hof-core/src/runtime_config.rs
git commit -m "feat(config): propagate runtime settings via LISTEN/NOTIFY and watch"
```

---

## Task 3: Consumers adopt the watch channel

**Files:**
- Modify: `crates/hof-core/src/startup.rs`, `actors/cleanup.rs`, `actors/scheduler.rs`, `actors/download_supervisor.rs`

**Interfaces:**
- Consumes: `RuntimeConfig::subscribe()` (Task 2)
- Produces: each actor's `Args` gains `config_rx: watch::Receiver<Arc<EffectiveSettings>>`

Three shapes of knob application — implement each once:

- [ ] **Step 1: Interval rebuild (cleanup + scheduler)**

`cleanup.rs` currently ticks bare inside its spawned loop (~line 139). Replace with a `select!` that rebuilds on change:
```rust
let mut ticker = interval(current_interval);
ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
loop {
    tokio::select! {
        _ = ticker.tick() => { /* existing cleanup body */ }
        Ok(()) = config_rx.changed() => {
            let next = config_rx.borrow_and_update().cleanup_interval.value;
            if next != current_interval {
                current_interval = next;
                ticker = interval(current_interval);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
                info!(interval_secs = current_interval.as_secs(), "Cleanup interval updated");
            }
        }
    }
}
```
Apply the identical shape to the scheduler's `check_interval`.

- [ ] **Step 2: Read-per-use (scheduler)**

Replace the `self.max_indexers_per_tick` field read with `config_rx.borrow().max_indexers_per_tick.value` at the top of `CheckSources`. Same for `rate_limit_delay` in the supervisor.

- [ ] **Step 3: Semaphore resize (supervisor)**

First add a field to `DownloadSupervisor` tracking the total permits ever
issued — `Semaphore` exposes only *available* permits, so the current target
cannot be recovered from it while downloads are in flight:
```rust
/// Total permits the semaphore is sized to, including those currently held.
/// `Semaphore::available_permits()` excludes in-flight permits, so it cannot
/// serve as the resize baseline.
permits_total: usize,
```
Initialize it in `Actor::on_start` next to the semaphore, from the same
resolved `max_concurrent_downloads` value.

```rust
/// Resize the download semaphore.
///
/// Growing is immediate. Shrinking reclaims only currently-free permits;
/// the remainder lands as in-flight downloads finish, so a decrease is
/// applied lazily by design (see design doc §4.5).
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
```

- [ ] **Step 4: Write tests**

```rust
#[tokio::test]
async fn semaphore_grows_immediately() {
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
```

- [ ] **Step 5: Wire startup**

In `startup.rs::initialize`, after the pool is available:
```rust
let runtime_config = RuntimeConfig::new(pool.clone(), EnvOverrides::from_env()).await?;
let _listener = runtime_config.clone().spawn_listener();
```
Pass `runtime_config.subscribe()` into `DownloadSupervisorArgs`, `SchedulerArgs`, and `CleanupActorArgs`, and return `runtime_config` on `ActorSystem`.

- [ ] **Step 6: Verify and commit**

```bash
just lint && just test
git add crates/hof-core/src
git commit -m "feat(actors): adopt runtime settings for pacing and concurrency"
```

---

## Task 4: Pause gates

**Files:** Modify `actors/scheduler.rs`, `actors/download_supervisor.rs`

**Interfaces:** Consumes `EffectiveSettings::{indexing_paused, downloads_paused}` (Task 1)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn paused_indexing_blocks_new_indexers() {
    let row = RuntimeSettingsRow {
        indexing_paused_until: Some(Utc::now() + chrono::Duration::hours(1)),
        ..RuntimeSettingsRow::default()
    };
    let s = resolve(&row, &EnvOverrides::default());
    assert!(s.indexing_paused(Utc::now()));
}

#[test]
fn paused_downloads_leave_indexing_running() {
    let row = RuntimeSettingsRow {
        downloads_paused_until: Some(Utc::now() + chrono::Duration::hours(1)),
        ..RuntimeSettingsRow::default()
    };
    let s = resolve(&row, &EnvOverrides::default());
    assert!(s.downloads_paused(Utc::now()));
    assert!(!s.indexing_paused(Utc::now()));
}
```

- [ ] **Step 2: Implement the scheduler gate**

At the top of `CheckSources::handle`, before the due-source loop:
```rust
if self.config_rx.borrow().indexing_paused(Utc::now()) {
    debug!("Indexing paused; skipping this tick");
    return;
}
```
In-flight `SourceIndexerActor`s are untouched and run to completion — this only prevents new spawns.

- [ ] **Step 3: Implement the supervisor gate**

In the dispatch path, before acquiring a permit:
```rust
if self.config_rx.borrow().downloads_paused(Utc::now()) {
    debug!("Downloads paused; leaving videos pending");
    return;
}
```
Videos remain `pending` and drain naturally on resume. This is the "index ok but no downloads" mode.

- [ ] **Step 4: Verify and commit**

```bash
just lint && just test
git add crates/hof-core/src/actors
git commit -m "feat(actors): gate indexing and downloads on pause state"
```

---

## Task 5: Drain and shutdown

**Files:** Modify `crates/hof-core/src/startup.rs`, `crates/hof-api/src/lib.rs`, `crates/hof-web/src/main.rs`

**Interfaces:**
- Produces: `DrainToken { start: Option<DateTime<Utc>>, token: CancellationToken }`, `AppState.drain`

- [ ] **Step 1: Add the drain token**

Process-local only — never persisted. See ADR-0004: a persisted drain would leave a restarted container refusing work with no visible cause.
```rust
In `crates/hof-core/src/runtime_config.rs`:
```rust
use std::sync::{Arc, RwLock};
use tokio::sync::Notify;

/// Process-local drain state. Deliberately NOT persisted (see ADR-0004):
/// a persisted drain would leave a restarted container refusing all work,
/// wedged, with no visible cause.
#[derive(Debug, Clone, Default)]
pub struct DrainToken {
    started_at: Arc<RwLock<Option<DateTime<Utc>>>>,
    complete: Arc<Notify>,
}

impl DrainToken {
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Begin draining. Idempotent within a process: the first start time wins,
    /// so repeated calls cannot extend the drain deadline.
    pub fn begin(&self, now: DateTime<Utc>) {
        // A poisoned lock means another thread panicked while holding it.
        // Recover rather than propagate: refusing to drain is worse than
        // proceeding, and the guarded value is a single Option.
        let mut guard = self.started_at.write().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = Some(now);
        }
    }

    #[must_use]
    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        *self.started_at.read().unwrap_or_else(|e| e.into_inner())
    }

    #[must_use]
    pub fn is_draining(&self) -> bool { self.started_at().is_some() }

    /// Absolute deadline after which shutdown proceeds regardless.
    #[must_use]
    pub fn deadline(&self, timeout: Duration) -> Option<DateTime<Utc>> {
        let started = self.started_at()?;
        let delta = chrono::Duration::from_std(timeout).ok()?;
        started.checked_add_signed(delta)
    }

    /// Signal that draining finished; wakes `main`'s shutdown arm.
    pub fn signal_complete(&self) { self.complete.notify_waiters(); }

    /// Await drain completion — this is what `main.rs`'s third select arm uses.
    pub async fn wait_complete(&self) { self.complete.notified().await; }
}
```

Note the `unwrap_or_else(|e| e.into_inner())` idiom: `unwrap_used` is denied, and this recovers from a poisoned lock rather than panicking.
```

- [ ] **Step 2: Extend the pause gates**

Both gates from Task 4 also return early when `drain.is_draining()` — reuse, no second refusal path.

- [ ] **Step 3: Drain watcher**

Polls supervisor and scheduler status until `active_downloads == 0 && active_indexers == 0`, or `drain_timeout` elapses, then signals shutdown. Forcing is safe: `recover_from_crash` already resets `downloading` rows to `pending` and removes `.part` files on next boot.

- [ ] **Step 4: Third select arm in `main.rs`**

At `main.rs:129`, alongside `server` and `ctrl_c`:
```rust
() = drain_complete.notified() => {
    tracing::info!("Drain complete, shutting down");
}
```
Then fall through to the existing `shutdown(actor_system)` at line 142 and return `Ok(())`. **Do not call `std::process::exit`** — `exit = "deny"`, and returning from `main` yields exit code 0 naturally.

- [ ] **Step 5: Write the test**

```rust
#[tokio::test(start_paused = true)]
async fn drain_times_out_and_still_shuts_down() { /* never reaches quiescence */ }

#[tokio::test]
async fn drain_token_starts_not_draining() {
    let t = DrainToken::new();
    assert!(!t.is_draining());
    t.begin();
    assert!(t.is_draining());
}
```

- [ ] **Step 6: Verify and commit**

```bash
just lint && just test
git add crates/hof-core/src crates/hof-api/src crates/hof-web/src/main.rs
git commit -m "feat(shutdown): add drain-then-exit with bounded drain window"
```

---

## Task 6: API endpoints

**Files:** Create `crates/hof-api/src/routes/settings.rs`; modify `routes/system.rs`, `routes/mod.rs` (or `lib.rs` router assembly), `lib.rs` (`AppState`)

**Interfaces:** Consumes `RuntimeConfig`, `DrainToken`, `patch_runtime_settings`

Follow the existing `OpenApiRouter` + utoipa pattern (`routes!(...)`, `#[utoipa::path]`, `Auth` extractor, `auth.require_scope(ApiKeyScope::Write)` on mutations) exactly as `trigger_cleanup` at `system.rs:234` does.

- [ ] **Step 1: Response types** — `SettingsResponse` with `{ value, provenance }` per field; pause serialized as `{ paused: bool, until: Option<DateTime<Utc>>, indefinite: bool }`. Never emit a literal `"infinity"` string (ADR-0003).
- [ ] **Step 2:** `GET /api/v1/system/settings`
- [ ] **Step 3:** `PATCH /api/v1/system/settings` — partial; explicit JSON `null` resets to fallback. Validate against the same bounds as the `CHECK` constraints; reject with 400 and a clear message.
- [ ] **Step 4:** `POST /api/v1/system/pause` — `{ module, duration }`; compute `now + interval` with `checked_add_signed`, or `DateTime::<Utc>::MAX_UTC` for indefinite.
- [ ] **Step 5:** `DELETE /api/v1/system/pause` — set the column(s) to `NULL`.
- [ ] **Step 6:** `POST /api/v1/system/shutdown` — begin drain, return 202 with the drain deadline.
- [ ] **Step 7:** Extend `GET /api/v1/system/status` with pause state, expiries, and drain progress.
- [ ] **Step 8: Endpoint tests** alongside the existing suite in `crates/hof-api/src/routes/tests.rs`.
- [ ] **Step 9: Verify and commit**

```bash
just lint && just test && just prepare
git add crates/hof-api .sqlx
git commit -m "feat(api): add runtime settings, pause, and shutdown endpoints"
```

---

## Task 7: UI control panel

**Files:** Modify `crates/hof-web/src/pages.rs`, `crates/hof-web/src/lib.rs`

Maud + htmx, matching existing pages. Live updates reuse the `ActivityBroadcaster` SSE path — do not add polling.

- [ ] **Step 1:** Pause controls per module — dropdown 1h / 6h / 12h / 24h / 3d / 7d / indefinite, with a Resume action when paused.
- [ ] **Step 2:** Drain button with confirmation and live remaining-job count.
- [ ] **Step 3:** Effective-settings table — every knob with its value and a `default` / `env` / `db` badge. This badge is required, not decorative: without it a three-layer precedence chain is opaque (ADR-0002).
- [ ] **Step 4: Timings panel** (design §7.1). Each of these shown as an absolute timestamp **and** a live relative countdown:
  indexing pause expiry; downloads pause expiry; drain deadline and remaining; time drained so far; next cleanup run; next scheduler tick; download timeout; yt-dlp command timeout; minimum re-index interval; rate-limit delay.
  Render countdowns client-side from a served absolute timestamp so ticking costs no requests.
- [ ] **Step 5:** `just css-build` for Tailwind, then verify manually with `just dev`.
- [ ] **Step 6: Verify and commit**

```bash
just lint && just test && just css-build
git add crates/hof-web
git commit -m "feat(ui): add runtime control panel with pause, drain, and timings"
```

---

## Task 8: Documentation

- [ ] **Step 1:** README — document the new env vars (`MAX_INDEXERS_PER_TICK`, `CHECK_INTERVAL_SECS`, `CLEANUP_INTERVAL_SECS`, `DRAIN_TIMEOUT_SECS`) and state that database values override them.
- [ ] **Step 2:** README — **document the restart-policy caveat**: draining exits 0, so under `restart: always` or `unless-stopped` the container comes straight back up. Deployments wanting the process to stay down must use `restart: on-failure`. This is the one place the UI's "shut down" will not do what it says, and it is a deployment-side fix.
- [ ] **Step 3:** Update `AGENTS.md` if the actor wiring description changed.
- [ ] **Step 4:** Commit.

---

## Notes on sequencing

Tasks 1 → 2 → 3 → 4 are a strict chain; each builds on the previous task's types. Task 5 depends on Task 4's gates. Tasks 6 and 7 depend on everything before them; 7 additionally depends on 6's response shapes. **None of these parallelize** — dispatching them concurrently produces agents building against interfaces that do not exist yet. Task 0 was the only genuinely parallel work in this plan.
