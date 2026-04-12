//! System endpoint tests.

use axum::http::StatusCode;

use crate::helpers::{ApiKeyBuilder, TestApp, UserBuilder};

#[tokio::test]
async fn system_status_returns_all_components() {
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

    let body: serde_json::Value = response.json();
    assert!(body.get("scheduler").is_some());
    assert!(body.get("downloads").is_some());
    assert!(body.get("cleanup").is_some());
    assert!(body.get("statistics").is_some());
    assert!(body.get("timestamp").is_some());
}

#[tokio::test]
async fn system_status_includes_statistics() {
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

    let body: serde_json::Value = response.json();
    let stats = &body["statistics"];
    assert!(stats["total_videos"].is_number());
    assert!(stats["pending_downloads"].is_number());
    assert!(stats["downloading"].is_number());
    assert!(stats["completed"].is_number());
    assert!(stats["failed"].is_number());
}

#[tokio::test]
async fn trigger_cleanup_returns_result() {
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

    let body: serde_json::Value = response.json();
    assert!(body.get("message").is_some());
    assert!(body.get("result").is_some());

    let result = &body["result"];
    assert!(result["retention_cleaned"].is_number());
    assert!(result["quota_cleaned"].is_number());
    assert!(result["temp_files_cleaned"].is_number());
    assert!(result["bytes_freed"].is_number());
}

#[tokio::test]
async fn system_status_requires_read_scope() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .delete_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/system/status")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn trigger_cleanup_requires_write_scope() {
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
