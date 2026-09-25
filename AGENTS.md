# Agent Instructions for Hofvarpnir

This document provides essential information for AI coding agents working in this repository.

## Build, Test, and Lint Commands

### Essential Commands

```bash
# Build the entire workspace
cargo build --workspace

# Build with all features
cargo build --all-features --release

# Run all tests
cargo test --workspace
cargo test --all-features

# Run a specific test (use this pattern)
cargo test --workspace <test_name>
cargo test --workspace -- <filter_pattern>

# Run tests for a specific package
cargo test -p hof-core
cargo test -p hof-api

# Format code
cargo fmt --all

# Check formatting without modifying
cargo fmt --all -- --check

# Run clippy. Lint levels live in Cargo.toml [workspace.lints.clippy] -- do NOT
# add `-D clippy::pedantic -D clippy::nursery` here. Those flags are applied
# after the manifest's lint levels and re-deny the whole group, silently
# defeating the nine selective `allow` entries (option_if_let_else,
# needless_pass_by_ref_mut, module_name_repetitions, ...). Cargo.toml is the
# single source of truth. Test-only panic helpers are allowed via clippy.toml.
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run strict clippy continuously
bacon pedantic
# Or manually:
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Fix auto-fixable clippy issues
cargo clippy --fix --allow-dirty --allow-staged --all-targets --all-features

# Check SQLx offline mode
cargo sqlx prepare --workspace --check -- --all-targets --all-features

# Lint/fix SQL (sqruff -- dialect=postgres, rules=all, see .sqruff).
# `just lint` does NOT run this -- that recipe is cargo-clippy only, so SQL is
# unchecked until commit/push time unless run explicitly. Hook-enforced:
# pre-commit runs `sqruff fix` on staged SQL, pre-push runs `sqruff lint` on
# pushed SQL. `rules = all` forbids hand-aligned DDL columns (LT01); expect
# alignment to be collapsed.
sqruff lint crates/hof-core/migrations/*.sql
sqruff fix crates/hof-core/migrations/*.sql

# Dependency audit
cargo deny check
```

### Development Tools

```bash
# Use bacon for continuous checking/testing (recommended)
bacon                    # Default: check
bacon test               # Run all tests continuously
bacon test -- <filter>   # Run specific test continuously
bacon clippy-all         # Run clippy on all targets
bacon pedantic           # Run strict pedantic and nursery clippy
bacon serve              # Run web server with auto-restart
bacon tui                # Run TUI binary

# Use just for task automation
just --list              # List all available tasks
just fmt                 # Format code
just lint                # Run clippy
just fix                 # Fix clippy issues
just test                # Run tests with DB setup (includes test-patches)
just test-patches        # Offline tests of patches/yt-dlp-patched (own workspace)
just dev                 # Run web server (config decrypted from .sops.env)
just db-reset            # Reset database
just mig-run             # Run migrations
just prepare             # Generate SQLx offline data
```

### Nix Environment

This project uses Nix for reproducible development environments:

```bash
# Enter development shell
nix develop

# Run commands in nix environment (used in CI)
nix develop .#default --command cargo test --all-features
```

## Testing Philosophy

- **NEVER write unit tests after you write code.** Unit tests written after the
  fact tend to just re-describe the implementation rather than verify
  behavior.
- **Highly prefer E2E tests as the sole testing mechanism.** Use them to
  verify complex features work end-to-end. At the end of an E2E test, produce
  a verifiable and repeatable artifact (e.g. a downloaded/verified file, a
  persisted DB row, an API response fixture) rather than just asserting a
  process exited cleanly.
- **If you must test a system in isolation**, first write down all the ways
  it could fail, *then* write the code to guard against those failure modes.
  Do not write the code first and backfill unit tests against it.
- **When writing an E2E test, don't pick the simplest possible scenario to
  prove the happy path works.** Pick a medium-to-hard scenario when verifying
  the work.

## Code Style Guidelines

### Imports

```rust
// Group imports in this order, separated by blank lines:
// 1. std/core/alloc
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

// 2. Third-party external crates
use chrono::{DateTime, Utc};
use kameo::prelude::*;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tracing::{debug, error, info, instrument};
use ulid::Ulid;

// 3. Internal crate imports (for non-lib.rs files)
use crate::db;
use crate::domain::profile::Quality;
use crate::domain::video::{DownloadProgress, Video};
use crate::ytdlp::{DownloadRequest, YtdlpClient};

// 4. Re-exports (only in lib.rs)
pub use config::Config;
pub use startup::{ActorSystem, initialize, shutdown};
```

### Formatting

- Use default rustfmt configuration (no custom rustfmt.toml)
- Run `cargo fmt --all` before committing
- Line length: follow Rust standard (100 chars soft limit)
- Trailing commas in multi-line structures

### Types and Naming

```rust
// Types
pub struct Video { ... }                    // PascalCase for structs
pub enum VideoStatus { ... }                // PascalCase for enums
type VideoId = Ulid;                        // PascalCase for type aliases

// Constants
const INCOMPLETE_DIR_NAME: &str = "incomplete";  // SCREAMING_SNAKE_CASE

// Functions and variables
fn download_video(video: &Video) -> Result<PathBuf> {  // snake_case
    let output_path = PathBuf::new();  // snake_case
}

// Error types (use thiserror)
#[derive(Debug, thiserror::Error)]
pub enum YtdlpError {
    #[error("Failed to initialize: {0}")]
    InitializationError(String),
    #[error("Video unavailable: {0}")]
    VideoUnavailable(String),
}

// Traits and implementations
impl Actor for DownloadWorker { ... }       // PascalCase trait names
```

### Error Handling

```rust
// Use thiserror for error types
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("Not found: {0}")]
    NotFound(Ulid),
    #[error("Validation failed: {0}")]
    Validation(String),
}

// Use color-eyre for application-level error handling
use color_eyre::Result;
use color_eyre::eyre::eyre;

// Prefer ? operator
let video = db::get_video(&pool, id).await?;

// Log errors with tracing
error!(error = %e, video_id = %id, "Failed to download video");

// Add context for errors
return Err(YtdlpError::InitializationError(msg.to_string()));
```

#### Banned: `unwrap()` and `expect()`

**Never use `.unwrap()` or `.expect()` in production code.** These methods panic on failure, which is unrecoverable and inappropriate for application code.

```rust
// BAD: Will panic on None/Err
let value = some_option.unwrap();
let result = fallible_call().expect("should work");

// GOOD: Use ? operator with color_eyre
let value = some_option.ok_or_else(|| eyre!("missing value"))?;
let result = fallible_call()?;

// GOOD: Use if-let or match for optional handling
if let Some(value) = some_option {
    // handle value
}

// GOOD: Provide default values when appropriate
let value = some_option.unwrap_or_default();
let value = some_option.unwrap_or(fallback);
```

**Exceptions:** `.unwrap()` is acceptable only in:

- Tests (where panics are expected failure modes)
- Cases where the invariant is statically provable (document with a comment)

### Documentation

```rust
//! Crate-level documentation (in lib.rs/main.rs)

//! Module-level documentation

/// Short description (ends with period)
///
/// Longer explanation if needed.
///
/// # Arguments
///
/// * `param` - Description
///
/// # Errors
///
/// Returns error when...
///
/// # Panics
///
/// Panics if...
pub fn function_name(param: Type) -> Result<ReturnType> { ... }

// Field-level comments (no doc comments needed for obvious fields)
pub struct Video {
    /// yt-dlp extractor name (e.g., "youtube", "vimeo")
    pub platform: String,
    pub title: String,  // No comment needed for obvious fields
}
```

### Database and SQLx

- Use SQLx with compile-time checked queries
- Migrations live in `crates/hof-core/migrations/`
- Use `sqlx::FromRow` for database row types
- Use `TryFrom<RowType> for DomainType` for conversion
- Run `just prepare` after schema changes for offline mode

#### Adding a migration: order matters

1. Write the SQL.
2. `sqruff fix` it.
3. `just prepare` (runs `mig-run`, then regenerates `.sqlx/`).
4. `just lint`.
5. `just test`.

`just prepare` must precede lint and test: every test recipe sets
`SQLX_OFFLINE=true`, and the postgres-test instance carries no schema, so a new
`query!` with no `.sqlx/` cache entry fails at **compile** time, not test time.

**Run `sqruff fix` on new SQL before applying the migration, never after.**
Applying a migration records its checksum in `_sqlx_migrations`; reformatting
the file afterwards strands that checksum, and every later commit fails the
pre-commit `sqlx-prepare` hook with `migration <version> was previously
applied but has been modified`. Fix by re-applying, not by hand-editing the
checksum:

```bash
just mig-revert && just mig-run
```

Full recovery steps and a second, unrelated failure mode (`XX002` index
corruption in the postgres-test instance) are in
[`docs/sqlx-troubleshooting.md`](docs/sqlx-troubleshooting.md).

#### `TIMESTAMPTZ` maps to `time::OffsetDateTime`, not `chrono::DateTime<Utc>`

`Cargo.toml` enables sqlx's `chrono` feature, but the vendored
`tower-sessions-sqlx-store` crate unifies sqlx's `time` feature on for the
whole workspace. With both enabled, the `query!`/`query_as!` macros prefer
`time`: a plain `SELECT` of a timestamptz column yields `OffsetDateTime` and
fails E0308 against a `DateTime<Utc>` field, plus a deprecation warning that
`-D warnings` turns into a build failure.

**Fix (the established pattern in this codebase):** an inline type override in
the query — `created_at AS "created_at: DateTime<Utc>"`, or the `?` form for a
nullable column, `paused_until AS "paused_until?: DateTime<Utc>"`. See
`crates/hof-core/src/db/source.rs` lines 69, 109, 148, 187, 232, 286, 548 for
existing uses.

Rejected alternative: sqlx supports a global `sqlx.toml` with
`[macros.preferred-crates] date-time = "chrono"`, but reading it is gated
behind the `sqlx-toml` cargo feature, which is not in sqlx's default features
and not declared in this workspace — a bare `sqlx.toml` would be silently
ignored. Enabling it would also mean adding `"sqlx-toml"` to the sqlx
features. Per-column overrides are cheaper and are what the codebase already
does.

### Actor Pattern (Kameo)

```rust
// Actor definition
pub struct DownloadWorker { ... }

impl Actor for DownloadWorker {
    type Args = DownloadWorkerArgs;
    ...
}

// Message handlers
#[derive(Reply)]
pub enum DownloadOutcome { ... }

impl Message<DownloadVideo> for DownloadWorker {
    type Reply = DownloadOutcome;
    async fn handle(...) -> Self::Reply { ... }
}
```

#### Anti-pattern: Self-tell with `.await`

**DON'T:** Use `.tell(msg).await` when an actor sends a message to itself. This can deadlock with bounded mailboxes.

```rust
// BAD: Self-tell with await can cause deadlock
ctx.actor_ref().tell(SomeMessage).await?;
```

**DO:** Use `.try_send()` for self-messages. If the mailbox is full, the message will be dropped (handle the error appropriately).

```rust
// GOOD: Use try_send for self-messages
ctx.actor_ref().tell(SomeMessage).try_send()?;

// Or if you want to ignore the error:
ctx.actor_ref().tell(SomeMessage).try_send().ok();
```

This pattern is commonly needed when:

- Spawning periodic tasks within an actor that need to trigger the actor again
- Processing items in a loop and enqueueing more work to the same actor
- Implementing state machines where the actor transitions states by sending itself messages

### Web API (Axum + Utoipa)

```rust
// Route handler
#[utoipa::path(
    get,
    path = "/api/videos",
    responses(
        (status = 200, description = "List of videos", body = Vec<Video>),
    ),
)]
pub async fn list_videos(State(state): State<AppState>) -> Result<impl IntoResponse> { ... }
```

## Database

In development you can use this connection string to connect to the database. DATABASE_URL=postgresql://postgres:postgres@localhost:5432/hofvarpnir_dev

The `just` DB recipes (`up`, `mig-*`, `db-*`, `prepare`) use exactly that URL via the `database_url` variable — it is compose's own dev credential, not a secret. The server's real configuration (including `DATABASE_URL` for any other environment) lives encrypted in `.sops.env` and is injected only into `just dev` / `bacon serve` via `sops exec-env`; edit with `sops .sops.env`, never `cat`/`sops -d` it. There is no plaintext `.env` and `set dotenv-load` is gone on purpose.

The test entry points (`just test`, `e2e`, `e2e-only`, `ci`, and the bacon `test`/`nextest` jobs) do not use the dev database: they override `DATABASE_URL` to a dedicated, ephemeral Postgres (`postgres-test` service in `containers/compose.dev.yml`, localhost:5433, no monitoring extensions, durability disabled). Override with `TEST_DATABASE_URL` (just) if needed. The bacon `run`/`serve`/`tui` jobs still use the dev database, as does `just dev`.

`#[sqlx::test]` migrates a fresh database per test against postgres-test, so a
migration checksum mismatch (see Database and SQLx above) affects the dev
database and pre-commit hook, not `just test` — don't run migrations against
5433 to "fix" a test failure. Durability being disabled on postgres-test also
means its own sqlx-managed registry (`_sqlx_test.databases`) can develop index
corruption after an unclean shutdown, surfacing as Postgres error `XX002`
(`heap tid from index tuple ... points past end of heap page`) from inside
sqlx's test harness rather than from application code. Fix with
`REINDEX TABLE _sqlx_test.databases;` against localhost:5433 — see
[`docs/sqlx-troubleshooting.md`](docs/sqlx-troubleshooting.md) for details.

You can use flake.nix psql client.

### Dependency Policy

**Before running `cargo add`, always:**

1. Verify the crate is open-source and its license is acceptable.
2. Check `deny.toml` `[licenses]` allow list to confirm the license is permitted.
3. If the license is not listed, ask the user before proceeding — new open-source licenses can be added to `deny.toml`.

Current allowed licenses (see `deny.toml`): MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, Unicode-3.0, CDLA-Permissive-2.0, ISC, Zlib, BSD-3-Clause, GPL-3.0-only, GPL-3.0-or-later.

## Project Structure

```
crates/
├── hof-core/     # Domain types, actors, database, yt-dlp wrapper
├── hof-api/      # Axum REST API + OpenAPI + SSE
├── hof-web/      # Maud + htmx frontend + Tailwind CSS
└── hof-tui/      # Ratatui terminal UI client
```

## Key Dependencies

- **Async**: tokio (runtime), futures, tokio-stream
- **Web**: axum, tower, tower-http, utoipa (OpenAPI)
- **Database**: sqlx (PostgreSQL)
- **Actors**: kameo
- **Templating**: maud
- **Serialization**: serde, serde_json
- **IDs**: ulid
- **Time**: chrono
- **Errors**: thiserror, color-eyre
- **Tracing**: tracing, tracing-subscriber, tracing-opentelemetry, tracing-loki
- **Metrics**: metrics, metrics-exporter-prometheus
- **Video**: yt-dlp

## Recent Download Features (MP4/Direct-Play Work)

This repository now includes profile-level output preset behavior for download format selection.

- **Profile output preset** (`OutputPreset`):
  - `Auto` -> keep broad compatibility behavior (`mkv`, any codecs)
  - `Browser` -> direct-play preference (`mp4`, AVC/H.264 then AV1, + AAC)
  - `Tv` -> direct-play preference (`mp4`, HEVC then AVC/H.264 then AV1, + AAC)
- `output_preset` is persisted in PostgreSQL (`profiles.output_preset`) and exposed through API + web profile forms.

### Download policy model

- `FormatPolicy` (in `crates/hof-core/src/ytdlp.rs`) is resolved from `(Quality, OutputPreset)`.
- Download fallback is deterministic and staged (`FallbackStage`):
  1. preferred video+audio codec pair
  2. preferred video codec + any audio
  3. any muxable codec pair
  4. then quality is relaxed until exhausted
- On exhaustion, download returns a structured format-unavailable error.

#### Codec preference is ordered, not absolute

**Resolution outranks codec.** Video codec preferences are expressed as
`VideoCodecPreference::Ranked(..)`, and selection takes the first entry that can
actually reach the requested height. This matters because YouTube publishes no
AVC/H.264 above 1080p — a bare `AVC1` preference silently caps a 1440p profile at
1080p, reporting success the whole way.

Rules when touching this area:

- A codec that only exists *below* the target height is skipped, not honored.
- If no ranked codec reaches the target, the **codec guarantee wins** and the
  resolution drops. Presets name codecs because the playback device can decode
  them; returning an undecodable stream at the right resolution is worse.
- Only when no ranked codec matches anything at all does selection widen to all
  formats.
- **VP9 is deliberately excluded from the `Browser` and `Tv` ladders.** Both force
  an `mp4` container, and VP9 outside `webm` is poorly supported by browsers.

Selection lives in `patches/yt-dlp-patched/src/client/streams/selection.rs`
(`select_video_format`); the ladders are built in `FormatPolicy::from`.

### Delivered quality is recorded, not assumed

A profile's `quality` is a *request*. What the platform served is persisted
separately on `videos.video_height` / `videos.video_codec`, sourced from
`DownloadBuilder::execute_detailed` -> `DownloadResult` -> `db::DeliveredVideo`.

- When a download under-delivers against the profile's requested height, the
  worker logs a `warn!` — the download still succeeds, so this is the only signal.
- The web UI surfaces it via `delivered_quality_badge` in `crates/hof-web/src/pages.rs`.
- Do not infer delivered resolution from the profile's `quality`; they diverge.

### Output path/extension behavior

- Output template rendering is container-aware (`container_ext`) instead of hardcoded `.mkv`.
- For `Quality::AudioOnly`, extension forcing is disabled; final extension is determined by yt-dlp output.

### Error contract

Machine-readable error codes for download failures are implemented in `YtdlpError`:

- `DOWNLOAD_FORMAT_UNAVAILABLE`
- `DOWNLOAD_FORMAT_INVALID_PRESET`
- `DOWNLOAD_EXECUTION_FAILED`
- `DOWNLOAD_VERIFICATION_FAILED`

These codes are propagated into worker/supervisor logs and persisted failure text (`[CODE] ...`).
API download responses expose parsed `last_error_code` when available.

### Downloads are verified before they are published

A clean yt-dlp exit does not mean a playable file. `crates/hof-core/src/verify.rs`
gates every download in `DownloadWorker::handle_success` **while the file is
still in `incomplete/`**, so a damaged file never reaches `completed/` and never
gets marked completed in the database. A failure returns
`DownloadOutcome::Failed` with `DOWNLOAD_VERIFICATION_FAILED`, which picks up
the supervisor's existing `max_attempts` counting and backoff.

Three checks, cheapest first:

1. `ffprobe` -- container readable, expected streams present, duration not short
   of `videos.duration_secs` (2% tolerance, 5s floor).
2. A zero-run scan -- any run of `0x00` at or above 4 MiB.
3. `ffmpeg -i <file> -c copy -f null -` -- reads every byte, validates container
   framing, decodes nothing.

Rules when touching this area:

- **`ffmpeg` exits 0 even when it reports stream errors.** The demux pass is
  judged on whether stderr is non-empty at `-v error`, never on exit status.
  This was measured, not assumed.
- **Do not replace the demux pass with a sampled window such as `-sseof -30`.**
  With `+faststart` the index is at the front of the file, so `-sseof` seeks
  past EOF on a truncated file, decodes **zero frames**, and exits clean -- it
  reports success on exactly the files it is supposed to catch.
- **Do not drop the zero-run scan as redundant with the demux pass.** `-c copy`
  catches an interior hole only where the demuxer validates in-band framing:
  measured, H.264 is caught but **AV1 and HEVC are not**, and AV1 is what the
  `Browser`/`Tv` ladders deliver above 1080p. The scan is what covers them.
  Truncation, by contrast, is caught for every codec.
- The file is **deleted** when verification fails. The segmented downloader
  resumes onto an existing output file and decides a segment is complete by
  probing only its first and last bytes (`parallel.rs`,
  `is_segment_downloaded`), so leaving damage in place lets every retry resume
  onto it.
- `-movflags +faststart` is applied to MP4-family muxes in
  `patches/yt-dlp-patched/src/client/streams/pipeline/combine.rs`. It costs one
  extra pass over the output and makes a truncated file playable up to the
  damage instead of unopenable (`moov atom not found`).

Set `DOWNLOAD_VERIFY=false` to disable the gate without a code change.

### FFmpeg exits 0 when it declines to overwrite

Measured (ffmpeg 9.0.1): if the output path exists and `-y` is absent, FFmpeg
prints `Not overwriting - exiting`, **exits 0 in milliseconds, and writes
nothing**. This produced the production `moov atom not found` failures:

1. A multi-GB MP4 combine (`-c copy -movflags +faststart`) hit the executor's
   300s timeout and was killed before writing `moov`, leaving a partial file at
   the output path.
2. The timeout error advanced `execute_fallback_attempts` to the next stage,
   which re-downloaded the streams into the same output path.
3. That stage's combine ran without `-y`, exited 0 in ~20ms, and was reported
   as success, publishing the killed file to verification.

Guards, all required:

- Combine passes `-y`.
- Each fallback stage deletes any leftover output first.
- The executor (`executor/process.rs`) turns an exit-0 "not overwriting" into
  `CommandFailed`.
- Combine's timeout is `max(DEFAULT_TIMEOUT, COMBINE_TIMEOUT_FLOOR)` (30 min).
  The shared 300s default is sized for metadata calls, and the `+faststart`
  rewrite pushed 2-5 GB muxes past it.
- A timeout **stops** the fallback (`AttemptError::Abort`), returning
  `DOWNLOAD_EXECUTION_FAILED` with the stage. Every stage re-downloads the full
  streams, so advancing on a timeout just repeats the same work. Only
  `AttemptError::TryNextStage` advances.

## Telemetry export over HTTPS

Verified end to end by `crates/hof-core/tests/otel_export.rs`. That test
re-runs its own binary as a child with a production-shaped environment and
plays the proxy itself: HTTPS with a private-CA leaf, 401 unless the bearer
token matches. Rules it pins down:

- **The OTLP HTTP path needs an HTTP-client feature.** Without
  `reqwest-blocking-client`, `http/protobuf` fails at startup with "no HTTP
  client is configured" and tracing silently turns off. It is blocking
  because the batch span processor exports from its own thread, which has no
  tokio runtime.
- **Two reqwest majors, two trust stories.** reqwest 0.13 (`rustls`)
  verifies via `rustls-platform-verifier`, i.e. the system store. reqwest
  0.12 (`tracing-loki`, `openidconnect`) with `rustls-tls` trusts only
  bundled webpki roots and rejects step-ca. hof-core's never-imported
  `reqwest-0-12` dependency turns on `rustls-tls-native-roots` through
  feature unification. Removing it breaks Loki over HTTPS (measured).
- **Never set a sampler on the tracer provider builder.** The SDK's default
  config is what reads `OTEL_TRACES_SAMPLER`; an explicit `.with_sampler()`
  silently overrides it.
- **Rejections are loud.** A 401 logs
  `ERROR opentelemetry_sdk: … BatchSpanProcessor.ExportError … status code: 401`
  and `ERROR tracing_loki: couldn't send logs to loki … 401 Unauthorized`.

## Diagnosing with traces and logs

Every actor message runs in a kameo `actor.handle_message` span. It is a
**root** span, *linked* (OTel link, not parent) to the sender, so one download
is several linked traces rather than one. Inside a worker's `StartDownload`,
the whole attempt is a single trace:

```
actor.handle_message (DownloadWorker / StartDownload)
└ handle [video_id, trace_id]
  └ execute_download [video_id, title]
    └ download_video [url, policy]
      └ download.fallback_attempt [stage, video_codec, outcome]   (one per stage)
        └ download.execute [platform_video_id, video_format_id, video_height, video_codec_selected]
          ├ download.format [format_id, path]
          │ └ download_task [task_id, destination]   (runs on the manager's worker, parented back here)
          └ ffmpeg.combine [video_bytes, audio_bytes, preexisting_output_bytes, output_bytes]
            └ process [executable, args, pid, exit_code, duration_ms, timed_out, stderr_tail]
    └ verify_media [path]
```

Key fields:

- `video_id` (ULID) is on every download log line. Start there.
- `trace_id` is recorded on handler spans (`crate::telemetry::record_trace_id`)
  and the HTTP request span when OTLP export is on. Every log line under them
  carries it, so a Loki line can be opened in Tempo.
- `process.stderr_tail` holds the last 2000 chars of any subprocess's stderr,
  which is where FFmpeg and yt-dlp explain themselves.
- `ffmpeg.combine.preexisting_output_bytes` set means a previous attempt left
  debris. An `output_bytes` far below `video_bytes + audio_bytes` means the mux
  did not finish.

When adding a new handler that starts a unit of work, declare
`trace_id = tracing::field::Empty` in its `#[instrument]` fields and call
`crate::telemetry::record_trace_id()` first. When spawning a task or queueing
work for a background loop, carry the span (`.instrument(span)` or
`Span::current()` captured at enqueue). A bare `tokio::spawn` starts a new trace
and loses `video_id`.

LogQL recipes (Loki label `service="hofvarpnir"`, logs are JSON):

```logql
# Everything for one video, oldest first
{service="hofvarpnir"} |= "<video_id>" | json
  | line_format "{{._target}} {{.level}} {{.message}} {{.error}}"

# Subprocess timeouts and failures, with the stderr tail
{service="hofvarpnir"} |~ "Process timed out|Command execution failed|refused to overwrite"
  | json | line_format "{{.video_id}} {{.executable}} {{.message}} {{.stderr_tail}}"

# Fallback stages that failed for a non-format reason
{service="hofvarpnir"} |= "trying next fallback stage" | json
  | line_format "{{.video_id}} {{.stage}} {{.error}}"

# Verification failures by reason
{service="hofvarpnir"} |= "failed verification" | json
  | line_format "{{.video_id}} {{.file_size}} {{.error}}"
```

A file that fails verification with the **same `file_size` across attempts** is
deterministic truncation (a killed mux or a stale file), not network damage.

### Testing guidance for this area

- Targeted fallback tests live in `crates/hof-core/src/ytdlp.rs`.
- Validate end-to-end status/error behavior with:

```bash
cargo test -p hof-core ytdlp::tests::test_fallback_
cargo test -p hof-api download_tests::test_video_response_

# Post-download verification (needs ffmpeg/ffprobe on PATH)
cargo test -p hof-core --lib verify::

# Codec-ladder selection (resolution outranks codec)
cargo test -p hof-core ytdlp::tests::test_browser_preset_
cd patches/yt-dlp-patched && cargo test --test unit selection::ranked
```

## CI Requirements

All PRs must pass:

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
3. `cargo test --all-features`
4. `cargo build --all-features --release`

**Important:** Always run strict clippy with pedantic and nursery lints before submitting changes:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Environment Variables

Required for development:

- `DATABASE_URL` - PostgreSQL connection string
- `PORT` - Server port (default: 3000)
- `YTDLP_PATH` - Path to yt-dlp binary (no underscore between `YT` and `DLP`)
- `SQLX_OFFLINE` - Set to `true` for offline builds

Optional (observability):

- `OTEL_EXPORTER_OTLP_ENDPOINT` - OTLP endpoint for trace export; enables export when set. Base URL only: `http/protobuf` appends `/v1/traces` itself (e.g. `https://otel.homelab.local`, or `http://localhost:4317` for gRPC)
- `OTEL_EXPORTER_OTLP_PROTOCOL` - `grpc` (default) or `http/protobuf`. Use `http/protobuf` behind a reverse proxy.
- `OTEL_EXPORTER_OTLP_HEADERS` / `OTEL_EXPORTER_OTLP_TRACES_HEADERS` - comma-separated `key=value`, values percent-encoded (`Authorization=Bearer%20<token>`); the traces variant wins. Read by the SDK on the HTTP path.
- `OTEL_TRACES_SAMPLER` / `OTEL_TRACES_SAMPLER_ARG` - e.g. `parentbased_traceidratio` + `0.1`. Honored because the provider never sets a sampler; keep it that way.
- `OTEL_SERVICE_NAME` - Service name for traces/logs (default: `hofvarpnir`)
- `LOKI_URL` - Grafana Loki base URL for log shipping; `/loki/api/v1/push` is appended (e.g. `http://localhost:3100`)
- `LOKI_HEADERS` - extra headers for Loki pushes, same format as `OTEL_EXPORTER_OTLP_HEADERS`, so one token string can feed both. An invalid entry disables Loki at startup rather than 401ing every batch.
- `SSL_CERT_FILE` - CA bundle for HTTPS export. Both exporters verify against the system trust store (see "Telemetry export over HTTPS" below), so a private CA such as step-ca works once its root is in this bundle.
- `METRICS_ENABLED` - Set to `true` to enable Prometheus metrics at `/metrics`
- `LOG_FORMAT` - Set to `json` for structured JSON log output
- `DOWNLOAD_VERIFY` - Set to `false`/`0`/`no` to skip post-download verification (default: `true`). Requires `ffprobe` on PATH when enabled; startup fails without it.

Optional (OIDC Authentication):

- `OIDC_ISSUER` - OIDC provider issuer URL (e.g., `https://auth.example.com`). If not set, OIDC is disabled.
- `OIDC_CLIENT_ID` - OAuth2 client ID from your OIDC provider
- `OIDC_CLIENT_SECRET` - OAuth2 client secret from your OIDC provider
- `OIDC_SCOPES` - Comma-separated scopes (default: `openid,profile,email`)
- `OIDC_AUTO_PROVISION` - Create user on first OIDC login (default: `true`)
- `OIDC_REDIRECT_BASE_URL` - Override base URL for callback (e.g., `https://hof.example.com`)
- `OIDC_LOGOUT_REDIRECT` - Enable RP-initiated logout (default: `false`)
- `OIDC_DISCOVERY_TIMEOUT` - OIDC discovery HTTP timeout in seconds (default: `30`)

## Commit Conventions

This repo uses [Conventional Commits](https://www.conventionalcommits.org/) (enforced via
`cog.toml` / cocogitto). Follow the existing history when writing messages.

- **Format:** `type(scope): subject`
  - Subject is **lowercase**, concise, imperative mood, **no trailing period**.
  - Keep to a single line — bodies are the exception, not the rule.
- **Types used:** `feat`, `fix`, `chore` (also `release` for version-bump commits).
- **Scope** is short and contextual to what changed. Observed scopes include:
  `deps`, `version`, `release`, `ci`, `tools`, `flake`, `container`, `oci`, `harbor`,
  `just`, `logo`. For feature work, scope by area (e.g. `api-keys`, `activity`, `schedule`,
  `web`, `core`).
- **Version bumps:** `chore(version): vX.Y.Z`.

Examples from history:

```
feat(logo): add new logo to project
fix(release): run pre-bump cargo check with SQLX_OFFLINE
chore(deps): upgrade flake
chore(version): v0.2.5
```

## Language Standards

- **Edition**: 2024
- **MSRV**: 1.94.0
- **Unsafe**: Forbidden (workspace lint)
- **Clippy**: All + Pedantic enabled
