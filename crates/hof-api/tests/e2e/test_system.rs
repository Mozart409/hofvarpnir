//! System endpoint tests.

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::helpers::{ApiKeyBuilder, TestApp, UserBuilder};

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn system_status_returns_all_components(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/system/status")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert!(body.get("scheduler").is_some());
    assert!(body.get("downloads").is_some());
    assert!(body.get("cleanup").is_some());
    assert!(body.get("statistics").is_some());
    assert!(body.get("timestamp").is_some());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn system_status_includes_statistics(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/system/status")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    let stats = &body["statistics"];
    assert!(stats["total_videos"].is_number());
    assert!(stats["pending_downloads"].is_number());
    assert!(stats["downloading"].is_number());
    assert!(stats["completed"].is_number());
    assert!(stats["failed"].is_number());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn trigger_cleanup_returns_result(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/system/cleanup")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert!(body.get("message").is_some());
    assert!(body.get("result").is_some());

    let result = &body["result"];
    assert!(result["retention_cleaned"].is_number());
    assert!(result["quota_cleaned"].is_number());
    assert!(result["temp_files_cleaned"].is_number());
    assert!(result["bytes_freed"].is_number());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn system_status_requires_read_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).delete_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/system/status")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn trigger_cleanup_requires_write_scope(pool: PgPool) {
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
async fn restart_download_supervisor_returns_200(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/system/actors/download_supervisor/restart")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert_eq!(body["actor"], "download_supervisor");
    assert!(body.get("message").is_some());

    // Verify system status afterward is still healthy
    let status_response = app
        .server
        .get("/api/v1/system/status")
        .add_header("Authorization", key.bearer())
        .await;

    status_response.assert_status_ok();
    let status_body: serde_json::Value = status_response.json();
    assert!(status_body.get("downloads").is_some());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn restart_scheduler_returns_200(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/system/actors/scheduler/restart")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert_eq!(body["actor"], "scheduler");

    // Verify system status afterward is still healthy
    let status_response = app
        .server
        .get("/api/v1/system/status")
        .add_header("Authorization", key.bearer())
        .await;

    status_response.assert_status_ok();
    let status_body: serde_json::Value = status_response.json();
    assert!(status_body.get("scheduler").is_some());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn restart_cleanup_returns_200(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/system/actors/cleanup/restart")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert_eq!(body["actor"], "cleanup");

    // Verify system status afterward is still healthy
    let status_response = app
        .server
        .get("/api/v1/system/status")
        .add_header("Authorization", key.bearer())
        .await;

    status_response.assert_status_ok();
    let status_body: serde_json::Value = status_response.json();
    assert!(status_body.get("cleanup").is_some());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn restart_jellyfin_metadata_returns_200(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/system/actors/jellyfin_metadata/restart")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert_eq!(body["actor"], "jellyfin_metadata");

    // Verify system status afterward is still healthy
    let status_response = app
        .server
        .get("/api/v1/system/status")
        .add_header("Authorization", key.bearer())
        .await;

    status_response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn restart_actor_unknown_name_returns_400(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/system/actors/nonexistent_actor/restart")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);

    let body: serde_json::Value = response.json();
    assert!(body.get("error").is_some());
    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("Unknown actor"),
        "Error should mention unknown actor"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn restart_actor_requires_write_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/system/actors/scheduler/restart")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}
