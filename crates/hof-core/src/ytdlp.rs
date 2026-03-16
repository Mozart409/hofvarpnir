//! Wrapper around the `yt-dlp` CLI binary.
//!
//! Provides structured invocation for:
//! - Flat playlist indexing (`--flat-playlist --dump-json`)
//! - Video download with progress (`--progress-template`)
//! - Metadata extraction
//!
//! All calls use `tokio::process::Command` with `kill_on_drop(true)`.
