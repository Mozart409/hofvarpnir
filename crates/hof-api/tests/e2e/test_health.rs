//! Health endpoint tests.
//!
//! Health endpoints are public - no authentication required.

use crate::helpers::TestApp;

#[tokio::test]
async fn health_check_returns_200() {
    let app = TestApp::new().await;

    let response = app.server.get("/api/health").await;

    response.assert_status_ok();
}

#[tokio::test]
async fn liveness_returns_200() {
    let app = TestApp::new().await;

    let response = app.server.get("/api/health/live").await;

    response.assert_status_ok();
}

#[tokio::test]
async fn readiness_returns_200_when_db_connected() {
    let app = TestApp::new().await;

    let response = app.server.get("/api/health/ready").await;

    response.assert_status_ok();
}

#[tokio::test]
async fn health_check_includes_components() {
    let app = TestApp::new().await;

    let response = app.server.get("/api/health").await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert!(body.get("status").is_some());
    assert!(body.get("database").is_some());
    assert!(body.get("ytdlp").is_some());
}
