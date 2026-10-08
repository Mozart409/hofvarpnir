//! Terminal UI client for the Hofvarpnir REST API.
//!
//! Talks to `hof-api` over HTTP only, authenticated with an API key
//! (`Authorization: Bearer hof_sk_...`). See GOALS.md Phase 6.

pub mod app;
pub mod client;
pub mod config;
pub mod run;
pub mod search;
pub mod setup;
pub mod sse;
pub mod types;
pub mod ui;
