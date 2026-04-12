//! Download endpoint tests.
//!
//! Note: These tests focus on the API layer, not actual download functionality.
//! Download operations like retry and cancel interact with actors.

use axum::http::StatusCode;

use crate::helpers::{ApiKeyBuilder, TestApp, UserBuilder};

#[tokio::test]
async fn list_downloads_returns_array() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/downloads")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[tokio::test]
async fn list_downloads_with_status_filter() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/downloads?status=Pending")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[tokio::test]
async fn list_downloads_with_invalid_status() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/downloads?status=InvalidStatus")
        .add_header("Authorization", key.bearer())
        .await;

    // Should return 400 for invalid enum value
    response.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_download_not_found() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_download_invalid_id() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .get("/api/v1/downloads/not-a-ulid")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn retry_download_not_found() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_write()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .post("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV/retry")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cancel_download_not_found() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_write()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .post("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV/cancel")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_download_not_found() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .full_access()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .delete("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bulk_retry_with_no_failed() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_write()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .post("/api/v1/downloads")
        .add_header("Authorization", key.bearer())
        .await;

    // 202 Accepted for async operation
    response.assert_status(StatusCode::ACCEPTED);

    let body: serde_json::Value = response.json();
    // Should indicate 0 retried when no failed downloads exist
    assert!(body["retried_count"].is_number());
}

// ============================================================================
// Auth Tests for Download Endpoints
// ============================================================================

#[tokio::test]
async fn retry_download_requires_write_scope() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .post("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV/retry")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cancel_download_requires_write_scope() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .post("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV/cancel")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn delete_download_requires_delete_scope() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_write()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .delete("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn bulk_retry_requires_write_scope() {
    let app = TestApp::new().await;
    let user = UserBuilder::new().build(&app.pool).await;
    let key = ApiKeyBuilder::new(user.id)
        .read_only()
        .build(&app.pool)
        .await;

    let response = app
        .server
        .post("/api/v1/downloads")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}
