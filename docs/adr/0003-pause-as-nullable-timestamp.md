# ADR-0003 — Pause encoded as a single nullable timestamp

**Date:** 2026-09-01
**Status:** Accepted
**Context:** [Issue #130](https://github.com/Mozart409/hofvarpnir/issues/130), [design](../superpowers/specs/2026-09-01-runtime-control-plane-design.md)

## Context

Two pause behaviors are required: a timed pause that auto-resumes after a chosen
duration (1h / 6h / 12h / 24h / 3d / 7d), and an indefinite "pause all" that
persists until explicitly resumed.

## Options considered

**A. Boolean `paused` plus a nullable `paused_until`.** The obvious shape, but it
admits contradictory states — `paused = false` with a future `paused_until`, or
`paused = true` with an elapsed one — that every reader must then defend against.

**B. A single nullable `TIMESTAMPTZ`**, using Postgres's native `infinity` value
for the indefinite case.

**C. Separate columns per mode.** Most explicit, most redundant, and multiplies
the invalid-state surface further.

## Decision

**Option B.** One column per pausable module:

- `NULL` — running.
- A future timestamp — paused until then, auto-resuming.
- `'infinity'::timestamptz` — paused indefinitely.

Postgres supports `infinity` for `timestamptz` natively, so both requirements
collapse into one column with **no mode flag and no representable contradiction**.
The UI dropdown computes `now() + interval`; "pause all" writes `infinity`.

## Consequences

- Every read is the same comparison — `paused_until > now()` — regardless of mode.
- Expiry requires no write: a pause lapses because time passes. See
  [ADR-0001](0001-runtime-config-propagation.md); the listener task arms a
  `sleep_until` for the nearest deadline and republishes when it fires. An
  `infinity` pause correctly schedules no deadline.
- Under a future multi-instance deployment, each process observes expiry
  independently with no race over which one clears the flag.
- Code touching these columns must handle `infinity`, which does not convert to
  all `chrono` representations cleanly. Serialization to the API is explicitly a
  nullable timestamp plus an `indefinite` boolean, so clients never receive a
  literal `"infinity"` string to parse.
