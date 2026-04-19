//! Health endpoint tests.
//!
//! Health endpoints are public - no authentication required.

use sqlx::PgPool;

use crate::helpers::TestApp;

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn health_check_returns_200(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/api/health").await;

    response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn liveness_returns_200(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/api/health/live").await;

    response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn readiness_returns_200_when_db_connected(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/api/health/ready").await;

    response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn health_check_includes_components(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/api/health").await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert!(body.get("status").is_some());
    assert!(body.get("database").is_some());
    assert!(body.get("ytdlp").is_some());
}
