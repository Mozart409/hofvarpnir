//! Core library for Hofvarpnir video archival system.
//!
//! This crate provides:
//! - Domain types for users, profiles, sources, and videos
//! - Database operations via `SQLx`
//! - yt-dlp wrapper for downloading and metadata extraction
//! - Actor system for managing concurrent downloads and scheduling
//! - Startup and crash recovery logic
//! - Jellyfin metadata generation (NFO files and artwork)

// The `Send` proof for `StartDownload`'s future walks the whole download
// pipeline (fallback stages, segmented downloader, ffmpeg combine, verify)
// and exceeds the default depth of 128, tripping the future-incompatible
// `recursion_depth_exceeding_limit` lint (rust-lang/rust#159228).
#![recursion_limit = "256"]

pub mod actors;
pub mod auth;
pub mod config;
pub mod db;
pub mod domain;
pub mod jellyfin;
pub mod liveness;
pub mod metrics;
pub mod oidc;
pub mod runtime_config;
pub mod startup;
pub mod telemetry;
pub mod verify;
pub mod watchdog;
pub mod ytdlp;

// Re-export commonly used types
pub use config::Config;
pub use db::ActivityBroadcaster;
pub use startup::{ActorSystem, initialize, shutdown};
pub use telemetry::{
    HttpResponseRecorder, RequestSpan, TelemetryGuard, UlidRequestId, init_tracing,
};
