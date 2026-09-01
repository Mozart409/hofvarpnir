# ADR-0001 — Runtime config propagation via Postgres NOTIFY and watch channels

**Date:** 2026-09-01
**Status:** Accepted
**Context:** [Issue #130](https://github.com/Mozart409/hofvarpnir/issues/130), [design](../superpowers/specs/2026-09-01-runtime-control-plane-design.md)

## Context

Settings must be changeable at runtime and observed by live actors
(`SchedulerActor`, `DownloadSupervisor`, `CleanupActor`) without a restart.
Today all configuration is read from environment variables once, at startup,
in `hof-core/src/config.rs`.

Deployment is a single container today, but must not be foreclosed from running
multiple processes against one database later.

## Options considered

**A. In-process broadcaster only.** A `watch`-channel handle threaded through
`startup.rs`, mirroring the existing `ActivityBroadcaster`. Least new code and a
perfect match for the codebase idiom, but config exists only in memory: it does
not survive a restart and can never span processes.

**B. Database as source of truth, `LISTEN`/`NOTIFY` fanned out to watch
channels.** A singleton table is authoritative; a trigger notifies; one listener
task re-reads and publishes over the same watch channels as A.

**C. Database plus polling.** Actors re-read on their existing tick. No listener
task, but change latency equals the tick interval — up to 60s for the scheduler
and 3h for cleanup, which makes a "pause now" control feel broken.

## Decision

**Option B.**

B *is* A plus a listener task, so the increment over the simplest option is
small, while it buys two things A structurally cannot:

1. **Pause survives restart.** A pause exists to hold back load; a container
   restart silently resuming it is precisely the failure the pause was meant to
   prevent.
2. **Multi-instance is a no-op later.** The listener already exists, so scaling
   out requires no redesign.

C is rejected on latency: an operator-facing control must take effect promptly.

`NOTIFY` is emitted from an `AFTER INSERT OR UPDATE` **trigger**, not from the
API handler, so that any writer is caught — the API, a future admin tool, or a
manual `psql` session.

`watch` is chosen over `broadcast` because consumers only care about the latest
value, late subscribers must immediately see current state, and a slow consumer
must not accumulate a backlog.

## Consequences

- A new long-lived listener task owning a `PgListener`.
- **`NOTIFY` is fire-and-forget**: a notification sent while the listener is
  disconnected is lost. The listener therefore performs a full re-read on every
  reconnect. This is required for correctness, not an optimization.
- The database becomes a hard dependency for config changes — already true for
  all application state.
- Actors gain a `watch::Receiver` and are unaware of the database.
