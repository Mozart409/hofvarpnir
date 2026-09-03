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
- A finite sentinel timestamp far in the future — paused indefinitely. (The
  original decision here was `'infinity'::timestamptz`; see **Amendment**
  below for why that was replaced before shipping and what actually landed.)

Both requirements collapse into one column with **no mode flag and no
representable contradiction**. The UI dropdown computes `now() + interval`;
"pause all" writes the indefinite sentinel.

## Consequences

- Every read is the same comparison — `paused_until > now()` — regardless of mode.
- Expiry requires no write: a pause lapses because time passes. See
  [ADR-0001](0001-runtime-config-propagation.md); the listener task arms a
  `sleep_until` for the nearest deadline and republishes when it fires. The
  indefinite sentinel correctly schedules no deadline (see Amendment).
- Under a future multi-instance deployment, each process observes expiry
  independently with no race over which one clears the flag.
- Serialization to the API is explicitly a nullable timestamp plus an
  `indefinite` boolean, so clients never receive the sentinel's raw value to
  parse or compare against.

## Amendment (2026-09-02): `infinity` does not work, replaced with a finite sentinel

The one-nullable-column-per-module shape above is what shipped and remains
correct. The specific encoding of "indefinite" as `'infinity'::timestamptz`
does not, and was never actually reachable in the shipped code — it is
recorded here rather than edited away because the failure, and what it took
to fix, is the useful part of this decision.

**Why `infinity` fails.** `sqlx` decodes a binary `timestamptz` column as
`postgres_epoch + Duration::microseconds(us)`, with no special case for
Postgres's `infinity`/`-infinity` values, and `chrono`'s `Add` panics on
overflow. Reading back a row where the column literally holds `infinity`
therefore **panics at runtime** — this is not a hypothetical edge case, it is
the code path every read of an indefinitely-paused module would take.

**First fix attempt also failed.** Using `DateTime::<Utc>::MAX_UTC` as the
indefinite sentinel avoids the decode panic, but carries nanosecond
precision that Postgres's microsecond-resolution `timestamptz` truncates
away on write. A value written as `MAX_UTC` and read back no longer equals
`MAX_UTC` — the sentinel stopped comparing equal to itself after a single
round trip, so an equality guard against it silently stopped matching, with
no error or panic to surface the bug.

**What shipped.** `hof_core::runtime_config::indefinite_pause()` returns a
sentinel built entirely from a whole microsecond count —
`9999-12-31T23:59:59Z`, `253_402_300_799_000_000` microseconds since the Unix
epoch — via `DateTime::from_timestamp_micros`, so it round-trips through
Postgres bit-for-bit and compares equal to itself after a write/read cycle.
Migration `20260902120000_add_runtime_settings_pause_finite_check` adds
`CHECK (isfinite(indexing_paused_until))` and the equivalent for
`downloads_paused_until`, so a literal `infinity` can no longer be written to
either column at all — the database itself now rejects the original
encoding.

Every reference to `infinity` above describes the decision as originally
made, kept intact for the record; nothing currently reads or writes a
literal `infinity` value.
