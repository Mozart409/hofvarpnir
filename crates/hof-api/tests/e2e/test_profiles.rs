//! Profile endpoint CRUD tests.

use axum::http::StatusCode;
use sqlx::PgPool;
use ulid::Ulid;

use crate::helpers::{ApiKeyBuilder, ProfileBuilder, TestApp, UserBuilder, db};

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

    // Verify persisted in DB
    let profile_id = Ulid::from_string(body["id"].as_str().unwrap()).unwrap();
    let (db_name, db_quality, db_retention) = db::fetch_profile_fields(&pool, profile_id)
        .await
        .expect("profile should be in DB");
    assert_eq!(db_name, "My Test Profile");
    assert_eq!(db_quality, "1080p");
    assert_eq!(db_retention, None);
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

    // Verify persisted in DB
    let profile_id = Ulid::from_string(body["id"].as_str().unwrap()).unwrap();
    let (_db_name, _db_quality, db_retention) = db::fetch_profile_fields(&pool, profile_id)
        .await
        .expect("profile should be in DB");
    assert_eq!(db_retention, Some(90));
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

    // Verify persisted in DB
    let (db_name, db_quality, _db_retention) = db::fetch_profile_fields(&pool, profile.id)
        .await
        .expect("profile should be in DB");
    assert_eq!(db_name, "Updated Name");
    assert_eq!(db_quality, "1080p");
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

    // Verify persisted in DB
    let (_db_name, _db_quality, db_retention) = db::fetch_profile_fields(&pool, profile.id)
        .await
        .expect("profile should be in DB");
    assert_eq!(db_retention, None);
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

    // Verify it's gone via API
    let get_response = app
        .server
        .get(&format!("/api/v1/profiles/{}", profile.id))
        .add_header("Authorization", key.bearer())
        .await;
    get_response.assert_status(StatusCode::NOT_FOUND);

    // Verify removed from DB
    let exists = db::profile_exists(&pool, profile.id)
        .await
        .expect("query should succeed");
    assert!(!exists, "deleted profile should not exist in DB");
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

// ---------------------------------------------------------------------------
// quality_wait_hours
// ---------------------------------------------------------------------------

/// Read `quality_wait_hours` straight from the table.
async fn db_quality_wait_hours(pool: &PgPool, profile_id: Ulid) -> Option<i32> {
    sqlx::query_scalar("SELECT quality_wait_hours FROM profiles WHERE id = $1")
        .bind(profile_id.to_string())
        .fetch_one(pool)
        .await
        .expect("profile row")
}

/// `quality_wait_hours` round-trips through create, set, clear and zero, and
/// negative values are rejected without touching the stored value.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn profile_quality_wait_hours_lifecycle(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let create_body = |wait: serde_json::Value| {
        serde_json::json!({
            "user_id": user.id.to_string(),
            "name": "Gated Profile",
            "quality": "Q1080p",
            "naming_template": "{title}-{id}.{ext}",
            "output_dir": "/downloads/gated",
            "quality_wait_hours": wait,
        })
    };

    // Negative on create: 400, and nothing is stored.
    let rejected = app
        .server
        .post("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .json(&create_body(serde_json::json!(-1)))
        .await;
    rejected.assert_status(StatusCode::BAD_REQUEST);
    let err: serde_json::Value = rejected.json();
    assert_eq!(err["error"], "quality_wait_hours must be 0 or greater");

    // Create with 12.
    let created = app
        .server
        .post("/api/v1/profiles")
        .add_header("Authorization", key.bearer())
        .json(&create_body(serde_json::json!(12)))
        .await;
    created.assert_status(StatusCode::CREATED);
    let body: serde_json::Value = created.json();
    assert_eq!(body["quality_wait_hours"], 12);
    let profile_id = Ulid::from_string(body["id"].as_str().unwrap()).unwrap();
    assert_eq!(db_quality_wait_hours(&pool, profile_id).await, Some(12));

    let url = format!("/api/v1/profiles/{profile_id}");
    let put = |payload: serde_json::Value| {
        app.server
            .put(&url)
            .add_header("Authorization", key.bearer())
            .json(&payload)
    };

    // An unrelated update leaves the value alone.
    let renamed: serde_json::Value = put(serde_json::json!({"name": "Renamed"})).await.json();
    assert_eq!(renamed["quality_wait_hours"], 12);

    // 0 disables the gate and is stored as 0, not NULL.
    let zero = put(serde_json::json!({"quality_wait_hours": 0})).await;
    zero.assert_status_ok();
    assert_eq!(zero.json::<serde_json::Value>()["quality_wait_hours"], 0);
    assert_eq!(db_quality_wait_hours(&pool, profile_id).await, Some(0));

    // Negative on update: 400, stored value unchanged.
    put(serde_json::json!({"quality_wait_hours": -5}))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(db_quality_wait_hours(&pool, profile_id).await, Some(0));

    // Set again, then null clears it back to the global default.
    put(serde_json::json!({"quality_wait_hours": 24}))
        .await
        .assert_status_ok();
    assert_eq!(db_quality_wait_hours(&pool, profile_id).await, Some(24));

    let cleared = put(serde_json::json!({"quality_wait_hours": null})).await;
    cleared.assert_status_ok();
    assert!(cleared.json::<serde_json::Value>()["quality_wait_hours"].is_null());
    assert_eq!(db_quality_wait_hours(&pool, profile_id).await, None);

    // GET reflects the persisted state.
    let fetched: serde_json::Value = app
        .server
        .get(&url)
        .add_header("Authorization", key.bearer())
        .await
        .json();
    assert!(fetched["quality_wait_hours"].is_null());
    assert_eq!(fetched["name"], "Renamed");
}
