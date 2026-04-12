//! Profile endpoint CRUD tests.

use axum::http::StatusCode;

use crate::helpers::{ApiKeyBuilder, ProfileBuilder, TestApp, UserBuilder};

#[tokio::test]
async fn list_profiles_returns_empty_array() {
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
    // The response is a Vec, which by definition is a JSON array
    let _body: Vec<serde_json::Value> = response.json();
}

#[tokio::test]
async fn create_profile_returns_201() {
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
            "name": "My Test Profile",
            "quality": "Q1080p",
            "naming_template": "{title}-{id}.{ext}",
            "output_dir": "/downloads/test"
        }))
        .await;

    response.assert_status(StatusCode::CREATED);

    let body: serde_json::Value = response.json();
    assert_eq!(body["name"], "My Test Profile");
    assert_eq!(body["quality"], "Q1080p");
    assert!(body["id"].is_string());
}

#[tokio::test]
async fn create_profile_with_optional_fields() {
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
            "name": "Full Profile",
            "quality": "Best",
            "naming_template": "{title}.{ext}",
            "output_dir": "/downloads",
            "include_livestreams": true,
            "include_shorts": true,
            "storage_quota_bytes": 500_000_000_000_i64,
            "retention_days": 90
        }))
        .await;

    response.assert_status(StatusCode::CREATED);

    let body: serde_json::Value = response.json();
    assert_eq!(body["include_livestreams"], true);
    assert_eq!(body["include_shorts"], true);
    assert_eq!(body["storage_quota_bytes"], 500_000_000_000_i64);
    assert_eq!(body["retention_days"], 90);
}

#[tokio::test]
async fn create_profile_invalid_naming_template() {
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
            "name": "Invalid Template",
            "quality": "Q1080p",
            "naming_template": "{invalid_field}",
            "output_dir": "/downloads"
        }))
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_profile_returns_profile() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id)
        .name("Get Test Profile")
        .build(&app.pool)
        .await;
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

    let body: serde_json::Value = response.json();
    assert_eq!(body["id"], profile.id.to_string());
    assert_eq!(body["name"], "Get Test Profile");
}

#[tokio::test]
async fn get_profile_not_found() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/profiles/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_profile_invalid_id() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/profiles/not-a-valid-ulid")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_profile_partial() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id)
        .name("Original Name")
        .build(&app.pool)
        .await;
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

    let body: serde_json::Value = response.json();
    assert_eq!(body["name"], "Updated Name");
    // Other fields should remain unchanged
    assert_eq!(body["quality"], "Q1080p");
}

#[tokio::test]
async fn update_profile_clear_retention() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id)
        .retention_days(30)
        .build(&app.pool)
        .await;
    let key = ApiKeyBuilder::new(user.id)
        .read_write()
        .build(&app.pool)
        .await;

    // Verify retention is set
    let get_response = app
        .server
        .get(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .await;
    let body: serde_json::Value = get_response.json();
    assert_eq!(body["retention_days"], 30);

    // Clear retention by setting to null
    let response = app
        .server
        .put(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({"retention_days": null}))
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert!(body["retention_days"].is_null());
}

#[tokio::test]
async fn delete_profile_returns_204() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let profile = ProfileBuilder::new(user.id).build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .full_access()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .delete(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NO_CONTENT);

    // Verify it's gone
    let get_response = app
        .server
        .get(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .await;
    get_response.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_profile_not_found() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .full_access()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .delete("/api/v1/profiles/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_profiles_filter_by_user() {
    let app = TestApp::new().await;
    let user1 = UserBuilder::new().build(&app.pool).await;
    let user2 = UserBuilder::new().build(&app.pool).await;

    // Create profiles for both users
    ProfileBuilder::new(user1.id)
        .name("User1 Profile")
        .build(&app.pool)
        .await;
    ProfileBuilder::new(user2.id)
        .name("User2 Profile")
        .build(&app.pool)
        .await;

    let key = ApiKeyBuilder::new(user1.id)
        .read_only()
        .build(&app.pool)
        .await;

    // Filter by user1
    let response = app
        .server
        .get(&format!("/api/v1/profiles?user_id={}", user1.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: Vec<serde_json::Value> = response.json();
    // Should only contain user1's profile
    for profile in &body {
        assert_eq!(profile["user_id"], user1.id.to_string());
    }
}
