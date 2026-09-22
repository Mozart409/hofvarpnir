//! End-to-end tests for source actions: metadata refresh and reset-order.
//!
//! These tests verify the behavior of endpoints that manipulate source state
//! beyond basic CRUD. The metadata endpoint requires channel metadata to be
//! present (which normally comes from indexing). The reset-order endpoint
//! resets the detected entry order back to Unknown so the scheduler will
//! re-detect it on next index.

use axum::http::StatusCode;
use sqlx::{PgPool, Row};
use ulid::Ulid;

use crate::helpers::{ApiKeyBuilder, ProfileBuilder, SourceBuilder, TestApp, UserBuilder};

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn metadata_trigger_not_found_for_nonexistent_source(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let nonexistent_id = Ulid::generate();
    let path = format!("/api/v1/sources/{nonexistent_id}/metadata");

    let response = app
        .server
        .post(&path)
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);

    let body: serde_json::Value = response.json();
    assert!(body.get("error").is_some());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn metadata_trigger_bad_request_without_channel_metadata(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;

    let source = SourceBuilder::new(profile.id).build(&pool).await;
    let path = format!("/api/v1/sources/{}/metadata", source.id);

    let response = app
        .server
        .post(&path)
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);

    let body: serde_json::Value = response.json();
    assert!(body.get("error").is_some());
    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("channel metadata") || error.contains("Channel"),
        "Error should mention missing channel metadata"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn metadata_trigger_invalid_id_format(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/sources/not-a-ulid/metadata")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);

    let body: serde_json::Value = response.json();
    assert!(body.get("error").is_some());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn metadata_trigger_requires_write_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    let path = format!("/api/v1/sources/{}/metadata", source.id);

    let response = app
        .server
        .post(&path)
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn reset_entry_order_resets_to_unknown_and_persists_in_db(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;

    let source = SourceBuilder::new(profile.id).build(&pool).await;

    // Manually set entry_order to something other than Unknown
    // Use CAST because entry_order is a SQL enum type, not text
    sqlx::query("UPDATE sources SET entry_order = $1::entry_order WHERE id = $2")
        .bind("ascending")
        .bind(source.id.to_string())
        .execute(&pool)
        .await
        .expect("Failed to update source entry order");

    // This path is what `#[utoipa::path]` on `reset_entry_order` now derives:
    // the annotation is relative to the `/api/v1/sources` nest prefix, like
    // every other route in that router. It previously declared the full
    // `/api/sources/{id}/reset-order`, which mounted the handler at
    // `/api/v1/sources/api/sources/{id}/reset-order` and advertised a path in
    // the OpenAPI spec that did not exist. This test is the regression guard.
    let path = format!("/api/v1/sources/{}/reset-order", source.id);

    let response = app
        .server
        .post(&path)
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert_eq!(
        body["entry_order"], "Unknown",
        "Response should show entry_order reset to Unknown"
    );
    assert_eq!(body["id"], source.id.to_string());

    // Verify in database. `entry_order` is a Postgres enum, so it is cast to
    // text here rather than decoded into a `String` directly — and its
    // variants are stored lowercase (see the `CREATE TYPE entry_order`
    // migration), not in the `EntryOrder` Rust spelling.
    let db_row = sqlx::query("SELECT entry_order::text AS entry_order FROM sources WHERE id = $1")
        .bind(source.id.to_string())
        .fetch_one(&pool)
        .await
        .expect("Source should exist");

    let db_entry_order: String = db_row.get("entry_order");
    assert_eq!(
        db_entry_order, "unknown",
        "Database should reflect entry_order reset to unknown"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn reset_entry_order_not_found_for_nonexistent_source(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let nonexistent_id = Ulid::generate();
    // Use the actual working path (see note in reset_entry_order_resets_to_unknown_and_persists_in_db)
    let path = format!("/api/v1/sources/{nonexistent_id}/reset-order");

    let response = app
        .server
        .post(&path)
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);

    let body: serde_json::Value = response.json();
    assert!(body.get("error").is_some());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn reset_entry_order_invalid_id_format(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    // Use the actual working path (see note in reset_entry_order_resets_to_unknown_and_persists_in_db)
    let response = app
        .server
        .post("/api/v1/sources/invalid-ulid/reset-order")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);

    let body: serde_json::Value = response.json();
    assert!(body.get("error").is_some());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn reset_entry_order_requires_write_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    // Use the actual working path (see note in reset_entry_order_resets_to_unknown_and_persists_in_db)
    let path = format!("/api/v1/sources/{}/reset-order", source.id);

    let response = app
        .server
        .post(&path)
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}
