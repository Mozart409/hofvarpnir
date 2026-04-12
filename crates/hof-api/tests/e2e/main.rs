//! End-to-end API tests.
//!
//! Tests all API endpoints with different authentication scopes.
//! Tests run in parallel - each creates its own entities with unique IDs.

// Relax some clippy lints for test code
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::literal_string_with_formatting_args)]
#![allow(dead_code)]

mod helpers;

mod test_auth;
mod test_downloads;
mod test_health;
mod test_profiles;
mod test_sources;
mod test_system;
