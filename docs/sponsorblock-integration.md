# SponsorBlock integration — plausibility study

**Date:** 2026-09-06
**Status:** Exploration. No implementation exists; nothing in this document has been built.
**Decisions:** [ADR-0005](adr/0005-sponsorblock-segments-via-ytdlp-metadata.md),
[ADR-0006](adr/0006-sponsorblock-marks-not-cuts.md),
[ADR-0007](adr/0007-sponsorblock-scoped-per-profile.md)

## Summary

Integrating SponsorBlock is feasible and cheap, but **not by the route the yt-dlp
CLI documentation suggests**. `--sponsorblock-remove` cannot work in this
codebase, because yt-dlp never performs the download here (§2).

The workable route is narrow and well-supported: ask the *existing* metadata
call for segments, and embed them as chapter markers.

| Question | Answer | Evidence |
| --- | --- | --- |
| Can we use `--sponsorblock-remove`? | **No** | §2 — yt-dlp is metadata-only in this pipeline |
| Can we get segments at all? | **Yes, for free** | §3 — `--dump-single-json --sponsorblock-mark all` emits `sponsorblock_chapters`, no measurable latency cost |
| Cost of marking (chapters)? | **~7s per 2 GB, lossless** | §4.1 — one stream-copy remux at 307 MB/s |
| Cost of removing (cutting)? | **~19 min CPU per 40-min 1080p video** | §4.3 — correct cuts require a full re-encode |
| Is stream-copy cutting a shortcut? | **No — it is silently wrong** | §4.2 — leaves ~3.8s of sponsor in the file and breaks timestamps |
| New runtime dependencies? | **None** | §3.2 — one field added to the vendored `Video` model |

Recommended scope: **mark-only, opt-in per profile, configurable API endpoint.**

## 1. What SponsorBlock is

A crowd-sourced database mapping a video ID to time ranges that viewers have
labelled as sponsor reads, intros, self-promotion and so on. It is queried over
a public HTTP API at `https://sponsor.ajay.app`.

### 1.1 The endpoint we would use

```
GET /api/skipSegments/{sha256(videoID)[0..4]}?service=YouTube&categories=[...]&actionTypes=[...]
```

The video ID is **not** sent. Only the first four hex characters of its SHA-256
are, and the server returns every video in that bucket. The client filters
locally for the one it wants. This is the form yt-dlp uses, and the form any
integration here should use.

Measured against the live API on 2026-09-06 (video `dQw4w9WgXcQ`, prefix `5f6b`,
six categories requested):

| Property | Measured |
| --- | --- |
| Videos returned in the bucket | 124 |
| Segments returned | 210 |
| Response size | 48 KB |
| Round-trip | 122 ms |

So the privacy property costs roughly 48 KB and one request per video — an
entirely acceptable trade for not disclosing a viewing history to a third party.

### 1.2 Response shape

```json
{
  "videoID": "601HYb7gFz0",
  "segments": [
    {
      "category": "sponsor",
      "actionType": "skip",
      "segment": [285.331, 347.819],
      "UUID": "24da9aba9ef2d57b...",
      "videoDuration": 946.141,
      "locked": 0,
      "votes": 3,
      "description": ""
    }
  ]
}
```

Two details that will bite a hand-rolled deserializer:

- **`locked` is returned as `0`/`1`, not a JSON boolean**, despite the server's
  own TypeScript typing it as `boolean`. A `bool` field will fail to parse.
- **The API returns `404`, not an empty array, when a video has no segments.**
  A "no segments" result is the common case and must not be treated as an error.

Neither matters under [ADR-0005](adr/0005-sponsorblock-segments-via-ytdlp-metadata.md),
because yt-dlp already handles both — which is part of the argument for that ADR.

### 1.3 Categories

The server (`src/types/segments.model.ts`) recognises: `sponsor`, `selfpromo`,
`interaction`, `intro`, `outro`, `preview`, `music_offtopic`, `poi_highlight`,
`chapter`, `filler`, `exclusive_access`.

yt-dlp's `SponsorBlockPP` knows a slightly different set — it adds `hook` and
omits `exclusive_access`. Categories are therefore **not a fixed enum we can
safely close over**; the two upstreams already disagree. Any persisted category
representation should be an open `TEXT`, not a Postgres enum.

`actionType` is one of `skip`, `mute`, `chapter`, `full`, `poi`. Only `skip`,
`chapter` and `poi` are meaningful for a file on disk — `mute` and `full`
describe player behaviour.

### 1.4 Rate limits and terms

In the SponsorBlockServer source, `rateLimitMiddleware` is applied only to the
**vote** and **view** endpoints (`src/app.ts`). No application-level rate limit
is attached to `skipSegments` reads. This says nothing about CDN or reverse-proxy
limits in front of the public instance, so an integration should still back off
politely on `429` and treat SponsorBlock as strictly optional — a failed lookup
must never fail a download.

> **Open question — must be resolved before implementation.** The SponsorBlock
> wiki (`wiki.sponsor.ajay.app`) is behind an anti-scraping proof-of-work gate
> and could not be read during this study. Its *Database and API License* page
> was therefore **not** verified. The licence terms for the segment data, and
> any attribution requirement, need to be checked by hand before shipping. Do
> not assume they are permissive.

## 2. Why `--sponsorblock-remove` cannot be used

This is the central architectural finding, and it inverts the obvious plan.

`hof-core` does not drive the yt-dlp CLI. It depends on the `yt-dlp` **Rust
crate**, vendored at `patches/yt-dlp-patched/` and wired in through
`[patch.crates-io]` in the root `Cargo.toml`. In that crate the yt-dlp binary
has exactly one job:

- **Metadata only.** `ExtractorBase::fetch_video_metadata` builds
  `--no-progress --dump-single-json`, runs the binary via `Executor`, writes
  stdout to a temp file and deserializes it
  (`patches/yt-dlp-patched/src/extractor/mod.rs:264`).
- **The media is fetched natively.** Streams are downloaded with `reqwest`
  through the crate's own `DownloadManager`
  (`patches/yt-dlp-patched/src/client/stream_downloads.rs`), including parallel
  ranged requests.
- **Muxing is ffmpeg, invoked directly** by the crate
  (`patches/yt-dlp-patched/src/client/streams/pipeline/combine.rs:537`).

`--sponsorblock-remove` is implemented as a yt-dlp *post-processor*
(`ModifyChaptersPP`) that runs after yt-dlp has downloaded and muxed a file. In
this pipeline yt-dlp downloads nothing, so that post-processor has no file to
act on and never runs. Passing the flag would be silently inert.

The same is true of `--sponsorblock-mark`'s *file-writing* half. Its
*segment-fetching* half, however, runs earlier — which §3 exploits.

## 3. The workable route: segments via the existing metadata call

### 3.1 The experiment

`SponsorBlockPP` runs at yt-dlp's `after_filter` stage, which executes during
`--dump-single-json` — before the skip-download check. So the segments land in
the dumped JSON even though nothing is downloaded.

```console
$ yt-dlp --no-progress --dump-single-json --sponsorblock-mark all \
    "https://www.youtube.com/watch?v=601HYb7gFz0" | jq '.sponsorblock_chapters[0]'
{
  "start_time": 75.152,
  "end_time": 80,
  "category": "selfpromo",
  "title": "Unpaid/Self Promotion",
  "type": "skip",
  "_categories": [["selfpromo", 75.152, 80, "Unpaid/Self Promotion"]]
}
```

Confirmed against yt-dlp 2026.08.19 (the version pinned by `flake.nix`).

This is strictly better than calling the API ourselves, because yt-dlp has
already done the fiddly parts:

- dropped segments whose `videoDuration` disagrees with the real duration
  (stale segments from a re-uploaded video);
- snapped sub-1-second starts to `0` and clamped ends to the duration;
- widened zero-length `poi` markers to 1s so they are representable;
- resolved the `locked` and `404` quirks from §1.2;
- attached a human-readable `title` per category.

### 3.2 Cost

Median of three runs each, same video, same machine:

| Invocation | Median |
| --- | --- |
| `--dump-single-json` | 2.39 s |
| `--dump-single-json --sponsorblock-mark all` | 2.33 s |

The difference is inside the noise floor. The SponsorBlock fetch (~122 ms)
overlaps the YouTube extraction, so it is effectively free. **This removes the
main argument for caching segments in Postgres** — there is no latency to hide.

### 3.3 What has to change in the vendored crate

`patches/yt-dlp-patched/src/model/video.rs` already carries
`pub chapters: Vec<Chapter>`, but has no `sponsorblock_chapters`, so the field
is currently dropped during deserialization. It needs to be added:

```rust
/// SponsorBlock segments, populated only when the extractor was invoked
/// with `--sponsorblock-mark`. Empty otherwise.
#[serde(default)]
pub sponsorblock_chapters: Vec<SponsorBlockChapter>,
```

Note `start_time`/`end_time` are `f64` here, matching the existing `Chapter`
type — not the `OrderedFloat` used in `format.rs`.

Vendoring a field is an established practice in this repo, and
`patches/yt-dlp-patched/PATCHES.md` is the record of every such divergence; it
must be updated in the same change. Unlike the existing playlist patches, this
one is **purely additive** — it changes no upstream type — so it should re-sync
against upstream cleanly.

The flag itself reaches the binary through `ExtractorConfig::with_arg`, which
`Generic` already implements
(`patches/yt-dlp-patched/src/extractor/generic.rs:28`). Note that
`DownloaderBuilder::with_args` does **not** help: those args are stored on
`Downloader` and are never propagated to the extractors, which
`DownloaderBuilder::build` constructs fresh (`src/client/builder.rs:374`). Only
cookie and netrc settings are forwarded today. So the flag has to be pushed onto
the extractor, not the downloader — either by extending the builder to forward
args, or by constructing the extractor in `hof-core`.

## 4. Marking versus removing, measured

### 4.1 Marking is nearly free and lossless

Embedding chapters is an ffmetadata file plus a stream-copy remux
(`-map_metadata 0 -map_chapters 1 -c copy`). The vendored crate **already
implements this** as `MetadataManager::add_chapters_metadata`
(`patches/yt-dlp-patched/src/metadata/chapters.rs:90`) — `hof-core` simply
never calls it today.

Verified locally against both containers this project produces (`mkv` for the
`Auto` preset, `mp4` for `Browser`/`Tv`):

| Container | Chapters read back | Timestamps | Size delta |
| --- | --- | --- | --- |
| `mp4` | 3/3 | exact | +767 bytes |
| `mkv` | 3/3 | exact | −34 KB (container overhead differs) |

Throughput of the remux, measured on a 23 MB file: **307 MB/s**, i.e. roughly
**7 seconds for a 2 GB archive file** — and that is an I/O-bound whole-file
rewrite, so it scales with file size, not duration. Real spinning storage will
be slower than this measurement, which benefited from page cache.

Critically: the operation is **reversible**. The bytes of the media streams are
untouched, and a wrong or malicious segment costs a bad chapter marker, not
destroyed footage.

### 4.2 Stream-copy cutting looks correct and is not

The tempting cheap version of removal is yt-dlp's own approach without the
re-encode: an `ffconcat` spec listing the file once per surviving span, with
`inpoint`/`outpoint`, stream-copied.

Tested on a synthetic 60s / 30fps clip with keyframes every 5s (comparable to
real YouTube AVC streams), cutting `[22.5s, 33.7s]` — deliberately not on
keyframe boundaries:

| Check | Expected | Actual |
| --- | --- | --- |
| Duration | 48.800 s | 48.823 s ✅ |
| Video frames | 1464 | **1577** ❌ |
| Decode | clean | **non-monotonic DTS errors** ❌ |

The duration is right, so a naive check passes — but there are **113 extra
frames, about 3.8 seconds** of content that should have been removed. The concat
demuxer cannot start mid-GOP on a copied stream, so it starts at the preceding
keyframe (30.0s) instead of the requested inpoint (33.7s).

The practical consequence: **the last few seconds of every sponsor read survive
the cut**, and the file carries broken timestamps. This is the worst outcome
available — it destroys data *and* fails to achieve the goal, while reporting
success.

### 4.3 Correct cutting costs a full re-encode

Forcing keyframes at the cut points does work — verified: requesting
`-force_key_frames 22.5,33.7` produced keyframes at exactly 22.500 and 33.700.
But `-force_key_frames` re-encodes the entire video stream.

Measured on synthetic 1080p30 with libx264 `preset medium` on 6 cores:

| Metric | Value |
| --- | --- |
| Encode speed | **2.11× realtime** |
| Extrapolated, 40-min video | **~19 minutes**, saturating all cores |

Two caveats make this an *optimistic* number: `testsrc2` is far easier to encode
than real video, and this box has 6 cores to spare. On a self-hosted box that is
also serving Jellyfin and running concurrent downloads, a 19-minute
all-core burn per video is not a background task — it is the workload.

It is also **generation loss**: the archive would hold a re-encode of a
re-encode.

A smarter variant exists — re-encode only the GOPs containing cut points and
stream-copy the rest — but it is materially more complex than anything currently
in this pipeline, and it does not change the conclusion for a first version.

### 4.4 Conclusion

Marking costs seconds and is reversible. Removing costs either correctness
(§4.2) or twenty minutes of CPU and a generation of quality (§4.3), against an
archive whose entire purpose is to be the good copy. See
[ADR-0006](adr/0006-sponsorblock-marks-not-cuts.md).

## 5. Proposed design

Scope: **embed SponsorBlock segments as chapter markers, opt-in per profile.**

### 5.1 Flow

```
DownloadWorker::execute_download
  └─ YtdlpClient::download_video
       ├─ extractor.fetch_video(url)          ← +"--sponsorblock-mark <categories>"
       │    └─ Video { chapters, sponsorblock_chapters }   ← new field (§3.3)
       ├─ execute_fallback_attempts(...)      ← unchanged
       └─ embed_chapters(result_path, &video) ← NEW: ffmetadata + -c copy remux
            └─ DownloadResult { .., sponsorblock_segments_marked: usize }
  └─ handle_success → move_to_completed → db::mark_video_completed
```

The remux happens in the **incomplete** directory, before
`move_to_completed`, so a failure mid-remux never leaves a half-written file in
the completed tree. The existing `incomplete → completed` move is already the
atomicity boundary; this reuses it rather than inventing a second one.

### 5.2 Touch points

| File | Change |
| --- | --- |
| `patches/yt-dlp-patched/src/model/video.rs` | add `sponsorblock_chapters` field |
| `patches/yt-dlp-patched/PATCHES.md` | record the divergence |
| `crates/hof-core/migrations/` | add `profiles.sponsorblock_categories` |
| `crates/hof-core/src/domain/profile.rs` | field on `Profile` + `ProfileRow` |
| `crates/hof-core/src/db/profile.rs` | select/insert/update the column |
| `crates/hof-core/src/ytdlp.rs` | pass the flag; call chapter embedding |
| `crates/hof-core/src/config.rs` | `SPONSORBLOCK_API_URL` |
| `crates/hof-core/src/actors/download_worker.rs` | thread the setting via `DownloadConfig` |
| `crates/hof-api/src/routes/profiles.rs` | request/response fields |
| `crates/hof-web/src/pages.rs` | profile form control |

This mirrors, almost line for line, the surface that `output_preset` already
occupies — which is the argument for
[ADR-0007](adr/0007-sponsorblock-scoped-per-profile.md).

### 5.3 Schema

Storing the *category list* rather than a boolean costs nothing extra and avoids
a second migration the first time someone wants intros kept but sponsors marked:

```sql
ALTER TABLE profiles
ADD COLUMN sponsorblock_categories TEXT [] NOT NULL DEFAULT '{}';
```

Empty array means disabled — no separate boolean, no chance of the two
disagreeing. `TEXT[]` rather than an enum array, because §1.3 shows the category
vocabulary is not stable across upstreams.

Suggested default for profiles that opt in: `sponsor`, `selfpromo`,
`interaction`, `intro`, `outro`. Deliberately excluded from the default:
`music_offtopic` (wrong for the music use case), `filler` (highly subjective),
`poi_highlight` and `chapter` (not sponsor-ish, and `chapter` would collide with
the video's real chapters).

Segments are **not** persisted. §3.2 shows there is no latency to hide, the file
on disk is the artifact, and a segments table would immediately raise a cache
invalidation question that marking simply does not have.

### 5.4 Configuration

`SPONSORBLOCK_API_URL` (optional, default `https://sponsor.ajay.app`), passed
through to yt-dlp's `--sponsorblock-api`. This costs one line and lets a
privacy-sensitive deployment point at a self-hosted mirror without a code
change. It belongs in `Config` alongside `ytdlp_path`, not in `runtime_settings`
— it is deployment topology, not an operator control.

### 5.5 Interaction with existing behaviour

- **`Quality::AudioOnly`** — chapters work in audio containers too, but
  extension forcing is disabled for this quality, so the container is whatever
  yt-dlp produced. The embedding step must read the actual extension rather than
  assume one.
- **Jellyfin** — chapter markers are read from the container, so
  `crates/hof-core/src/jellyfin.rs` and the `.nfo` generation need no changes.
- **Non-YouTube platforms** — SponsorBlock's `Service` enum covers YouTube,
  Spotify and PeerTube; yt-dlp's `SponsorBlockPP` maps only `Youtube`. For every
  other platform this is a no-op, which is the correct behaviour, but the UI
  should not imply otherwise.
- **`video_height` / `video_codec`** — untouched. The remux is a stream copy, so
  delivered-quality recording stays accurate.

## 6. Risks

| Risk | Severity | Mitigation |
| --- | --- | --- |
| Segment data licence unverified (§1.4) | **Blocking** | Verify before implementation |
| SponsorBlock API down or slow | Low | yt-dlp already degrades to zero segments; never fail a download on it |
| Remux fails, leaving a temp file | Low | Runs in `incomplete/`; existing cleanup covers it |
| Remux doubles peak disk use per video | Low | Transient, one file, in `incomplete/` |
| Wrong/vandalised segments | Low | Marking is reversible; a bad chapter is cosmetic |
| Upstream re-sync conflict | Low | The patch is purely additive (§3.3) |
| Category vocabulary drifts | Low | `TEXT[]`, not an enum (§1.3, §5.3) |

## 7. Suggested phasing

1. **Verify the licence** (§1.4). Blocking; everything below assumes it clears.
2. **Vendored crate**: add the field, update `PATCHES.md`, add a deserialization
   test against a captured JSON fixture.
3. **Plumbing**: config var, profile column, `DownloadConfig`, flag construction.
   No file writing yet — log what *would* be marked. This is independently
   shippable and de-risks the rest.
4. **Embedding**: call the chapter writer in `incomplete/` before
   `move_to_completed`.
5. **Surface**: API field, web form control, and a badge showing how many
   segments were marked (alongside `delivered_quality_badge`).

Steps 3–5 are each independently revertible. If removal is ever revisited, §4.3
is the number to argue against, and it should be a separate ADR superseding
[ADR-0006](adr/0006-sponsorblock-marks-not-cuts.md).

## Appendix — reproducing the measurements

All figures in this document came from these commands on 2026-09-06
(yt-dlp 2026.08.19, ffmpeg 9.0, 6 cores).

```bash
# §1.1 — hash-prefix bucket size and latency
VID=dQw4w9WgXcQ; H=$(printf '%s' "$VID" | sha256sum | cut -c1-4)
curl -sS -w '\nHTTP %{http_code} %{time_total}s %{size_download}B\n' \
  "https://sponsor.ajay.app/api/skipSegments/$H?service=YouTube&categories=%5B%22sponsor%22%5D"

# §3.1 — segments in the metadata dump
yt-dlp --no-progress --dump-single-json --sponsorblock-mark all \
  "https://www.youtube.com/watch?v=601HYb7gFz0" | jq '.sponsorblock_chapters'

# §4.1 — chapter round-trip, mp4 and mkv
ffmpeg -y -f lavfi -i "testsrc2=size=320x240:rate=30:duration=60" \
  -f lavfi -i "sine=frequency=440:duration=60" \
  -c:v libx264 -g 150 -keyint_min 150 -sc_threshold 0 -pix_fmt yuv420p \
  -c:a aac -shortest src.mp4
ffmpeg -y -i src.mp4 -i chapters.ffmetadata \
  -map_metadata 0 -map_chapters 1 -c copy out.mp4
ffprobe -v error -show_chapters -of json out.mp4

# §4.2 — the stream-copy cut is wrong; count the frames, do not trust the duration
printf 'ffconcat version 1.0\nfile src.mp4\noutpoint 22.500000\nfile src.mp4\ninpoint 33.700000\n' > cut.concat
ffmpeg -y -f concat -safe 0 -i cut.concat -c copy cut.mp4
ffprobe -v error -select_streams v -count_frames \
  -show_entries stream=nb_read_frames -of csv=p=0 cut.mp4   # 1577, expected 1464

# §4.3 — re-encode throughput
ffmpeg -y -i src1080.mp4 -c:v libx264 -preset medium -force_key_frames 10.0,20.0 out.mp4
```
