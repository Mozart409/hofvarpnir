# ADR-0005 — SponsorBlock segments arrive through the yt-dlp metadata call

**Date:** 2026-09-06
**Status:** Proposed
**Context:** [Plausibility study](../sponsorblock-integration.md) §2, §3

## Context

We want SponsorBlock segments for downloaded videos. The obvious route —
yt-dlp's `--sponsorblock-remove` / `--sponsorblock-mark` writing the file — is
unavailable, and understanding *why* determines every remaining option.

`hof-core` does not drive the yt-dlp CLI. It depends on the vendored `yt-dlp`
Rust crate (`patches/yt-dlp-patched/`), in which the yt-dlp binary is invoked
only for `--dump-single-json` metadata extraction
(`src/extractor/mod.rs:264`). Media is fetched natively over `reqwest` through
the crate's own `DownloadManager`, and muxed by a direct ffmpeg invocation.

yt-dlp's SponsorBlock support is a **post-processor**. It acts on a file yt-dlp
downloaded. Here yt-dlp downloads nothing, so the flags are inert — not broken
in a visible way, just silently without effect.

The *fetching* half of `--sponsorblock-mark` runs earlier, though, at yt-dlp's
`after_filter` stage, which does execute under `--dump-single-json`. Verified
against yt-dlp 2026.08.19: the dumped JSON carries a `sponsorblock_chapters`
array.

## Options considered

**A. Native SponsorBlock client in `hof-core`.** Call
`/api/skipSegments/{sha256[..4]}` with `reqwest` and deserialize into our own
types. Full control over caching, retries and category filtering, and no
vendored-crate patch. But it reimplements work yt-dlp has already done and
tested: the stale-segment duration filter, sub-second start snapping,
zero-length POI widening, the `404`-means-empty convention, and the fact that
`locked` deserializes from `0`/`1` rather than a JSON boolean. It also adds a
second outbound network dependency on a path that already has one.

**B. Segments ride along on the existing metadata call.** Add
`--sponsorblock-mark <categories>` to the extractor invocation and add a
`sponsorblock_chapters` field to the vendored `Video` model so the data is no
longer dropped during deserialization.

**C. A second yt-dlp invocation purely for segments.** Keeps the vendored crate
untouched, at the cost of a whole extra subprocess and YouTube round-trip per
video, for data the first call could have returned.

## Decision

**Option B.**

The deciding measurement is that B is free. Median of three runs on the same
video: 2.39s without the flag, 2.33s with it — the SponsorBlock fetch (~122 ms)
overlaps YouTube extraction and disappears into the noise. C pays a full second
extraction for the same bytes.

Against A, the argument is that the fiddly parts of this problem are all in the
*interpretation* of the segment list, not the fetching of it. A stale segment
from a re-uploaded video, a POI marker with zero length, and a `404` that means
"nothing to do" are each a small bug waiting to be written; yt-dlp has already
written and fixed them. Option A's advantages — caching and retry control —
answer problems the 2.33s measurement says we do not have.

The cost is one added field in the vendored crate. That is an established
practice here, recorded in `PATCHES.md`, and unlike the existing playlist
patches this one is **purely additive**: it changes no upstream type, so
re-syncing against upstream should stay clean.

Because the flag must reach the *extractor*, not the downloader, it is pushed
via `ExtractorConfig::with_arg`. `DownloaderBuilder::with_args` looks like the
right hook and is not: those args live on `Downloader` and are never forwarded
to the extractors, which `build` constructs fresh (`src/client/builder.rs:374`).
Only cookie and netrc settings are propagated today.

## Consequences

- The vendored crate gains `Video::sponsorblock_chapters`, with `#[serde(default)]`
  so the field is absent-tolerant. `PATCHES.md` must record it in the same change.
- **Segments exist only when the flag was passed.** The field is empty both for
  "no segments in the database" and "we did not ask", and nothing downstream can
  distinguish those. Anything that needs the distinction must carry it
  separately.
- We inherit yt-dlp's category vocabulary, which already differs from the
  server's — yt-dlp adds `hook` and omits `exclusive_access`. Categories are
  therefore stored as open `TEXT`, never a Postgres enum.
- A yt-dlp upgrade can change segment interpretation without any change here.
  That is mostly the point, but it means the deserialization fixture test is
  load-bearing.
- `SPONSORBLOCK_API_URL` maps to yt-dlp's `--sponsorblock-api`, so pointing at a
  self-hosted mirror stays a one-line config change.
