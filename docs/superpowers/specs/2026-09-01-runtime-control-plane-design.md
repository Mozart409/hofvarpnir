# Runtime Control Plane — Design

**Date:** 2026-09-01
**Issue:** [#130 — FEATURE: better 429 handling](https://github.com/Mozart409/hofvarpnir/issues/130)
**Status:** Approved (design), pending implementation plan

---

## 1. Purpose

Give an operator manual, immediate control over hofvarpnir's background load —
pausing indexing and downloads, retuning concurrency and pacing, and draining
the process for a clean shutdown — without a restart and without editing
environment variables.

This is **proactive control, not an outage fix**. The mass-timeout incident of
2026-08-21 (`todos/indexing-timeout-2026-08-23.md`) was resolved in 0.7 by
`DEFAULT_MAX_INDEXERS_PER_TICK = 5` in `SchedulerActor`, which staggers a due
backlog across ticks instead of spawning every indexer at once. Issue #130's
item 2 asked whether the existing design would hold up under sustained load;
that question is closed. What remains is the absence of any lever an operator
can pull *before* load becomes a problem, and any view into what pacing is
currently in effect.

### 1.1 Scope

Issue #130 lists six items. Their disposition:

| Item | Disposition |
|---|---|
| 1. Timed pause of indexing / downloads | **In scope** — core of this design |
| 2. Does the current design hold under load? | **Closed** — fixed in 0.7, see above |
| 3. Runtime-mutable config and how modules observe it | **In scope** — the keystone; determines 1, 4, 6 |
| 4. Drain and shut down from the UI | **In scope** — as two distinct controls |
| 5. Show active profile | **Merged into item 6** — meant effective settings, not `profiles` rows |
| 6. Show active timeouts | **In scope** — the effective-settings view |

Two premises in the issue were stale and are corrected here: the cleanup
interval is already 3h (`cleanup.rs:26`), not 15 minutes; and there is no
`config.toml` — configuration is entirely environment-variable driven
(`hof-core/src/config.rs`).

### 1.2 Non-goals

- Reducing request pressure automatically (no adaptive backoff or 429 detection).
- Cancelling in-flight work on pause. In-flight jobs always run to completion.
- Multi-instance deployment. The design must not *foreclose* it; it does not
  deliver it.

---

## 2. Architecture

Four components, in dependency order:

1. **`runtime_settings` table** — the source of truth, one singleton row.
2. **Resolver** — combines the DB row with the env-derived `Config` into an
   `EffectiveSettings` value.
3. **`RuntimeConfig` handle + listener task** — republishes `EffectiveSettings`
   over a `tokio::sync::watch` channel whenever the row changes or a pause
   deadline lapses.
4. **Consumers** — `SchedulerActor`, `DownloadSupervisor`, `CleanupActor` hold a
   `watch::Receiver` and adapt without restarting.

The handle is threaded through `startup.rs::initialize` exactly as the existing
`ActivityBroadcaster` already is, and follows the same construction shape. This
is deliberate: it is the codebase's established pattern for "a thing every actor
observes," and reusing it keeps the wiring uniform.

```
PATCH /settings ──▶ runtime_settings ──trigger──▶ pg_notify
                                                     │
                                                     ▼
                                            listener task ──┐
                                                     ▲      │ resolve(row, Config)
                                     sleep_until(deadline)  │
                                                            ▼
                                                  watch::Sender<Arc<EffectiveSettings>>
                                                            │
                        ┌───────────────────────────────────┼───────────────────┐
                        ▼                                   ▼                   ▼
                 SchedulerActor                     DownloadSupervisor    CleanupActor
```

---

## 3. Settings model

### 3.1 Schema

```sql
CREATE TABLE IF NOT EXISTS runtime_settings (
    id                       BOOLEAN PRIMARY KEY DEFAULT true CHECK (id),
    indexing_paused_until    TIMESTAMPTZ,
    downloads_paused_until   TIMESTAMPTZ,
    max_concurrent_downloads INTEGER,
    max_indexers_per_tick    INTEGER,
    rate_limit_delay_secs    INTEGER,
    check_interval_secs      INTEGER,
    cleanup_interval_secs    INTEGER,
    drain_timeout_secs       INTEGER,
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by               TEXT REFERENCES users (id)
);
```

Typed columns, not a `jsonb` bag, so `cargo sqlx prepare` continues to give
compile-time verification of every read. `CHECK (id)` on a boolean primary key
is the standard singleton guard: exactly one row, seeded by the migration.

Every tunable column is **nullable**, and `NULL` means "not set at this layer."
That is what makes both the precedence chain (§3.3) and "reset to default"
(set the column back to `NULL`) work without a sentinel value.

Value constraints (`max_concurrent_downloads >= 1`, intervals `> 0`, etc.) are
enforced as `CHECK` constraints *and* validated in the API layer, so a bad value
cannot be written by any path.

### 3.2 Pause encoding

Pause is **one nullable timestamp per module**, not a boolean plus a duration:

- `NULL` — running.
- A future timestamp — paused until then, auto-resuming.
- `'infinity'::timestamptz` — paused indefinitely.

Postgres supports `infinity` natively for `timestamptz`, so item 1's timed pause
and item 4's indefinite "Pause all" collapse into a single column with no mode
flag and no combination of fields that can contradict each other. The UI
dropdown (1h / 6h / 12h / 24h / 3d / 7d) simply computes `now() + interval`.

Because pause lives in the database, **it survives a restart** — which is the
correct behavior: a pause exists to hold back load, and a container restart
silently resuming it is exactly the failure it was meant to prevent.

### 3.3 Precedence

**code default → environment variable → database**, with the database winning.

A `NULL` column falls back to the environment variable, which falls back to the
compiled-in default. This preserves existing env-var deployments untouched,
makes the UI authoritative the moment a knob is set, and gives "reset" a natural
representation.

Actors never see this layering. The resolver produces an `EffectiveSettings`
struct with fully-resolved concrete values, along with the provenance of each
(`Default` / `Env` / `Database`) for display. See
[ADR-0002](../../adr/0002-settings-precedence.md).

---

## 4. Propagation

### 4.1 Write path

An `AFTER INSERT OR UPDATE ON runtime_settings` trigger calls
`pg_notify('runtime_settings_changed', '')`.

Notifying from the trigger rather than the API handler means every writer is
caught — the API, a future admin tool, or a manual `psql` session — and it is
what makes the eventual multi-instance step require no new code. See
[ADR-0001](../../adr/0001-runtime-config-propagation.md).

### 4.2 Read path

One listener task owns a `sqlx::postgres::PgListener`. On notification it
re-reads the singleton row, resolves it against `Config`, and publishes the
result through a `watch::Sender<Arc<EffectiveSettings>>`.

`watch` is the right channel here rather than `broadcast`: consumers only ever
care about the *latest* value, late subscribers must see current state
immediately, and a slow consumer must not accumulate a backlog.

### 4.3 Pause expiry

**The listener task owns the deadline.** After each load it computes the nearest
future value among `indexing_paused_until` and `downloads_paused_until` and
`select!`s a `tokio::time::sleep_until` against the notification stream. When the
deadline fires it re-resolves and republishes; the pause has lapsed simply
because `paused_until <= now()`.

No database write is involved in expiry. This matters for two reasons: the
alternative — each actor comparing `paused_until` to `now()` on its own tick —
would make resume latent by up to a full tick, and `CleanupActor` ticks every 3
hours; and a write-free expiry means that under a future multi-instance
deployment each process notices independently, with no race over which one
clears the flag.

### 4.4 Gate placement

- **Indexing pause** — early return in `SchedulerActor`'s `CheckSources` handler,
  before the due-source loop. In-flight `SourceIndexerActor`s run to completion.
- **Downloads pause** — blocks dispatch in `DownloadSupervisor` before semaphore
  acquisition. Videos still enqueue as `pending`; they simply are not started,
  and the backlog drains naturally on resume.

The downloads gate is what delivers issue #130's "index ok but no downloads"
mode: discovery continues, dispatch stops.

### 4.5 How each knob applies

Every tunable falls into one of three shapes:

| Shape | Knobs | Mechanism |
|---|---|---|
| **Read-per-use** | `max_indexers_per_tick`, `rate_limit_delay_secs` | Read the current value at point of use. No coordination. |
| **Interval rebuild** | `check_interval_secs`, `cleanup_interval_secs` | The spawned loops (`cleanup.rs:139` and the scheduler equivalent) currently `interval.tick().await` bare. They become a `select!` on tick vs `cfg_rx.changed()`, rebuilding the `tokio::time::interval` on change. |
| **Semaphore resize** | `max_concurrent_downloads` | `Semaphore::add_permits` to grow; `Semaphore::forget_permits` to shrink. |

**Semaphore resize is asymmetric and the API must say so.** Growing takes effect
immediately. Shrinking reclaims only *currently free* permits; the remainder
lands as in-flight downloads finish. The documented contract is therefore
"increases apply immediately, decreases apply as running downloads complete."
`forget_permits` requires tokio ≥ 1.36; the workspace is on 1.53.1.

---

## 5. Pause vs. drain

These are two distinct controls with different persistence, and the difference
is deliberate.

|  | Pause all | Drain & shut down |
|---|---|---|
| State lives in | Database | In-memory `CancellationToken` in `AppState` |
| Survives restart | Yes | **No, by design** |
| Process | Keeps running | Exits after draining |
| Reversible | Yes, from the UI | No — restart the container |

**Drain state is deliberately not persisted.** A drain is process lifecycle, not
configuration. Persisting it would mean a container that restarted mid-drain
would come back up already refusing work — wedged, with no visible cause. See
[ADR-0004](../../adr/0004-drain-state-not-persisted.md).

### 5.1 Drain sequence

1. `POST /api/v1/system/shutdown` flips the in-memory drain token.
2. Drain reuses the **same gates** from §4.4 — no new refusal logic; it is the
   pause gate with a second source.
3. A drain watcher waits for `active_downloads == 0 && active_indexers == 0`.
4. It signals `main.rs`, whose existing `tokio::select!` (`main.rs:129`, today
   selecting over `server` and `ctrl_c`) gains a third arm, falling through to
   the `shutdown(actor_system)` call already present at `main.rs:142`.

The API and UI keep serving throughout, so drain progress and the remaining job
count stay visible.

### 5.2 Drain timeout

A download may legitimately run for hours (`DOWNLOAD_TIMEOUT_HOURS` defaults to
4), so an unbounded drain is indistinguishable from a hang. The drain window is
bounded (`drain_timeout_secs`, default 1800), after which shutdown proceeds
regardless.

Forcing a drain is safe because crash recovery already handles it:
`startup.rs::recover_from_crash` resets `downloading` rows to `pending` and
removes orphaned `.part` files on the next boot. A forced drain costs progress
on at most a few in-flight files, never correctness.

### 5.3 Exit code

The process exits **0** after a successful drain, by returning normally from
`main` — not via `std::process::exit`, which the workspace now forbids
(`exit = "deny"`).

**Deployment caveat, which must be documented in the README:** under
`restart: always` or `restart: unless-stopped`, an exit-0 container is
immediately restarted, so "drain & shut down" will appear to merely restart the
service. Deployments that want the process to stay down must use
`restart: on-failure`.

---

## 6. API

Following the existing `OpenApiRouter` / utoipa pattern in
`crates/hof-api/src/routes/system.rs`.

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/v1/system/settings` | Effective values **with provenance** per field (`default` / `env` / `database`) |
| `PATCH` | `/api/v1/system/settings` | Partial update; explicit `null` resets a knob to its fallback |
| `POST` | `/api/v1/system/pause` | `{ module: indexing \| downloads \| all, duration: <preset> \| indefinite }` |
| `DELETE` | `/api/v1/system/pause` | Resume immediately |
| `POST` | `/api/v1/system/shutdown` | Begin drain, then exit |
| `GET` | `/api/v1/system/status` | *Extended* with pause state, expiry timestamps, drain progress |

All mutating endpoints require authentication via the existing `Auth` extractor
and are recorded in the activity log with `updated_by`.

Returning provenance from `GET /settings` is not decoration — it is what makes
item 6 answerable. "Which timeout is currently active" is undefined until you
can also see *which layer* supplied it.

---

## 7. UI

A System / Control panel in Maud + htmx, matching the existing stack:

- **Pause controls** — per-module, with the 1h/6h/12h/24h/3d/7d dropdown plus
  "indefinite", showing the current expiry as an absolute timestamp when paused.
- **Drain button** — with a confirmation step and a live remaining-job count
  while draining.
- **Effective settings table** — every knob with its current value and a badge
  reading `default`, `env`, or `db`.

That badge is the antidote to the failure mode the precedence chain otherwise
invites: an operator changing a value and not understanding why nothing happened.

Live updates reuse the existing `ActivityBroadcaster` SSE path rather than
introducing polling.

### 7.1 Timings

Every time value the system is currently operating under is surfaced, not just
the tunables. Anything that is counting down is shown **both** as an absolute
timestamp and as a live relative countdown — an absolute time is what you need
to reason about a schedule, a countdown is what you need to decide whether to
wait.

| Displayed | Source |
|---|---|
| Indexing pause expiry | `indexing_paused_until` (or "indefinite") |
| Downloads pause expiry | `downloads_paused_until` (or "indefinite") |
| Drain deadline and time remaining | Drain start + `drain_timeout_secs` |
| Time drained so far | Drain start |
| Next cleanup run | Last cleanup + `cleanup_interval_secs` |
| Next scheduler tick | Last tick + `check_interval_secs` |
| Download timeout | `DownloadConfig::timeout` |
| Indexing / yt-dlp command timeout | `DEFAULT_TIMEOUT` (`patches/yt-dlp-patched/src/client/mod.rs:19`) |
| Minimum re-index interval per source | `MIN_INDEX_INTERVAL_SECS` |
| Inter-invocation rate-limit delay | `rate_limit_delay_secs` (mutable) |

Three of these are **read-only**: the download timeout, the yt-dlp command
timeout, and the minimum re-index interval are compiled-in or env-derived and are
not part of the runtime-mutable set. They are displayed anyway, with a `default`
or `env` provenance badge, because they are exactly the "what timeouts are
currently active" that issue #130 item 6 asks for. Promoting any of them into the
mutable set later is additive: a nullable column plus a resolver entry.

Countdowns are rendered client-side from a served absolute timestamp rather than
re-fetched, so a ticking display costs no requests. The `GET /system/status`
extension (§6) carries these timestamps.

---

## 8. Error handling

- **Listener disconnect.** `PgListener` reconnects with backoff. On reconnect the
  task performs a full re-read rather than assuming it missed nothing, because a
  notification delivered while disconnected is lost — `NOTIFY` is fire-and-forget.
  This full-resync-on-reconnect is required for correctness, not a nicety.
- **Startup with the database unavailable.** Settings load is part of existing
  startup, which already fails loudly on an unreachable database. No new path.
- **Invalid values.** Rejected at both the API layer and by `CHECK` constraints.
- **Watch channel lag.** Not possible — `watch` holds only the latest value.
- **Drain watcher never settles.** Bounded by `drain_timeout_secs` (§5.2).

---

## 9. Testing

- **Resolver** — unit tests over the precedence matrix: every combination of
  default / env / DB present or absent, asserting both value and provenance.
- **Pause encoding** — `NULL`, future timestamp, and `infinity` round-trip
  through the database and resolve correctly.
- **Expiry** — with `tokio::time` paused, assert republish occurs at the deadline
  and that a pause set to `infinity` never schedules one.
- **Gates** — a paused scheduler spawns no indexers while in-flight ones finish;
  a paused supervisor leaves videos `pending` without dispatching.
- **Semaphore resize** — grow is immediate; shrink is observed as permits return.
- **Drain** — reaches quiescence and signals shutdown; separately, exceeding the
  timeout still shuts down.
- **API** — endpoint tests alongside the existing suite in
  `crates/hof-api/src/routes/tests.rs`.

Tests run against the dedicated ephemeral `postgres-test` service on
localhost:5433 via `just test`, per `AGENTS.md`.

---

## 10. Implementation constraints

The workspace adopted substantially stricter lints in `dc9598f`
(`Cargo.toml` `[workspace.lints.clippy]`). Several bear directly on this feature:

| Lint | Consequence |
|---|---|
| `exit = "deny"` | Shutdown must return from `main`, never `std::process::exit`. §5.3 complies. |
| `arithmetic_side_effects = "deny"` | All deadline and permit-delta math must be checked or saturating. This feature is largely made of such arithmetic. |
| `unchecked_time_subtraction = "deny"` | Duration computations for `sleep_until` must use checked subtraction. |
| `as_conversions = "deny"` | Collides with the existing `max_concurrent as usize` at `download_supervisor.rs:120`, which §4.5 touches anyway; use `usize::try_from`. |
| `unwrap_used` / `expect_used` / `panic` = `deny` | Production paths must propagate errors. `clippy.toml` permits test-only panic helpers. |

Verification per task: `just lint`, `just test`, and `just prepare` after any
schema change (offline sqlx data must be regenerated and committed).

---

## 11. Sequencing

The dependency order is strict for the first three; item 6's UI depends on
precedence being settled, since provenance is meaningless before then.

1. Migration, `EffectiveSettings`, and the resolver (with the precedence tests).
2. `RuntimeConfig` handle, listener task, expiry deadline, `startup.rs` wiring.
3. Consumer adoption — the three knob shapes across scheduler, supervisor, cleanup.
4. Pause gates in scheduler and supervisor.
5. Drain token, drain watcher, `main.rs` select arm.
6. API endpoints.
7. UI panel (items 5 + 6, merged).

---

## 12. Decision records

- [ADR-0001 — Runtime config propagation](../../adr/0001-runtime-config-propagation.md)
- [ADR-0002 — Settings precedence](../../adr/0002-settings-precedence.md)
- [ADR-0003 — Pause encoded as a nullable timestamp](../../adr/0003-pause-as-nullable-timestamp.md)
- [ADR-0004 — Drain state is not persisted](../../adr/0004-drain-state-not-persisted.md)
