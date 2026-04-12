//! Authentication and authorization tests.
//!
//! Tests the auth matrix: which endpoints require which scopes,
//! and proper 401/403 responses for missing/insufficient auth.

use axum::http::StatusCode;

use crate::helpers::{ApiKeyBuilder, ProfileBuilder, SourceBuilder, TestApp, UserBuilder};

// ============================================================================
// No Auth -> 401 Unauthorized
// ============================================================================

#[tokio::test]
async fn profiles_list_without_auth_returns_401() {
    let app = TestApp::new().await;

    let response = app.server.get("/api/v1/profiles").await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sources_list_without_auth_returns_401() {
    let app = TestApp::new().await;

    let response = app.server.get("/api/v1/sources").await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn downloads_list_without_auth_returns_401() {
    let app = TestApp::new().await;

    let response = app.server.get("/api/v1/downloads").await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn system_status_without_auth_returns_401() {
    let app = TestApp::new().await;

    let response = app.server.get("/api/v1/system/status").await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn activity_list_without_auth_returns_401() {
    let app = TestApp::new().await;

    let response = app.server.get("/api/v1/activity").await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

// ============================================================================
// Invalid Token -> 401 Unauthorized
// ============================================================================

#[tokio::test]
async fn invalid_token_returns_401() {
    let app = TestApp::new().await;

    let response = app
        .server
        .get("/api/v1/profiles")
        .add_header("Authorization", "Bearer invalid_token")
        .await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn malformed_bearer_returns_401() {
    let app = TestApp::new().await;

    let response = app
        .server
        .get("/api/v1/profiles")
        .add_header("Authorization", "NotBearer hof_sk_xxx")
        .await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn expired_token_returns_401() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id).expired().build(&app.pool).await;

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

#[tokio::test]
async fn read_token_can_list_profiles() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[tokio::test]
async fn read_token_can_get_profile() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id).build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[tokio::test]
async fn read_token_cannot_create_profile() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

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

#[tokio::test]
async fn read_token_cannot_update_profile() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id).build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .put(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({"name": "Updated"}))
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn read_token_cannot_delete_profile() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id).build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

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

#[tokio::test]
async fn write_token_can_create_profile() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_write()
        .build(&app.pool)
        .await;

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

#[tokio::test]
async fn write_token_can_update_profile() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id).build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_write()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .put(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({"name": "Updated Name"}))
        .await;

    response.assert_status_ok();
}

#[tokio::test]
async fn write_token_cannot_delete_profile() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id).build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_write()
        .build(&app.pool)
        .await;

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

#[tokio::test]
async fn delete_token_cannot_read() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .delete_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn delete_token_cannot_write() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .delete_only()
        .build(&app.pool)
        .await;

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

#[tokio::test]
async fn delete_token_can_delete_profile() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id).build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .delete_only()
        .build(&app.pool)
        .await;

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

#[tokio::test]
async fn full_access_token_can_do_everything() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .full_access()
        .build(&app.pool)
        .await;

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

#[tokio::test]
async fn read_token_can_list_sources() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/sources")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[tokio::test]
async fn read_token_cannot_create_source() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id).build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

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

#[tokio::test]
async fn write_token_can_trigger_index() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id).build(&app.pool).await;
    let source = SourceBuilder::new(profile.id).build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_write()
        .build(&app.pool)
        .await;

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

#[tokio::test]
async fn read_token_can_get_system_status() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/system/status")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[tokio::test]
async fn read_token_cannot_trigger_cleanup() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .post("/api/v1/system/cleanup")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn write_token_can_trigger_cleanup() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_write()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .post("/api/v1/system/cleanup")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}
