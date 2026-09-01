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

**Drain state is an in-memory `CancellationToken` in `AppState` and is
deliberately never persisted**, in contrast to pause.

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
  that wants the process to stay down must use `restart: on-failure`. This must
  be documented in the README.
