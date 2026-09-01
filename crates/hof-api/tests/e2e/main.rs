//! End-to-end API tests.
//!
//! Tests all API endpoints with different authentication scopes.
//! Tests run in parallel - each creates its own entities with unique IDs.

// Relax some clippy lints for test code
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::literal_string_with_formatting_args)]
#![allow(dead_code)]
// `clippy.toml` sets `allow-unwrap-in-tests` / `allow-expect-in-tests`, but those
// only apply inside `#[test]` functions and `#[cfg(test)]` modules. This is an
// integration-test crate whose failures live in plain helper functions and in
// `#[tokio::test]` bodies, neither of which clippy treats as test context — so
// the exemption has to be declared here instead.
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

mod helpers;

mod test_activity;
mod test_auth;
mod test_downloads;
mod test_health;
mod test_openapi;
mod test_profiles;
mod test_sources;
mod test_system;
