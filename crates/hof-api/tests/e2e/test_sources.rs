//! Source endpoint CRUD tests.

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::helpers::{ApiKeyBuilder, ProfileBuilder, SourceBuilder, TestApp, UserBuilder};

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn list_sources_returns_array(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/sources")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn create_source_returns_201(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/sources")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "profile_id": profile.id.to_string(),
            "url": "https://youtube.com/@testchannel",
            "source_type": "Channel",
            "cutoff_date": "2024-01-01"
        }))
        .await;

    response.assert_status(StatusCode::CREATED);

    let body: serde_json::Value = response.json();
    assert_eq!(body["url"], "https://youtube.com/@testchannel");
    assert_eq!(body["source_type"], "Channel");
    assert!(body["id"].is_string());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn create_source_playlist(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/sources")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "profile_id": profile.id.to_string(),
            "url": "https://youtube.com/playlist?list=PLxxxx",
            "source_type": "Playlist",
            "custom_name": "My Playlist",
            "cutoff_date": "2024-06-01",
            "index_frequency_secs": 7200,
            "retention_days": 60
        }))
        .await;

    response.assert_status(StatusCode::CREATED);

    let body: serde_json::Value = response.json();
    assert_eq!(body["source_type"], "Playlist");
    assert_eq!(body["custom_name"], "My Playlist");
    assert_eq!(body["index_frequency_secs"], 7200);
    assert_eq!(body["retention_days"], 60);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn create_source_invalid_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/sources")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "profile_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "url": "https://youtube.com/@test",
            "source_type": "Channel",
            "cutoff_date": "2024-01-01"
        }))
        .await;

    // Foreign key constraint should fail
    response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_source_returns_source(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id)
        .custom_name("Test Source")
        .build(&pool)
        .await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert_eq!(body["id"], source.id.to_string());
    assert_eq!(body["custom_name"], "Test Source");
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_source_not_found(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/sources/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn update_source_partial(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .put(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "custom_name": "Updated Name",
            "index_frequency_secs": 1800
        }))
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert_eq!(body["custom_name"], "Updated Name");
    assert_eq!(body["index_frequency_secs"], 1800);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn update_source_clear_custom_name(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id)
        .custom_name("Original Name")
        .build(&pool)
        .await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .put(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({"custom_name": null}))
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert!(body["custom_name"].is_null());
}

// Note: The `enabled` field is not part of UpdateSourceRequest.
// Sources cannot be disabled via the API (only via direct DB update).

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn delete_source_returns_204(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).full_access().build(&pool).await;

    let response = app
        .server
        .delete(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NO_CONTENT);

    // Verify it's gone
    let get_response = app
        .server
        .get(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .await;
    get_response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn trigger_index_returns_accepted_or_conflict(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post(&format!("/api/v1/sources/{}/index", source.id))
        .add_header("Authorization", key.bearer())
        .await;

    // 202 Accepted for async operation, or 409 Conflict if scheduler already started indexing
    let status = response.status_code();
    assert!(
        status == StatusCode::ACCEPTED || status == StatusCode::CONFLICT,
        "Expected 202 or 409, got {status}"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn trigger_index_source_not_found(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/sources/01ARZ3NDEKTSV4RRFFQ69G5FAV/index")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn list_sources_filter_by_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile1 = ProfileBuilder::new(user.id).build(&pool).await;
    let profile2 = ProfileBuilder::new(user.id).build(&pool).await;

    // Create sources for both profiles
    SourceBuilder::new(profile1.id).build(&pool).await;
    SourceBuilder::new(profile2.id).build(&pool).await;

    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    // Filter by profile1
    let response = app
        .server
        .get(&format!("/api/v1/sources?profile_id={}", profile1.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: Vec<serde_json::Value> = response.json();
    for source in &body {
        assert_eq!(source["profile_id"], profile1.id.to_string());
    }
}
