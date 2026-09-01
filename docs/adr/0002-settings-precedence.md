# ADR-0002 — Settings precedence: default, then env, then database

**Date:** 2026-09-01
**Status:** Accepted
**Context:** [Issue #130](https://github.com/Mozart409/hofvarpnir/issues/130), [design](../superpowers/specs/2026-09-01-runtime-control-plane-design.md)

## Context

Introducing database-backed settings creates a third configuration layer
alongside the compiled-in defaults and the existing environment variables. Their
precedence must be defined explicitly.

Existing deployments configure hofvarpnir entirely through environment
variables and must not break.

## Options considered

**A. Database wins; environment is a fallback.** A `NULL` column means "not set
here" and falls through to the env var, then the compiled default.

**B. Environment wins as a hard override.** If an env var is set, the knob is
locked and the UI shows it read-only. Good for pinning production values, but
produces "I changed it and nothing happened" confusion.

**C. Database only.** Remove these env vars entirely. Cleanest model, but a
breaking change for existing deployments.

## Decision

**Option A: code default → environment variable → database, database winning.**

Nullable columns represent "unset at this layer," which yields three properties
at once: existing env-configured deployments keep working untouched, the UI
becomes authoritative the moment a knob is touched, and "reset to default" has a
natural representation (set the column back to `NULL`).

## Consequences

- Every tunable column is nullable. `NULL` is meaningful, not merely absent.
- A resolver combines the row with `Config` into an `EffectiveSettings` value.
  Actors consume only resolved values and never see the layering.
- **The resolver must also report provenance** (`Default` / `Env` / `Database`)
  per field, and the UI must display it as a badge. Without this, a three-layer
  chain is opaque, and an operator cannot tell why a value is what it is. This
  is the single most likely source of future confusion, so surfacing provenance
  is a requirement of this decision rather than a UI nicety.
- Issue #130's item 6 ("show what timeouts are active") is only well-defined
  once this precedence exists — "active" means "whatever won the chain."
