//! Core library for Hofvarpnir video archival system.
//!
//! This crate provides:
//! - Domain types for users, profiles, sources, and videos
//! - Database operations via `SQLx`
//! - yt-dlp wrapper for downloading and metadata extraction
//! - Actor system for managing concurrent downloads and scheduling
//! - Startup and crash recovery logic
//! - Jellyfin metadata generation (NFO files and artwork)

pub mod actors;
pub mod auth;
pub mod config;
pub mod db;
pub mod domain;
pub mod jellyfin;
pub mod metrics;
pub mod startup;
pub mod telemetry;
pub mod ytdlp;

// Re-export commonly used types
pub use config::Config;
pub use db::ActivityBroadcaster;
pub use startup::{ActorSystem, initialize, shutdown};
pub use telemetry::{RequestSpan, TelemetryGuard, UlidRequestId, init_tracing};
