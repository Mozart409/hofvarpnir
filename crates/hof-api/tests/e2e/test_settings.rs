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
use serde_json::json;
use sqlx::{PgPool, Row};

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
/// catch it. `sources.rs`'s `reset_entry_order` route shipped with exactly
/// this bug — it declared `path = "/api/sources/{id}/reset-order"` and so
/// mounted at `/api/v1/sources/api/sources/{id}/reset-order`, unreachable at
/// the path the spec advertised, until `test_source_actions.rs` caught it.
/// Asserting the exact literal path strings here pins them down against that
/// class of mistake instead.
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

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_settings_returns_defaults_with_provenance(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/system/settings")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    // Check that required fields exist
    assert!(body.get("pause").is_some());
    assert!(body.get("max_concurrent_downloads").is_some());
    assert!(body.get("max_indexers_per_tick").is_some());
    assert!(body.get("rate_limit_delay_secs").is_some());
    assert!(body.get("check_interval_secs").is_some());
    assert!(body.get("cleanup_interval_secs").is_some());
    assert!(body.get("drain_timeout_secs").is_some());

    // Check that each resolved setting has provenance
    let max_downloads = &body["max_concurrent_downloads"];
    assert!(max_downloads.get("value").is_some());
    assert!(max_downloads.get("provenance").is_some());

    let pause_state = &body["pause"];
    assert!(pause_state.get("indexing").is_some());
    assert!(pause_state.get("downloads").is_some());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn patch_settings_updates_response_and_db(pool: PgPool) {
    // Asserts a PATCH reaching the actors, so this one needs the listener.
    let app = TestApp::with_settings_listener(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let new_max_concurrent = 5;
    // Typed `i32` to match the column it is asserted against below, so the
    // comparison needs no lossy cast.
    let new_rate_limit_secs = 10i32;

    let patch_response = app
        .server
        .patch("/api/v1/system/settings")
        .add_header("Authorization", key.bearer())
        .json(&json!({
            "max_concurrent_downloads": new_max_concurrent,
            "rate_limit_delay_secs": new_rate_limit_secs
        }))
        .await;

    patch_response.assert_status_ok();

    let patch_body: serde_json::Value = patch_response.json();
    assert_eq!(
        patch_body["max_concurrent_downloads"]["value"], new_max_concurrent,
        "PATCH response should reflect the new value"
    );
    assert_eq!(
        patch_body["rate_limit_delay_secs"]["value"], new_rate_limit_secs,
        "PATCH response should reflect the new rate limit"
    );

    // A subsequent GET is served from the in-process watch channel, which the
    // PATCH reaches asynchronously over LISTEN/NOTIFY — so wait for the
    // change to land rather than either racing it or skipping the check. This
    // is what proves the patch reached the running actors, not just the row.
    app.wait_for_settings(&key.bearer(), |body| {
        body["max_concurrent_downloads"]["value"] == json!(new_max_concurrent)
            && body["rate_limit_delay_secs"]["value"] == json!(new_rate_limit_secs)
    })
    .await;

    // Verify in database directly
    let db_row = sqlx::query(
        "SELECT max_concurrent_downloads, rate_limit_delay_secs FROM runtime_settings WHERE id = true"
    )
    .fetch_one(&pool)
    .await
    .expect("runtime_settings row should exist");

    let db_max_concurrent: Option<i32> = db_row.get("max_concurrent_downloads");
    let db_rate_limit: Option<i32> = db_row.get("rate_limit_delay_secs");

    assert_eq!(
        db_max_concurrent,
        Some(new_max_concurrent),
        "Database row should reflect the new max concurrent"
    );
    assert_eq!(
        db_rate_limit,
        Some(new_rate_limit_secs),
        "Database row should reflect the new rate limit"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn patch_settings_out_of_range_returns_400_and_leaves_db_unchanged(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    // First, set a known value so we can verify it doesn't change
    app.server
        .patch("/api/v1/system/settings")
        .add_header("Authorization", key.bearer())
        .json(&json!({
            "max_concurrent_downloads": 3
        }))
        .await
        .assert_status_ok();

    // Read the current state
    let current =
        sqlx::query("SELECT max_concurrent_downloads FROM runtime_settings WHERE id = true")
            .fetch_one(&pool)
            .await
            .expect("runtime_settings row should exist");
    let original_value: Option<i32> = current.get("max_concurrent_downloads");

    // Try to patch with out-of-range value (0 is below minimum of 1)
    let response = app
        .server
        .patch("/api/v1/system/settings")
        .add_header("Authorization", key.bearer())
        .json(&json!({
            "max_concurrent_downloads": 0
        }))
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);

    let error_body: serde_json::Value = response.json();
    assert!(
        error_body.get("error").is_some(),
        "Error response should have error field"
    );

    // Verify DB is unchanged
    let after_patch =
        sqlx::query("SELECT max_concurrent_downloads FROM runtime_settings WHERE id = true")
            .fetch_one(&pool)
            .await
            .expect("runtime_settings row should exist");
    let after_value: Option<i32> = after_patch.get("max_concurrent_downloads");

    assert_eq!(
        original_value, after_value,
        "Database row should be unchanged after failed PATCH"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn patch_settings_unknown_field_rejected(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .patch("/api/v1/system/settings")
        .add_header("Authorization", key.bearer())
        .json(&json!({
            "max_concurrent_downloads": 5,
            "unknown_field": "should_be_rejected"
        }))
        .await;

    // Axum returns 422 (Unprocessable Entity) for JSON deserialization errors
    // (unknown fields with deny_unknown_fields), not 400
    response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    // The response body is plain text describing the error, not JSON
    // Just verify we got rejected; don't try to parse the response body
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn patch_settings_explicit_null_resets_to_default(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    // First, patch to a non-default value
    app.server
        .patch("/api/v1/system/settings")
        .add_header("Authorization", key.bearer())
        .json(&json!({
            "max_concurrent_downloads": 7
        }))
        .await
        .assert_status_ok();

    // Now reset to default with explicit null
    let reset_response = app
        .server
        .patch("/api/v1/system/settings")
        .add_header("Authorization", key.bearer())
        .json(&json!({
            "max_concurrent_downloads": serde_json::Value::Null
        }))
        .await;

    reset_response.assert_status_ok();

    let reset_body: serde_json::Value = reset_response.json();
    // After reset, the value should come from env or default
    // (we can't assert a specific value without knowing the env, but we can verify
    // it changed from the patched value)
    assert_ne!(
        reset_body["max_concurrent_downloads"]["value"], 7,
        "Explicit null reset should have changed the value from 7"
    );

    // Verify DB has NULL
    let db_row =
        sqlx::query("SELECT max_concurrent_downloads FROM runtime_settings WHERE id = true")
            .fetch_one(&pool)
            .await
            .expect("runtime_settings row should exist");

    let db_value: Option<i32> = db_row.get("max_concurrent_downloads");
    assert!(
        db_value.is_none(),
        "Database column should be NULL after explicit null reset"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn pause_and_resume_indexing_persists_in_db(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    // Pause indexing for 1 hour
    let pause_response = app
        .server
        .post("/api/v1/system/pause")
        .add_header("Authorization", key.bearer())
        .json(&json!({
            "module": "indexing",
            "duration_secs": 3600
        }))
        .await;

    pause_response.assert_status_ok();

    let pause_body: serde_json::Value = pause_response.json();
    assert!(
        pause_body["pause"]["indexing"]["paused"]
            .as_bool()
            .unwrap_or(false),
        "Indexing should be paused"
    );

    // Verify in database that the pause was recorded
    let db_row = sqlx::query("SELECT indexing_paused_until FROM runtime_settings WHERE id = true")
        .fetch_one(&pool)
        .await
        .expect("runtime_settings row should exist");

    let db_paused_until: Option<chrono::DateTime<chrono::Utc>> =
        db_row.get("indexing_paused_until");
    assert!(
        db_paused_until.is_some(),
        "Database should have indexing_paused_until set"
    );

    // Resume indexing
    let resume_response = app
        .server
        .delete("/api/v1/system/pause")
        .add_header("Authorization", key.bearer())
        .add_query_param("module", "indexing")
        .await;

    resume_response.assert_status_ok();

    let resume_body: serde_json::Value = resume_response.json();
    assert!(
        !resume_body["pause"]["indexing"]["paused"]
            .as_bool()
            .unwrap_or(true),
        "Indexing should be resumed"
    );

    // Verify in database that the pause was cleared
    let db_row_after =
        sqlx::query("SELECT indexing_paused_until FROM runtime_settings WHERE id = true")
            .fetch_one(&pool)
            .await
            .expect("runtime_settings row should exist");

    let db_paused_until_after: Option<chrono::DateTime<chrono::Utc>> =
        db_row_after.get("indexing_paused_until");
    assert!(
        db_paused_until_after.is_none(),
        "Database should have indexing_paused_until cleared after resume"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn patch_settings_requires_write_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .patch("/api/v1/system/settings")
        .add_header("Authorization", key.bearer())
        .json(&json!({
            "max_concurrent_downloads": 5
        }))
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn pause_requires_write_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/system/pause")
        .add_header("Authorization", key.bearer())
        .json(&json!({
            "module": "indexing",
            "duration_secs": 3600
        }))
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}
