# ADR-0007 — SponsorBlock is configured per profile, as a category list

**Date:** 2026-09-06
**Status:** Proposed
**Context:** [Plausibility study](../sponsorblock-integration.md) §5.3, §5.4,
[ADR-0002](0002-settings-precedence.md)

## Context

Chapter marking ([ADR-0006](0006-sponsorblock-marks-not-cuts.md)) needs a switch.
This repo has two established homes for configuration, and they mean different
things:

- **`profiles`** — per-user download preferences that describe *what the output
  should look like*: `quality`, `output_preset`, `naming_template`,
  `include_shorts`.
- **`runtime_settings`** — a singleton table of operator controls, live-reloaded
  through Postgres `NOTIFY` and watch channels ([ADR-0001](0001-runtime-config-propagation.md)):
  pause, concurrency caps, tick intervals.

## Options considered

**A. Per-profile column.** Mirrors `output_preset` exactly — column, domain
field, API field, web form control.

**B. Global `runtime_settings` column.** One instance-wide switch,
live-reloadable through plumbing that already exists, no migration to profiles or
forms.

**C. Both — per-profile opt-in plus a global kill switch.**

## Decision

**Option A**, storing a **category array** rather than a boolean.

### Why the profile, not runtime settings

The distinction in ADR-0001 and ADR-0002 is that `runtime_settings` holds
controls an operator reaches for *while the system is running* — to shed load or
stop the world. SponsorBlock marking is not that. It is a property of the
artifact, in the same family as `output_preset`: it describes what a finished
file should contain. A profile targeting a TV and one archiving a music channel
legitimately want different answers, and B cannot express that.

C was rejected as premature. The global kill switch it adds answers "SponsorBlock
is down" — but a failed lookup already degrades to zero segments without failing
the download, so there is nothing to switch off. Adding a second precedence rule
to the one ADR-0002 already defines needs a real problem behind it.

### Why a category list, not a boolean

`sponsorblock_categories TEXT[] NOT NULL DEFAULT '{}'` — empty means disabled.

This costs nothing over a boolean and avoids a near-certain second migration:
"mark sponsors but not intros" is an ordinary preference, and the yt-dlp flag
takes a category list regardless, so a boolean would only hardcode one. Folding
the enable flag into emptiness also removes the possibility of a boolean and a
list disagreeing.

`TEXT[]`, not a Postgres enum array, because the category vocabulary is already
unstable: yt-dlp's `SponsorBlockPP` knows `hook`, which the server's `Category`
type does not list, and the server knows `exclusive_access`, which yt-dlp does
not. An enum would turn an upstream addition into a migration, and an unknown
value from a newer yt-dlp into a hard parse failure on an unrelated code path.

Suggested default when a profile opts in: `sponsor`, `selfpromo`, `interaction`,
`intro`, `outro`. Excluded deliberately — `music_offtopic` (wrong for music
sources), `filler` (highly subjective), `poi_highlight` and `chapter` (not
sponsor-ish; `chapter` would collide with the video's own chapters).

### The API endpoint is not a profile setting

`SPONSORBLOCK_API_URL` belongs in `Config` beside `ytdlp_path`, read from the
environment at startup. It is deployment topology — which host to talk to — not
a per-download preference, and it should be identical for every profile in an
instance.

## Consequences

- One additive migration; no data backfill, since the default disables the
  feature and existing profiles keep today's behaviour.
- The change touches the same eight-file surface `output_preset` already
  occupies, which makes it a mechanical change rather than a design one.
- Validation belongs at the API boundary: an unknown category should be rejected
  on write, not discovered when yt-dlp refuses the flag mid-download.
- **Segments are not persisted.** The metadata call already returns them at no
  measurable cost (study §3.2), the file on disk is the artifact, and a segments
  table would raise a cache-invalidation question that marking does not have.
- No live reload. A profile change takes effect on the next download, which is
  the same behaviour as every other profile field.
- If a global kill switch is ever needed, it can be added to `runtime_settings`
  later without disturbing the per-profile column — but it will need its own
  precedence rule under ADR-0002.
