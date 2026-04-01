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

# Run clippy (strict mode)
cargo clippy --all-targets --all-features -- -D warnings

# Run clippy with pedantic lints
bacon pedantic
# Or manually:
cargo clippy --workspace --all-targets -- -W clippy::pedantic

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
bacon pedantic           # Run pedantic clippy
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
  - `Browser` -> direct-play preference (`mp4`, AVC/H.264 + AAC)
  - `Tv` -> direct-play preference (`mp4`, HEVC + AAC)
- `output_preset` is persisted in PostgreSQL (`profiles.output_preset`) and exposed through API + web profile forms.

### Download policy model

- `FormatPolicy` (in `crates/hof-core/src/ytdlp.rs`) is resolved from `(Quality, OutputPreset)`.
- Download fallback is deterministic and staged (`FallbackStage`):
  1. preferred video+audio codec pair
  2. preferred video codec + any audio
  3. any muxable codec pair
  4. then quality is relaxed until exhausted
- On exhaustion, download returns a structured format-unavailable error.

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
```

## CI Requirements

All PRs must pass:

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-features`
4. `cargo build --all-features --release`

**Important:** Always run clippy with pedantic lints before submitting changes:

```bash
cargo clippy --workspace --all-targets -- -W clippy::pedantic
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

## Language Standards

- **Edition**: 2024
- **MSRV**: 1.94.0
- **Unsafe**: Forbidden (workspace lint)
- **Clippy**: All + Pedantic enabled
