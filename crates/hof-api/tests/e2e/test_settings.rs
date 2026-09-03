//! Settings/pause/shutdown endpoint tests.
//!
//! Most of `settings.rs`'s behavior is covered by pure unit tests in that
//! same file (see its `#[cfg(test)] mod tests`), per this task's own
//! instruction not to stand up an axum test server for every case. These
//! tests exist specifically to cover what unit tests structurally cannot:
//! that the routes are actually registered/reachable behind the real
//! `hof_api::router` merge, and that the *handler* (not just the underlying
//! `DrainToken`) honors the shutdown idempotence guarantee end-to-end.

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::helpers::{ApiKeyBuilder, TestApp, UserBuilder};

/// Ruling R-N: a repeated `POST /shutdown` must report the deadline derived
/// from the ORIGINAL drain start time, not a freshly computed `now +
/// timeout`. The unit test `repeated_begin_leaves_deadline_anchored_to_first_start`
/// in `settings.rs` proves this for `DrainToken` directly, but never calls
/// the `shutdown` handler — a handler that computed `Utc::now() + timeout`
/// itself instead of reading `DrainStatusResponse::new`'s derived value
/// would still pass every existing test. This closes that hole over real
/// HTTP.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn shutdown_is_idempotent_over_http(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let first = app
        .server
        .post("/api/v1/system/shutdown")
        .add_header("Authorization", key.bearer())
        .await;
    first.assert_status(StatusCode::ACCEPTED);
    let first_body: serde_json::Value = first.json();
    assert_eq!(first_body["drain"]["draining"], true);
    let first_deadline = first_body["drain"]["deadline"].clone();
    assert!(
        !first_deadline.is_null(),
        "drain.deadline must be set once draining has begun"
    );

    let second = app
        .server
        .post("/api/v1/system/shutdown")
        .add_header("Authorization", key.bearer())
        .await;
    second.assert_status(StatusCode::ACCEPTED);
    let second_body: serde_json::Value = second.json();
    let second_deadline = second_body["drain"]["deadline"].clone();

    assert_eq!(
        first_deadline, second_deadline,
        "a repeated POST /shutdown must report the ORIGINAL drain deadline \
         (ruling R-N), not one recomputed from the second call's start time"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn shutdown_requires_write_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/system/shutdown")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

/// A `#[utoipa::path]` written with an absolute path (e.g.
/// `path = "/api/v1/system/settings"`) instead of a path relative to the
/// nest prefix (`path = "/settings"`) produces a consistently-wrong route in
/// BOTH the axum router and the `OpenAPI` spec — so a naive reachability
/// check that only probes paths pulled from the spec itself would never
/// catch it (this exact bug already exists elsewhere in this codebase, at
/// `sources.rs`'s `reset_order` route; out of scope for this task, tracked
/// separately). Asserting the exact literal path strings here pins them
/// down against that class of mistake instead.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn openapi_spec_includes_new_settings_paths(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/docs/openapi.json").await;
    response.assert_status_ok();

    let spec: serde_json::Value = response.json();
    let paths = spec
        .get("paths")
        .and_then(|p| p.as_object())
        .expect("paths should exist");

    for expected in [
        "/api/v1/system/settings",
        "/api/v1/system/pause",
        "/api/v1/system/shutdown",
    ] {
        assert!(
            paths.contains_key(expected),
            "OpenAPI spec is missing path '{expected}'; got: {:?}",
            paths.keys().collect::<Vec<_>>()
        );
    }
}
