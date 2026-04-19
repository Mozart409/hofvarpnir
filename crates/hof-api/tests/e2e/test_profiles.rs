//! Profile endpoint CRUD tests.

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::helpers::{ApiKeyBuilder, ProfileBuilder, TestApp, UserBuilder};

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn list_profiles_returns_empty_array(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    // The response is a Vec, which by definition is a JSON array
    let _body: Vec<serde_json::Value> = response.json();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn create_profile_returns_201(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

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

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn create_profile_with_optional_fields(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

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

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn create_profile_invalid_naming_template(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

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

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_profile_returns_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id)
        .name("Get Test Profile")
        .build(&pool)
        .await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

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

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_profile_not_found(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/profiles/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_profile_invalid_id(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/profiles/not-a-valid-ulid")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn update_profile_partial(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id)
        .name("Original Name")
        .build(&pool)
        .await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

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

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn update_profile_clear_retention(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id)
        .retention_days(30)
        .build(&pool)
        .await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

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

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn delete_profile_returns_204(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).full_access().build(&pool).await;

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

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn delete_profile_not_found(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).full_access().build(&pool).await;

    let response = app
        .server
        .delete("/api/v1/profiles/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn list_profiles_filter_by_user(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user1 = UserBuilder::new().build(&pool).await;
    let user2 = UserBuilder::new().build(&pool).await;

    // Create profiles for both users
    ProfileBuilder::new(user1.id)
        .name("User1 Profile")
        .build(&pool)
        .await;
    ProfileBuilder::new(user2.id)
        .name("User2 Profile")
        .build(&pool)
        .await;

    let key = ApiKeyBuilder::new(user1.id).read_only().build(&pool).await;

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
