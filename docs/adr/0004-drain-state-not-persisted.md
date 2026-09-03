# ADR-0004 — Drain state is process-local, not persisted

**Date:** 2026-09-01
**Status:** Accepted
**Context:** [Issue #130](https://github.com/Mozart409/hofvarpnir/issues/130), [design](../superpowers/specs/2026-09-01-runtime-control-plane-design.md)

## Context

Issue #130 item 4 asks for a UI control that drains in-flight work and then shuts
the process down, leaving the UI and API responsive throughout.

[ADR-0001](0001-runtime-config-propagation.md) establishes the database as the
source of truth for runtime settings, and [ADR-0003](0003-pause-as-nullable-timestamp.md)
persists pause state there so it survives restarts. Drain superficially looks
like a third pause mode and could plausibly live in the same table.

## Decision

**Drain state is process-local and deliberately never persisted**, in contrast
to pause. (The concrete type is a `watch`-channel-based `DrainToken`, not the
`CancellationToken` this ADR originally named — see the Amendment below.)

## Rationale

A drain is process lifecycle, not configuration. If it were persisted, a
container that restarted mid-drain — which is the *expected* outcome under
`restart: always`, since draining ends in process exit — would come back up
already refusing all work, wedged, with no visible cause and no obvious way for
an operator to connect the symptom to the earlier drain.

Making drain process-local means a restart clears it by construction. The
asymmetry with pause is intentional and is the point: pause is a decision about
*load*, which should outlive a restart; drain is a decision about *this process*,
which should not.

## Consequences

- Drain reuses the same gates as pause (§4.4 of the design), differing only in
  where the signal originates. No second refusal path.
- `POST /api/v1/system/shutdown` is not idempotent across restarts, and cannot be
  — that is the intent.
- The drain must be bounded (`drain_timeout_secs`, default 1800). A download may
  legitimately run for hours, so an unbounded drain is indistinguishable from a
  hang. Exceeding the bound proceeds to shutdown regardless; this is safe because
  `startup.rs::recover_from_crash` already resets `downloading` rows to `pending`
  and removes orphaned `.part` files on the next boot.
- Exit is by returning from `main` with code 0. `std::process::exit` is forbidden
  workspace-wide (`exit = "deny"`). **Under `restart: always` or
  `unless-stopped`, an exit-0 container restarts immediately**, so a deployment
  that wants the process to stay down must use `restart: on-failure`. This is
  documented in the README's "Runtime control" section.

## Amendment (2026-09-03) — the concrete type, and a frozen deadline

The decision above is unchanged and is what shipped: drain state is
process-local and never persisted. Two mechanism details differ from the
original text.

**1. `tokio::sync::watch`, not `CancellationToken`.** The design named
`CancellationToken`, which would have required adding `tokio-util` as a
dependency for no gain. An intermediate attempt used `tokio::sync::Notify` and
was **broken**: `Notify::notify_waiters()` stores no permit and wakes only
waiters already parked, so draining an *idle* instance reached quiescence
before `main`'s shutdown arm was polled — the wakeup was dropped and `main`
parked forever on a shutdown that had already completed. `DrainToken` ships as
two `watch` channels (`started`, `complete`), which are level-triggered: a
watcher that subscribes after the fact still observes the signal. As a bonus,
`send_replace`/`send_if_modified` are infallible, so none of the poisoned-lock
recovery an `RwLock` version would have needed exists.

**2. The deadline is frozen at drain start.** `begin(now, timeout)` records a
`DrainStart { started_at, deadline }`, and `deadline()` returns that stored
value rather than recomputing from a timeout supplied per call. Originally the
timeout was a parameter of `deadline(..)`, which meant the drain watcher (which
read it once, correctly) and the HTTP API (which re-read it per request)
could disagree: an operator retuning `drain_timeout_secs` *during* a drain saw
`GET /system/status` report a deadline that the watcher was not enforcing.
Freezing it at `begin` makes the single-value guarantee structural rather than
a convention each caller must remember. `begin` remains first-write-wins, so a
repeated `POST /api/v1/system/shutdown` returns the original deadline.
