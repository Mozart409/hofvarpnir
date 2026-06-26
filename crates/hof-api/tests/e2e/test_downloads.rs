//! Download endpoint tests.
//!
//! Note: These tests focus on the API layer, not actual download functionality.
//! Download operations like retry and cancel interact with actors.

use axum::http::StatusCode;
use hof_core::db;
use sqlx::PgPool;
use ulid::Ulid;

use crate::helpers::{ApiKeyBuilder, ProfileBuilder, SourceBuilder, TestApp, UserBuilder};

/// Seed a completed video downloaded `days_ago` in the past, linked to `source_id`.
async fn seed_completed_video(
    pool: &PgPool,
    source_id: Ulid,
    platform_video_id: &str,
    days_ago: i32,
) -> Ulid {
    let video = db::create_video(
        pool,
        db::CreateVideo {
            platform: "youtube",
            platform_video_id,
            title: "Test Video",
            description: None,
            duration_secs: Some(100),
            published_at: None,
            thumbnail_url: None,
        },
    )
    .await
    .expect("create video");

    sqlx::query(
        r"
        UPDATE videos
        SET status = 'completed',
            downloaded_at = NOW() - make_interval(days => $2),
            file_path = '/tmp/test.mp4',
            file_size_bytes = 1000
        WHERE id = $1
        ",
    )
    .bind(video.id.to_string())
    .bind(days_ago)
    .execute(pool)
    .await
    .expect("mark video completed");

    db::link_video_to_source(pool, source_id, video.id)
        .await
        .expect("link video to source");

    video.id
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn list_downloads_returns_array(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/downloads")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn list_downloads_with_status_filter(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/downloads?status=Pending")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn list_downloads_with_invalid_status(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/downloads?status=InvalidStatus")
        .add_header("Authorization", key.bearer())
        .await;

    // Should return 400 for invalid enum value
    response.assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_download_not_found(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_download_invalid_id(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/downloads/not-a-ulid")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn retry_download_not_found(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV/retry")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn cancel_download_not_found(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV/cancel")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn delete_download_not_found(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).full_access().build(&pool).await;

    let response = app
        .server
        .delete("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn bulk_retry_with_no_failed(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

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

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn retry_download_requires_write_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV/retry")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn cancel_download_requires_write_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV/cancel")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn delete_download_requires_delete_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .delete("/api/v1/downloads/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn bulk_retry_requires_write_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/downloads")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn pending_deletion_lists_video_with_scheduled_time(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let profile = ProfileBuilder::new(user.id)
        .retention_days(30)
        .build(&pool)
        .await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;
    let video_id = seed_completed_video(&pool, source.id, "vid_due", 25).await;

    let response = app
        .server
        .get("/api/v1/downloads/pending-deletion")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    let items = body.as_array().expect("array body");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["video"]["id"], video_id.to_string());
    assert_eq!(items[0]["effective_retention_days"], 30);
    assert!(items[0]["scheduled_deletion_at"].is_string());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn pending_deletion_within_days_filters_window(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    // retention 30d, downloaded 25d ago => deletion in ~5 days.
    let profile = ProfileBuilder::new(user.id)
        .retention_days(30)
        .build(&pool)
        .await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;
    seed_completed_video(&pool, source.id, "vid_window", 25).await;

    // Window too small: excluded.
    let response = app
        .server
        .get("/api/v1/downloads/pending-deletion?within_days=3")
        .add_header("Authorization", key.bearer())
        .await;
    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body.as_array().expect("array").len(), 0);

    // Window large enough: included.
    let response = app
        .server
        .get("/api/v1/downloads/pending-deletion?within_days=10")
        .add_header("Authorization", key.bearer())
        .await;
    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body.as_array().expect("array").len(), 1);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn pending_deletion_excludes_keep_forever(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    // No retention on profile or source, and the test AppState has no global
    // retention => keep forever => excluded.
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;
    seed_completed_video(&pool, source.id, "vid_forever", 25).await;

    let response = app
        .server
        .get("/api/v1/downloads/pending-deletion")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body.as_array().expect("array").len(), 0);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn pending_deletion_filters_by_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let profile_a = ProfileBuilder::new(user.id)
        .retention_days(30)
        .build(&pool)
        .await;
    let source_a = SourceBuilder::new(profile_a.id).build(&pool).await;
    let video_a = seed_completed_video(&pool, source_a.id, "vid_a", 25).await;

    let profile_b = ProfileBuilder::new(user.id)
        .retention_days(30)
        .build(&pool)
        .await;
    let source_b = SourceBuilder::new(profile_b.id).build(&pool).await;
    seed_completed_video(&pool, source_b.id, "vid_b", 25).await;

    let response = app
        .server
        .get(&format!(
            "/api/v1/downloads/pending-deletion?profile_id={}",
            profile_a.id
        ))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    let items = body.as_array().expect("array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["video"]["id"], video_a.to_string());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn pending_deletion_invalid_profile_id_returns_400(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/downloads/pending-deletion?profile_id=not-a-ulid")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);
}
