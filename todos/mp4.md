# Profile-Level Output Preset Plan

## Issue Goal

Reduce Jellyfin/server remux overhead by preferring direct-play friendly outputs (H.264 + AAC in MP4), while still handling cases where YouTube only offers VP9/AV1 or non-AAC audio.

## Scope Decision

- No global `.env` knob.
- Add **runtime configuration on profiles** via an `OutputPreset` enum so each profile can target a specific playback environment.
- Default new and existing profiles to `Browser` (the direct-play fix).

## Design

### 1) `OutputPreset` enum (new, on profile)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "output_preset", rename_all = "lowercase")]
pub enum OutputPreset {
    Auto,       // yt-dlp picks best codecs, MKV container (current behavior)
    Browser,    // H.264 + AAC, MP4 container — Jellyfin/browser direct-play
    Tv,         // H.265/HEVC + AAC, MP4 container — smart TV direct-play
}
```

Default: `Browser` for all new profiles. Migration sets existing profiles to `Browser`.

### 2) `FormatPolicy` struct (internal, not persisted)

Resolved from `(Quality, OutputPreset)` — single codepath, replaces `quality_to_yt_quality()`.

```rust
pub struct FormatPolicy {
    pub video_quality: VideoQuality,          // from yt-dlp crate
    pub audio_quality: AudioQuality,          // always Best
    pub video_codec: VideoCodecPreference,    // from yt-dlp crate
    pub audio_codec: AudioCodecPreference,    // from yt-dlp crate
    pub container_ext: &'static str,          // "mkv" or "mp4"
}
```

Mapping:

| OutputPreset | VideoCodecPreference | AudioCodecPreference | container_ext |
|-------------|---------------------|---------------------|--------------|
| Auto        | Any                 | Any                 | mkv          |
| Browser     | AVC1                | AAC                 | mp4          |
| Tv          | Custom("hevc")      | AAC                 | mp4          |

When `Quality::AudioOnly`: skip video codec preference entirely, let yt-dlp pick best audio, and do not force container extension in template rendering. The final extension comes from the actual result path returned by yt-dlp.

Graceful fallback requirement (explicit):

- For `Browser`/`Tv`, format selection must be best-effort and not fail immediately when preferred codecs are unavailable at requested quality.
- Fallback order:
  1. Preferred codec pair at requested quality (e.g. AVC+AAC for `Browser`).
  2. Preferred video codec + best available audio at requested quality.
  3. Best available muxable pair at requested quality.
  4. If none available at requested quality, relax quality constraint one level at a time until a muxable pair is found.
- Only return a hard error when no downloadable muxable format exists after fallback exhaustion.

`Quality` maps to `VideoQuality` as before (Best → Best, Q1080p → CustomHeight(1080), etc).

### 3) Output template extension refactor

- `render_output_relative_path()` gains a `container_ext: &str` parameter.
- All hardcoded `"mkv"` replaced with `container_ext`.
- Extension enforcement logic becomes generic (check/append `container_ext` instead of `.mkv`).
- For `Quality::AudioOnly`, extension enforcement is disabled (do not append/force any extension).
- Comment on line 523 updated.
- Tests updated to be parameterized over extension and audio-only no-force behavior.

### 4) yt-dlp execution path changes

- `DownloadRequest` carries `FormatPolicy` instead of `&Quality`.
- In `YtdlpClient::download_video`:
  - Pass `FormatPolicy` fields to `DownloadBuilder`: `.video_quality()`, `.audio_quality()`, `.video_codec()`, `.audio_codec()`.
  - Output path uses `container_ext` from policy (via updated `render_output_relative_path`).
- Delete `quality_to_yt_quality()` — fully replaced by `FormatPolicy::from(quality, preset)`.
- No raw format selector strings needed — the yt-dlp crate's native API handles codec selection and fallback internally.

### 5) Data flow wiring

- Extend domain + DB structs:
  - `Profile`, `ProfileRow`: add `output_preset: OutputPreset`
  - `CreateProfile`, `UpdateProfile`: add field
- Add SQL migration:
  - Create `output_preset` PostgreSQL ENUM type with `auto`, `browser`, `tv` values.
  - Add `output_preset` column to `profiles` with `DEFAULT 'browser'` and `NOT NULL`.
  - Set all existing rows to `browser`.
  - Down migration: drop column, drop type.
- Update CRUD SQL (`INSERT`, `SELECT`, `UPDATE`, response mapping).

### 6) API + web UI

- **API (`hof-api`)**: add `output_preset` to `CreateProfileRequest`, `UpdateProfileRequest`, `ProfileResponse`.
- **Web UI (`hof-web`)**: add dropdown to profile form (similar to existing quality dropdown).
  - `OutputPresetForm` enum for form deserialization.
  - Options labeled: "Auto (best quality)", "Browser (Jellyfin/web direct-play)", "TV (smart TV direct-play)".

### 7) Error contract and codes

- Add structured error code(s) for format-selection/download failures surfaced by API and activity log.
- Proposed codes:
  - `DOWNLOAD_FORMAT_UNAVAILABLE`: preferred preset/quality could not be satisfied directly, fallback exhausted.
  - `DOWNLOAD_FORMAT_INVALID_PRESET`: preset maps to invalid selector/config (defensive, should be rare).
  - `DOWNLOAD_EXECUTION_FAILED`: yt-dlp execution failed after selection succeeded.
- API error responses should include both a user-readable message and machine-readable code.
- Worker/activity logging should include code + preset + quality + selected fallback stage for debugging.

## Implementation Steps

1. Add `OutputPreset` enum to `hof-core/src/domain/profile.rs`.
2. Add migration for `output_preset` ENUM type and column.
3. Update `Profile`, `ProfileRow`, `CreateProfile`, `UpdateProfile` structs and CRUD SQL.
4. Add `FormatPolicy` struct and `build_format_policy(quality, preset)` function in `hof-core/src/ytdlp.rs`.
5. Refactor `render_output_relative_path` to accept `container_ext` parameter; update all hardcoded `.mkv`.
6. Update `DownloadRequest` to carry `FormatPolicy`; wire into `download_video` builder calls.
7. Delete `quality_to_yt_quality()`.
8. Update API types and handlers in `hof-api/src/routes/profiles.rs`.
9. Update web form types and template in `hof-web/src/pages.rs`.
10. Update all callers of `DownloadRequest` and `render_output_relative_path`.
11. Implement and propagate structured error codes for format policy + download failures.
12. Update tests (profile CRUD, template rendering, format policy mapping, error code mapping).
13. Run: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -W clippy::pedantic`, targeted tests, then workspace tests.

## Implementation Phases (Checklist)

### Phase 0 - Planning and Scope

- [x] Confirm runtime profile config (not `.env`) as source of truth.
- [x] Define `OutputPreset`-based approach (`Auto`, `Browser`, `Tv`).
- [x] Define graceful fallback expectations and audio-only extension behavior.
- [x] Define initial machine-readable error code contract.

### Phase 1 - Persistence and Domain Wiring

- [x] Add `output_preset` PostgreSQL enum + `profiles.output_preset` column migration.
- [x] Backfill existing rows to `browser` and enforce `NOT NULL` + default.
- [x] Update `Profile`, `ProfileRow`, `CreateProfile`, `UpdateProfile` in `hof-core`.
- [x] Update profile CRUD SQL and mapping for read/write support.

### Phase 2 - Core Download Policy Refactor

- [x] Add `FormatPolicy` and `build_format_policy(quality, preset)` in `crates/hof-core/src/ytdlp.rs`.
- [x] Update `DownloadRequest` to carry `FormatPolicy`.
- [x] Replace `quality_to_yt_quality()` usage and remove it.
- [x] Implement deterministic fallback stages for `Browser`/`Tv`.
- [x] Add structured internal context for fallback stage selection (for logs/errors).

### Phase 3 - Output Path and Extension Semantics

- [x] Refactor `render_output_relative_path()` to accept `container_ext`.
- [x] Replace hardcoded `.mkv` usage with policy-driven extension.
- [x] Disable extension forcing for `Quality::AudioOnly`.
- [x] Ensure fallback filename generation honors non-audio and audio-only rules.

### Phase 4 - API and Web UI Exposure

- [x] Add `output_preset` to API request/response DTOs in `crates/hof-api/src/routes/profiles.rs`.
- [x] Validate and persist preset through create/update handlers.
- [x] Add preset field to web form model (`OutputPresetForm`) in `crates/hof-web/src/pages.rs`.
- [x] Add dropdowns in create/edit profile UI with agreed labels.

### Phase 5 - Error Contract and Observability

- [x] Introduce and wire error codes: `DOWNLOAD_FORMAT_UNAVAILABLE`, `DOWNLOAD_FORMAT_INVALID_PRESET`, `DOWNLOAD_EXECUTION_FAILED`.
- [x] Include machine-readable code + human message in API error responses.
- [x] Include code + preset + quality + fallback stage in worker/activity logs.
- [x] Ensure fallback exhaustion maps to stable, testable error output.

### Phase 6 - Verification and Hardening

- [x] Add/adjust unit tests for `FormatPolicy` mapping and fallback behavior.
- [x] Add/adjust unit tests for template rendering across `mp4`/`mkv` + audio-only no-force.
- [x] Add integration/API tests for profile preset round-trip and defaults.
- [ ] Add integration test for preferred-codec-unavailable -> fallback succeeds.
- [ ] Add integration test for fallback exhaustion -> expected machine-readable error code.
- [x] Run `cargo fmt --all`.
- [x] Run `cargo clippy --workspace --all-targets -- -W clippy::pedantic`.
- [x] Run targeted tests and then workspace tests.

## Test Plan

- Unit tests:
  - `build_format_policy` for each `(Quality, OutputPreset)` combination
  - `render_output_relative_path` with `"mp4"` and `"mkv"` extensions
  - fallback filename with correct extension
- Integration/API tests:
  - create/update/get profile round-trip with each preset value
  - default value behavior for new profiles
- Behavioral:
  - confirm output path uses `.mp4` extension when preset is `Browser` or `Tv`
  - confirm audio-only downloads do not get forced extension appended by template renderer
  - integration test: trigger a case where preferred codec is unavailable and assert fallback succeeds (no hard failure)
  - integration test: trigger fallback exhaustion and assert API/worker returns expected machine-readable error code

## Risks / Notes

- Fallback logic must be deterministic and logged, otherwise diagnosing codec-selection behavior will be difficult.
- For `AudioOnly` + any preset, there's no video stream to constrain. Audio codec preference still applies (AAC for Browser/Tv), but container/extension is determined by yt-dlp output, not forced by us.
- `Tv` uses `Custom("hevc")` since the yt-dlp crate's `VideoCodecPreference` doesn't have a native HEVC variant. The `matches_video_codec` function does substring matching, so `"hevc"` will match codec strings containing "hevc" or "h265" — need to verify this covers YouTube's codec identifiers (they typically report as `"vp09..."` or `"av01..."` or `"avc1..."` — HEVC/H.265 is rare on YouTube but common on other platforms).
