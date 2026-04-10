# Profile-Level yt-dlp Format Override Plan

## Issue Goal

Reduce Jellyfin/server remux overhead by preferring direct-play friendly outputs (H.264 + AAC in MP4), while still handling cases where YouTube only offers VP9/AV1 or non-AAC audio.

## Scope Decision (updated)

- No global `.env` knob.
- Add **runtime configuration on profiles** so each profile can opt into/override yt-dlp format behavior.

## Proposed Design

### 1) Profile model: add per-profile format override

- Add nullable profile field, e.g. `ytdlp_format_override: Option<String>`.
- Semantics:
  - `None` => use application default format policy (MP4-friendly default described below).
  - `Some(value)` => pass through as explicit yt-dlp `-f` selector for that profile.
- Keep this field orthogonal to existing `quality` (quality still constrains target resolution behavior).

### 2) Default format policy (no override provided)

- Introduce a default format selector that prefers browser/Jellyfin direct-play compatibility:
  - Prefer AVC/H.264 video + MP4A/AAC audio.
  - Prefer mux result in MP4 (`--merge-output-format mp4`).
- Add graceful fallback branches in the selector so downloads do not fail when exact AVC+AAC is unavailable.
- For quality presets (`2160p`, `1080p`, etc.), include quality-aware constraints in generated selector (idiomatic Rust builder/function rather than stringly scattered logic).

### 3) Output extension handling (important)

- Current template rendering hardcodes `.mkv` in `render_output_relative_path` and `{ext}` replacement.
- Update template rendering to use a **resolved target container extension** from download settings (default `mp4`, potentially other value if profile override implies different behavior).
- Ensure fallback filename and extension normalization logic no longer forces MKV.

### 4) Data flow wiring

- Extend domain + DB structs:
  - `Profile`, `ProfileRow`
  - `CreateProfile`, `UpdateProfile`
- Add SQL migration for new nullable `profiles.ytdlp_format_override` column.
- Update CRUD SQL (`INSERT`, `SELECT`, `UPDATE`, response mapping).

### 5) API + web runtime config exposure

- API (`hof-api`): include new field in create/update/request/response types.
- Web UI (`hof-web`): add profile form input for optional format override.
- Validation:
  - Accept empty as `None`.
  - Basic guardrails (non-empty trim, max length, reject obvious invalid control chars).
  - Keep validation pragmatic; let yt-dlp remain final authority for complex selector correctness.

### 6) yt-dlp execution path changes

- Update `DownloadRequest` to carry resolved format/container config derived from profile.
- In `YtdlpClient::download_video`, apply:
  - format selector (`-f ...`) from override or default builder.
  - merge output format (`mp4`) for default policy.
- Preserve existing progress/error behavior.

## Acceptance Criteria Mapping

- Runtime profile config override exists: `ytdlp_format_override` on profile.
- Default behavior prefers MP4 container with H.264 + AAC when available.
- Fallback path remains graceful when preferred codecs are not available at requested quality.

## Implementation Steps

1. Add migration and profile model/DB plumbing for `ytdlp_format_override`.
2. Add API + web form support for create/edit/read.
3. Add format policy module/helpers in `hof-core` (default selector + quality constraints + container ext).
4. Wire resolved policy into `DownloadRequest` and yt-dlp invocation.
5. Refactor output template extension handling to be container-driven, not MKV-hardcoded.
6. Update tests (profile CRUD, template rendering, quality/format mapping, any API snapshots).
7. Run: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -W clippy::pedantic`, targeted tests, then workspace tests.

## Test Plan

- Unit tests:
  - default selector generation by `Quality` (including fallback chain)
  - override pass-through behavior
  - output template `{ext}` and fallback filename with `mp4`
- Integration/API tests:
  - create/update/get profile round-trip with and without override
  - validation behavior for empty/invalid override values
- Behavioral smoke test (if feasible in existing harness):
  - confirm produced path/container defaults to mp4 in normal flow

## Risks / Notes

- The current `yt_dlp` Rust crate API may expose format controls differently than raw CLI args; if direct flag parity is limited, we should centralize a minimal adapter layer instead of scattering workarounds.
- For `AudioOnly`, we should keep behavior explicit (likely not forcing MP4 video container); this needs a small policy branch but does not block core issue.
