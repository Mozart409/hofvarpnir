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
just test                # Run tests with DB setup
just dev                 # Run web server
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

The test entry points (`just test`, `e2e`, `e2e-only`, `ci`, and the bacon `test`/`nextest` jobs) do not use the dev database: they override `DATABASE_URL` to a dedicated, ephemeral Postgres (`postgres-test` service in `containers/compose.dev.yml`, localhost:5433, no monitoring extensions, durability disabled). Override with `TEST_DATABASE_URL` (just) if needed. The bacon `run`/`serve`/`tui` jobs still use the dev database, as does `just dev`.

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

These codes are propagated into worker/supervisor logs and persisted failure text (`[CODE] ...`).
API download responses expose parsed `last_error_code` when available.

### Testing guidance for this area

- Targeted fallback tests live in `crates/hof-core/src/ytdlp.rs`.
- Validate end-to-end status/error behavior with:

```bash
cargo test -p hof-core ytdlp::tests::test_fallback_
cargo test -p hof-api download_tests::test_video_response_

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
- `YT_DLP_PATH` - Path to yt-dlp binary
- `SQLX_OFFLINE` - Set to `true` for offline builds

Optional (observability):

- `OTEL_EXPORTER_OTLP_ENDPOINT` - OTLP gRPC endpoint for trace export (e.g. `http://localhost:4317`)
- `OTEL_SERVICE_NAME` - Service name for traces/logs (default: `hofvarpnir`)
- `LOKI_URL` - Grafana Loki endpoint for log shipping (e.g. `http://localhost:3100`)
- `METRICS_ENABLED` - Set to `true` to enable Prometheus metrics at `/metrics`
- `LOG_FORMAT` - Set to `json` for structured JSON log output

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
