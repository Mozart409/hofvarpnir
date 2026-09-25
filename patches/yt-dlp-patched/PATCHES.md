# Local patches to `yt-dlp`

This directory is a vendored copy of the upstream [`yt-dlp`](https://crates.io/crates/yt-dlp)
crate, wired into the workspace via `[patch.crates-io]` in the root `Cargo.toml`.

**Currently vendored upstream version: 2.7.2.**

Keep this file up to date whenever the vendored crate is changed. It is the only
record of how this copy diverges from upstream.

## Do not bump `yt-dlp` in the root `Cargo.toml`

`[patch.crates-io]` only applies when the vendored version satisfies the
dependency requirement **and** is the version the resolver picks. The root
manifest pins `yt-dlp = "=2.7.2"` -- an *exact* requirement, because the
earlier caret form `"2.7"` still admitted `2.8.x`, so a routine `cargo update`
resolved to crates.io's `2.8.3` and dropped the patch without touching the
manifest at all. Raising or loosening it makes Cargo **silently ignore the
patch** and build against crates.io instead:

```
warning: patch `yt-dlp v2.7.2 (patches/yt-dlp-patched)` was not used in the crate graph
   Compiling yt-dlp v2.8.3
```

`Cargo.lock` records this as a `[[patch.unused]]` entry. This happened on
2026-09-18 with a bump to `"2.8"`.

What saves you is that `hof-core` then fails to compile, because the divergences
below are load-bearing. Do not "fix" those errors against upstream -- restore the
`"=2.7.2"` requirement and re-resolve:

```sh
cargo update -p yt-dlp   # re-points the lockfile at the vendored path
```

Confirm the fix with `grep '\[\[patch.unused\]\]' Cargo.lock` returning nothing.
Genuinely moving to a newer upstream is the re-sync procedure below, not a
version-requirement edit.

## Why we fork

Upstream models several `--flat-playlist` JSON fields as required `String`.
YouTube emits `null` for those fields on private, deleted and members-only videos,
which are retained as placeholder entries inside a playlist. Serde then fails the
**entire** playlist parse with:

```
invalid type: null, expected a string at line 1 column <N>
```

One dead video in a 1000-video playlist makes the whole source unindexable. This
took down five production sources (`PietSmiet Worms`, `Pietsmiet PUBG`,
`PietSmiet Trouble in Terrorist`, `PietSmiet Perfect Heist`, `PietSmiet GTA Online`).

The failure is worse than it looks because `hof-core` passes `--playlist-reverse`
for `list=` URLs (`crates/hof-core/src/ytdlp.rs`), which moves the dead entries —
usually the oldest, at the end — to index 0. The parse dies ~1.4 KB into a
300 KB–2 MB document.

## The intentional divergence

There are two kinds: the playlist field types that motivated the fork, and a set
of feature additions that `hof-core` now depends on. Everything else in the diff
is mechanical fallout that the compiler will point you at.

### 1. Playlist field types

Three field types are deliberately changed, all in
`src/model/types/playlist.rs`.

| Struct | Field | Upstream | Here | Notes |
| --- | --- | --- | --- | --- |
| `Playlist` | `title` | `String` | `Option<String>` | also gains `#[serde(default)]` so a *missing* key is tolerated, not just an explicit `null` |
| `PlaylistEntry` | `title` | `String` | `Option<String>` | |
| `PlaylistEntry` | `url` | `String` | `Option<String>` | predates the `title` work (commit `f473d66`) |

Deliberately **not** changed:

- `Playlist.id` and `PlaylistEntry.id` stay `String`. They have never been observed
  null, and an entry with no id is unusable to us anyway — we want that to be a
  hard error, not a silent skip.
- Upstream's `tests/`, `examples/` and `benches/` are kept so re-syncs stay
  diffable against upstream.

#### Consumer-side behaviour

The `Option` is deliberately *not* given a default inside this crate — the
placeholder policy lives in `crates/hof-core/src/ytdlp.rs`:

- a `None` entry title becomes `UNAVAILABLE_TITLE` (`"Unavailable"`); the entry is
  **retained**, not skipped, so playlist counts stay honest
- a `None` playlist title falls back to `playlist.id`
- a `None` entry `url` is still skipped — such an entry is not downloadable

Regression coverage: `crates/hof-core/tests/fixtures/flat_playlist_null_titles.json`
is a trimmed real capture. Its entry objects deliberately keep `title` as the
**first** key, because that ordering is what makes serde hit the null before it
ever reads `id`. Do not reorder those keys.

### 1b. Video fields the `generic` extractor omits

Three fields on `Video` in `src/model/video.rs` gain `#[serde(default)]`. The
types are unchanged; only their required-ness is.

| Field | Type |
| --- | --- |
| `age_limit` | `i64` |
| `live_status` | `String` |
| `playable_in_embed` | `bool` |

Upstream models all three as required. yt-dlp's `generic` extractor emits none
of them, so metadata for a URL handled by that extractor failed to parse at
all:

```
Failed to fetch video metadata: JSON error while JSON parsing: missing field `age_limit`
```

**Scope, measured rather than assumed.** The YouTube extractor emits all three,
so YouTube — the platform this deployment actually indexes — was never
affected by this. The only extractor confirmed to omit them is `generic`.
Whether any other platform's extractor does was *not* established: the other
extractors reachable for a spot check (Vimeo, Dailymotion, SoundCloud, Rumble)
could not be queried from the machine where this was investigated.

So treat this as defensive hardening in the spirit of the playlist divergence
above — a missing optional field must degrade, not abort the parse — and not
as a fix for a known user-facing break. It cannot regress a payload that
already carries the fields.

Guarded by `crates/hof-api/tests/e2e/test_download_pipeline.rs`, which drives a
`generic`-extractor URL and would regress to this exact error if the attributes
were dropped on a re-sync.

### 2. Feature additions

These are additions, not type changes, and none exist upstream as of 2.8.3. They
must be re-applied on every re-sync or `hof-core` will not compile.

| Addition | Lives in | Why |
| --- | --- | --- |
| `DownloadDetails`, `DownloadBuilder::execute_detailed` | `src/client/download_builder.rs`, `src/client/mod.rs` | Reports the **delivered** height/codec/fps, not the requested quality. Feeds `DownloadResult` -> `db::DeliveredVideo` so `videos.video_height` / `videos.video_codec` record what the platform actually served. See "Delivered quality is recorded, not assumed" in the root `AGENTS.md`. |
| `VideoCodecPreference::Ranked(..)` | `src/model/selector.rs`, `src/client/streams/selection.rs` | Ordered codec preference where **resolution outranks codec**. Upstream offers only an absolute preference, which silently caps a 1440p profile at 1080p because YouTube publishes no AVC above 1080p. |
| `-movflags +faststart` on MP4-family muxes | `src/client/streams/pipeline/combine.rs` | Moves the `moov` atom to the front so a truncated file is playable up to the damage rather than unopenable (`moov atom not found`). Paired with `crates/hof-core/src/verify.rs`; see "Downloads are verified before they are published" in `AGENTS.md`. |
| `preferred_language_audio` (original-language audio) | `src/client/streams/selection.rs` | Audio selection (`select_audio_format`, `best_audio_format`, `worst_audio_format`) keeps only the tracks with the highest `language_preference` before ranking by codec/bitrate, so YouTube's auto-dubbed (AI-translated) tracks are never picked over the original. Upstream ignores language and can land on a dub, including on bitrate ties. Behavioural only: a re-sync that drops it still **compiles** -- `tests/unit/selection.rs` (`original_language_*`) is the guard. |

The fork also carries a nested `crates/media-seek` member, which Cargo resolves
through the same patch entry.

## Mechanical fallout, and the conventions to follow

Making a field `Option` breaks call sites across the crate. Follow the existing
conventions so re-syncs stay consistent:

- **tracing / log fields** — `entry.title.as_deref().unwrap_or("unknown")`.
  Note `Option<String>` has no `Display`, so `title = %playlist.title` must
  become `?playlist.title` or an `as_deref().unwrap_or(...)`.
- **filename templates** (`.replace("%(title)s", ...)`) — mirror the neighbouring
  missing-`url` handling in the same function: skip-with-warn in the loop-based
  download paths, early-`Err` in `spawn_playlist_download_task`.
- **`search_entries_by_title`** — an entry with no title simply never matches:
  `entry.title.as_deref().is_some_and(|t| ...)`.
- **`CachedPlaylist.title`** is a plain `String`, so `cache/stores/playlist.rs`
  uses `.clone().unwrap_or_default()`.
- **struct literals in `tests/`** need `Some(...)`.

## Re-sync procedure

The compiler finds the fallout for you, so re-syncing is: take upstream clean,
re-apply the three type changes, then fix what breaks.

1. Download and extract the new upstream release:
   ```sh
   curl -sL -o up.crate https://static.crates.io/crates/yt-dlp/yt-dlp-<VERSION>.crate
   tar xzf up.crate
   ```
2. Diff it against this directory first, to see what upstream changed and whether
   any of it collides with the table above:
   ```sh
   diff -ru yt-dlp-<VERSION> patches/yt-dlp-patched \
     --exclude=target --exclude=Cargo.lock --exclude=.cargo_vcs_info.json
   ```
3. **Check whether the fork is still needed.** Even if upstream makes the
   playlist fields optional, the feature additions in section 2 still have to
   live somewhere -- dropping the vendored crate means upstreaming or
   reimplementing those too, not just deleting the directory.

   Verified against upstream **2.8.3** (2026-09-18): still needed.
   `PlaylistEntry.title`, `PlaylistEntry.url` and `Playlist.title` remain
   `String`, and none of the section 2 additions exist upstream.
4. Replace this directory's contents with upstream, preserving this file, then
   re-apply **both** the type changes (section 1) and the feature additions
   (section 2).
5. Iterate until clean:
   ```sh
   cd patches/yt-dlp-patched && cargo check --all-targets --all-features
   ```
   Do not gate on `-D warnings` — upstream ships pre-existing dead-code warnings
   in its test helpers.
6. Verify the consumer side and the regression tests:
   ```sh
   cargo test --workspace
   cargo clippy --all-targets --all-features -- -D warnings
   ```
7. Update the "Currently vendored upstream version" line at the top of this
   file, set this crate's own `version` in `patches/yt-dlp-patched/Cargo.toml`,
   and update the `yt-dlp = "..."` requirement in the root `Cargo.toml` to match.
   All three must agree or the patch goes unused -- see the warning at the top.
8. Confirm the patch is actually in the graph:
   ```sh
   grep '\[\[patch.unused\]\]' Cargo.lock   # must print nothing
   ```

## How this is guarded in CI

This crate declares its own `[workspace]` table and is not a member of the root
workspace (`members = ["crates/*"]`). The root build reaches it only as a
`[patch.crates-io]` path dependency, and Cargo builds dependencies as **lib
targets only**. So `cargo clippy --all-targets`, `cargo test --all-features` and
`cargo fmt --all` at the repo root never compile this crate's tests, examples or
benches — they were broken for the entire life of the `url` patch without CI
noticing.

Two guards exist specifically for that blind spot, both scoped so they cost
nothing unless `patches/` changes:

- `.github/workflows/patches.yml` — path-filtered to `patches/**`
- the `patches-check` job in `lefthook.yml` `pre-push`

Both run `cargo check --all-targets --all-features` **inside this nested
workspace**, which is the only way to reach these targets. Running that command
from the repo root does not work: it fails with unresolved-import errors for this
crate's dev-dependencies, which the outer lockfile never resolved.

## Upstreaming

The right long-term fix is upstream: these fields are genuinely optional in
yt-dlp's own output. An accepted upstream PR making them `Option<String>` would
let us delete this entire directory. Not yet filed.
