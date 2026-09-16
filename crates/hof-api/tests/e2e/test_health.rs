//! Health endpoint tests.
//!
//! Health endpoints are public - no authentication required.

use axum::http::StatusCode;
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

/// The case the endpoint exists for: an actor has exhausted its restart
/// budget, so no in-process restart can recover it and this process should be
/// replaced. `/live` must say so, because that 503 is what drives a
/// `livenessProbe` (or, under compose, the watchdog's own self-exit).
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn liveness_returns_503_when_unrecoverable(pool: PgPool) {
    let app = TestApp::new(pool).await;

    app.server.get("/api/health/live").await.assert_status_ok();

    app.liveness.set_alive(false);

    let response = app.server.get("/api/health/live").await;
    response.assert_status(StatusCode::SERVICE_UNAVAILABLE);

    // Readiness and liveness are different questions; tripping one must not
    // silently depend on the other having been tripped too.
    app.liveness.set_alive(true);
    app.server.get("/api/health/live").await.assert_status_ok();
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
