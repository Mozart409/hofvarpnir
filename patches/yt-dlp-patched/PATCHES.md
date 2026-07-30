# Local patches to `yt-dlp`

This directory is a vendored copy of the upstream [`yt-dlp`](https://crates.io/crates/yt-dlp)
crate, wired into the workspace via `[patch.crates-io]` in the root `Cargo.toml`.

**Currently vendored upstream version: 2.7.2.**

Keep this file up to date whenever the vendored crate is changed. It is the only
record of how this copy diverges from upstream.

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

Only **three field types** are deliberately changed, all in
`src/model/types/playlist.rs`. Everything else in the diff is mechanical
fallout that the compiler will point you at.

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

### Consumer-side behaviour

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
3. **Check whether the fork is still needed.** If upstream has made these fields
   optional, drop the vendored crate entirely and delete the `[patch.crates-io]`
   entry from the root `Cargo.toml`. As of upstream **2.8.0** it is still needed:
   `PlaylistEntry.url` and both `title` fields are still `String` there.
4. Replace this directory's contents with upstream, preserving this file, then
   re-apply the three type changes from the table above.
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
7. Update the "Currently vendored upstream version" line at the top of this file.

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
