//! Authentication and authorization tests.
//!
//! Tests the auth matrix: which endpoints require which scopes,
//! and proper 401/403 responses for missing/insufficient auth.

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::helpers::{ApiKeyBuilder, ProfileBuilder, SourceBuilder, TestApp, UserBuilder};

// ============================================================================
// No Auth -> 401 Unauthorized
// ============================================================================

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn profiles_list_without_auth_returns_401(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/api/v1/profiles").await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn sources_list_without_auth_returns_401(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/api/v1/sources").await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn downloads_list_without_auth_returns_401(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/api/v1/downloads").await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn system_status_without_auth_returns_401(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/api/v1/system/status").await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn activity_list_without_auth_returns_401(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/api/v1/activity").await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

// ============================================================================
// Invalid Token -> 401 Unauthorized
// ============================================================================

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn invalid_token_returns_401(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app
        .server
        .get("/api/v1/profiles")
        .add_header("Authorization", "Bearer invalid_token")
        .await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn malformed_bearer_returns_401(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app
        .server
        .get("/api/v1/profiles")
        .add_header("Authorization", "NotBearer hof_sk_xxx")
        .await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn expired_token_returns_401(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).expired().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

// ============================================================================
// Read Scope Tests
// ============================================================================

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn read_token_can_list_profiles(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn read_token_can_get_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn read_token_cannot_create_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "user_id": user.id.to_string(),
            "name": "Test",
            "quality": "Q1080p",
            "naming_template": "{title}.{ext}",
            "output_dir": "/tmp/test"
        }))
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn read_token_cannot_update_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .put(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({"name": "Updated"}))
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn read_token_cannot_delete_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .delete(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

// ============================================================================
// Write Scope Tests
// ============================================================================

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn write_token_can_create_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "user_id": user.id.to_string(),
            "name": "Test Profile",
            "quality": "Q1080p",
            "naming_template": "{title}.{ext}",
            "output_dir": "/tmp/test"
        }))
        .await;

    response.assert_status(StatusCode::CREATED);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn write_token_can_update_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .put(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({"name": "Updated Name"}))
        .await;

    response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn write_token_cannot_delete_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .delete(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

// ============================================================================
// Delete Scope Tests
// ============================================================================

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn delete_token_cannot_read(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).delete_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn delete_token_cannot_write(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).delete_only().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "user_id": user.id.to_string(),
            "name": "Test",
            "quality": "Q1080p",
            "naming_template": "{title}.{ext}",
            "output_dir": "/tmp/test"
        }))
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn delete_token_can_delete_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).delete_only().build(&pool).await;

    let response = app
        .server
        .delete(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NO_CONTENT);
}

// ============================================================================
// Full Access Tests
// ============================================================================

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn full_access_token_can_do_everything(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).full_access().build(&pool).await;

    // Create
    let create_response = app
        .server
        .post("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "user_id": user.id.to_string(),
            "name": "Full Access Test",
            "quality": "Q1080p",
            "naming_template": "{title}.{ext}",
            "output_dir": "/tmp/test"
        }))
        .await;
    create_response.assert_status(StatusCode::CREATED);

    let created: serde_json::Value = create_response.json();
    let profile_id = created["id"].as_str().unwrap();

    // Read
    let read_response = app
        .server
        .get(&format!("/api/v1/profiles/{profile_id}"))
        .add_header("Authorization", key.bearer())
        .await;
    read_response.assert_status_ok();

    // Update
    let update_response = app
        .server
        .put(&format!("/api/v1/profiles/{profile_id}"))
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({"name": "Updated"}))
        .await;
    update_response.assert_status_ok();

    // Delete
    let delete_response = app
        .server
        .delete(&format!("/api/v1/profiles/{profile_id}"))
        .add_header("Authorization", key.bearer())
        .await;
    delete_response.assert_status(StatusCode::NO_CONTENT);
}

// ============================================================================
// Source Auth Tests
// ============================================================================

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn read_token_can_list_sources(pool: PgPool) {
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
async fn read_token_cannot_create_source(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/sources")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "profile_id": profile.id.to_string(),
            "url": "https://youtube.com/@test",
            "source_type": "Channel",
            "cutoff_date": "2024-01-01"
        }))
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn write_token_can_trigger_index(pool: PgPool) {
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
    // Both indicate auth succeeded (not 401/403)
    let status = response.status_code();
    assert!(
        status == StatusCode::ACCEPTED || status == StatusCode::CONFLICT,
        "Expected 202 or 409, got {status}"
    );
}

// ============================================================================
// System Endpoint Auth Tests
// ============================================================================

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn read_token_can_get_system_status(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/system/status")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn read_token_cannot_trigger_cleanup(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/system/cleanup")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn write_token_can_trigger_cleanup(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/system/cleanup")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}
