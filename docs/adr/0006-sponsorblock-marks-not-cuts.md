# ADR-0006 — SponsorBlock marks chapters; it does not cut the file

**Date:** 2026-09-06
**Status:** Proposed
**Context:** [Plausibility study](../sponsorblock-integration.md) §4

## Context

Given SponsorBlock segments (see [ADR-0005](0005-sponsorblock-segments-via-ytdlp-metadata.md)),
the question is what to do to the file: annotate the sponsor ranges as chapter
markers, or physically remove them.

Removal is what most users picture when they hear "SponsorBlock", and it is what
`--sponsorblock-remove` does in a normal yt-dlp setup. It is also the option
that permanently modifies an archive.

## Options considered

**A. Mark.** Write an ffmetadata chapter list and remux with
`-map_metadata 0 -map_chapters 1 -c copy`. The vendored crate already implements
this as `MetadataManager::add_chapters_metadata`
(`src/metadata/chapters.rs:90`); `hof-core` simply never calls it.

**B. Remove, stream-copied.** yt-dlp's approach minus the re-encode: an
`ffconcat` spec listing the file once per surviving span with
`inpoint`/`outpoint`, copied.

**C. Remove, re-encoded.** As B, preceded by a `-force_key_frames` pass so cuts
land on keyframes. This is what `--force-keyframes-at-cuts` does.

## Decision

**Option A.** B is rejected as incorrect; C is rejected on cost.

### B is not a cheaper removal — it is a broken one

Measured on a synthetic 60s/30fps clip with 5s keyframe spacing (comparable to
real YouTube AVC), cutting `[22.5s, 33.7s]` off keyframe boundaries:

| Check | Expected | Actual |
| --- | --- | --- |
| Duration | 48.800 s | 48.823 s ✅ |
| Video frames | 1464 | **1577** ❌ |
| Decode | clean | **non-monotonic DTS** ❌ |

The duration is right, which is exactly what makes this dangerous — the obvious
check passes. But 113 extra frames survive: the concat demuxer cannot begin
mid-GOP on a copied stream, so it starts at the preceding keyframe (30.0s)
rather than the requested 33.7s.

In practice that means **the tail of every sponsor read stays in the file**,
timestamps are corrupted, and the operation reports success. It destroys data
*and* fails at its purpose. There is no configuration that fixes this; it is
inherent to copying a compressed stream.

### C is correct but priced wrong for this workload

Forcing keyframes does work — requesting `-force_key_frames 22.5,33.7` produced
keyframes at exactly those times. But it re-encodes the whole video stream.
Measured with libx264 `preset medium` on 1080p30, 6 cores: **2.11× realtime**,
so roughly **19 minutes of all-core CPU for a 40-minute video**.

Both caveats point the same way: `testsrc2` is far easier to encode than real
footage, and a real deployment shares those cores with Jellyfin and concurrent
downloads. On a self-hosted box this is not a background task, it is the
workload — and it would apply to every video the profile touches.

It is also generation loss, in an archive whose purpose is to hold the good copy.

### What A costs

One stream-copy remux, measured at **307 MB/s** — about **7 seconds for a 2 GB
file**. Verified to round-trip exact timestamps in both containers this project
produces:

| Container | Chapters read back | Timestamps | Size delta |
| --- | --- | --- | --- |
| `mp4` (`Browser`/`Tv`) | 3/3 | exact | +767 bytes |
| `mkv` (`Auto`) | 3/3 | exact | −34 KB |

The media streams are untouched, so the operation is reversible and cannot
degrade quality.

### The asymmetry that settles it

Segment data is crowd-sourced and occasionally wrong or vandalised. Under A a
bad segment costs a misplaced chapter marker. Under C it costs footage, from a
file kept precisely because it is the copy that still exists. The failure modes
are not comparable, and marking still delivers the actual user benefit —
one keypress to skip — in any player that reads chapters, Jellyfin included.

## Consequences

- Sponsor segments remain in the archived file. This is a deliberate trade, and
  should be stated plainly in the UI so nobody expects deletion.
- Playback-time skipping depends on the client honouring chapters. Jellyfin
  does; some clients only expose chapters as seek points.
- One extra whole-file rewrite per download, in `incomplete/` before
  `move_to_completed`, so the existing move stays the atomicity boundary and a
  failed remux never reaches the completed tree. Peak disk use per video roughly
  doubles, transiently.
- `Quality::AudioOnly` disables extension forcing, so the embedding step must
  read the real extension rather than assume the profile's container.
- Delivered-quality recording (`video_height`, `video_codec`) is unaffected —
  the remux is a stream copy.
- **If removal is revisited**, §4.3 of the study is the number to argue against,
  and it needs a new ADR superseding this one — not a flag added quietly beside
  the marking path.
