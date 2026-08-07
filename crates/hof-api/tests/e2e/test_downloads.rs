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

// ============================================================================
// Source/profile context enrichment
// ============================================================================

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn list_downloads_includes_source_and_profile_context(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id)
        .url("https://youtube.com/@my_channel")
        .custom_name("My Custom Name")
        .build(&pool)
        .await;
    let video_id = seed_completed_video(&pool, source.id, "vid_ctx", 1).await;

    let response = app
        .server
        .get("/api/v1/downloads")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    let items = body.as_array().expect("array body");
    let item = items
        .iter()
        .find(|v| v["id"] == video_id.to_string())
        .expect("seeded video present in list");

    // Additive fields: source/profile context.
    assert_eq!(item["source_id"], source.id.to_string());
    assert_eq!(item["source_url"], "https://youtube.com/@my_channel");
    assert_eq!(item["source_custom_name"], "My Custom Name");
    assert_eq!(item["source_display_name"], "My Custom Name");
    assert_eq!(item["profile_id"], profile.id.to_string());
    assert_eq!(item["profile_name"], profile.name);
    assert!(item["profile_quality"].is_string());
    assert!(item["profile_output_preset"].is_string());

    // Pre-existing fields are still present and unchanged in shape.
    // `VideoStatus` sets `rename_all` on its sqlx type only, not on serde, so
    // the JSON representation is the PascalCase variant name.
    assert_eq!(item["title"], "Test Video");
    assert_eq!(item["status"], "Completed");
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_download_matches_list_item_shape_for_linked_source(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id)
        .url("https://youtube.com/@another_channel")
        .custom_name("Another Custom Name")
        .build(&pool)
        .await;
    let video_id = seed_completed_video(&pool, source.id, "vid_detail_ctx", 1).await;

    let response = app
        .server
        .get(&format!("/api/v1/downloads/{video_id}"))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();

    assert_eq!(body["id"], video_id.to_string());
    assert_eq!(body["source_id"], source.id.to_string());
    assert_eq!(body["source_url"], "https://youtube.com/@another_channel");
    assert_eq!(body["source_custom_name"], "Another Custom Name");
    assert_eq!(body["source_display_name"], "Another Custom Name");
    assert_eq!(body["profile_id"], profile.id.to_string());
    assert_eq!(body["profile_name"], profile.name);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn download_source_display_name_falls_back_to_url_without_custom_name(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    // No `.custom_name(...)` set, and the builder doesn't set a channel_title,
    // so display_name should fall back to the source URL -- matching
    // `Source::display_name()`'s precedence used by the web UI.
    let source = SourceBuilder::new(profile.id)
        .url("https://youtube.com/@fallback_channel")
        .build(&pool)
        .await;
    let video_id = seed_completed_video(&pool, source.id, "vid_fallback", 1).await;

    let response = app
        .server
        .get(&format!("/api/v1/downloads/{video_id}"))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();

    assert!(body["source_custom_name"].is_null());
    assert_eq!(
        body["source_display_name"],
        "https://youtube.com/@fallback_channel"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_download_with_no_linked_source_has_null_context(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    // A video that has never been linked to any source (e.g. indexed but not
    // yet attached, or its source was since deleted).
    let video = db::create_video(
        &pool,
        db::CreateVideo {
            platform: "youtube",
            platform_video_id: "vid_orphan",
            title: "Orphan Video",
            description: None,
            duration_secs: None,
            published_at: None,
            thumbnail_url: None,
        },
    )
    .await
    .expect("create video");

    let response = app
        .server
        .get(&format!("/api/v1/downloads/{}", video.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();

    // Optional context fields are absent, not fabricated.
    assert!(body["source_id"].is_null());
    assert!(body["source_url"].is_null());
    assert!(body["source_custom_name"].is_null());
    assert!(body["source_display_name"].is_null());
    assert!(body["profile_id"].is_null());
    assert!(body["profile_name"].is_null());

    // Pre-existing fields are unaffected.
    assert_eq!(body["id"], video.id.to_string());
    assert_eq!(body["title"], "Orphan Video");
}

// ---------------------------------------------------------------------------
// Bulk actions on a selection
// ---------------------------------------------------------------------------

/// Seed a video in an arbitrary status, linked to `source_id`.
async fn seed_video_with_status(
    pool: &PgPool,
    source_id: Ulid,
    platform_video_id: &str,
    status: &str,
) -> Ulid {
    let video = db::create_video(
        pool,
        db::CreateVideo {
            platform: "youtube",
            platform_video_id,
            title: "Bulk Test Video",
            description: None,
            duration_secs: Some(100),
            published_at: None,
            thumbnail_url: None,
        },
    )
    .await
    .expect("create video");

    sqlx::query("UPDATE videos SET status = $2::video_status WHERE id = $1")
        .bind(video.id.to_string())
        .bind(status)
        .execute(pool)
        .await
        .expect("set video status");

    db::link_video_to_source(pool, source_id, video.id)
        .await
        .expect("link video to source");

    video.id
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn bulk_action_requires_write_scope(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/downloads/bulk")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "action": "retry",
            "video_ids": [Ulid::generate().to_string()],
        }))
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn bulk_action_rejects_empty_selection(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/downloads/bulk")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({ "action": "retry", "video_ids": [] }))
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);
}

/// A batch mixing eligible and ineligible videos must apply the action to the
/// eligible ones and report the rest, rather than failing wholesale.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn bulk_cancel_applies_to_eligible_and_reports_ineligible(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    let pending = seed_video_with_status(&pool, source.id, "bulk_pending", "pending").await;
    let completed = seed_video_with_status(&pool, source.id, "bulk_done", "completed").await;
    let ghost = Ulid::generate();

    let response = app
        .server
        .post("/api/v1/downloads/bulk")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "action": "cancel",
            "video_ids": [
                pending.to_string(),
                completed.to_string(),
                ghost.to_string(),
            ],
        }))
        .await;

    response.assert_status(StatusCode::ACCEPTED);
    let body: serde_json::Value = response.json();

    assert_eq!(body["succeeded"], serde_json::json!([pending.to_string()]));
    assert_eq!(
        body["ineligible"],
        serde_json::json!([completed.to_string()])
    );
    assert_eq!(body["not_found"], serde_json::json!([ghost.to_string()]));
}

/// Cancel must not touch a completed video, so its status is unchanged.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn bulk_action_leaves_ineligible_videos_untouched(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    let completed = seed_video_with_status(&pool, source.id, "bulk_keep", "completed").await;

    let response = app
        .server
        .post("/api/v1/downloads/bulk")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "action": "cancel",
            "video_ids": [completed.to_string()],
        }))
        .await;

    response.assert_status(StatusCode::ACCEPTED);

    let after = db::get_video(&pool, completed).await.expect("get video");
    assert_eq!(
        after.status,
        hof_core::domain::video::VideoStatus::Completed
    );
}

/// A malformed ULID is reported as not-found rather than 400-ing the batch.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn bulk_action_reports_malformed_ids_as_not_found(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/downloads/bulk")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "action": "retry",
            "video_ids": ["not-a-ulid"],
        }))
        .await;

    response.assert_status(StatusCode::ACCEPTED);
    let body: serde_json::Value = response.json();
    assert_eq!(body["not_found"], serde_json::json!(["not-a-ulid"]));
    assert_eq!(body["succeeded"], serde_json::json!([]));
}

// ---------------------------------------------------------------------------
// Search across source names
// ---------------------------------------------------------------------------

/// Downloads search matches the owning source's name, not just the title.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn search_matches_source_custom_name(pool: PgPool) {
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id)
        .custom_name("Astronomy Weekly")
        .build(&pool)
        .await;
    let other = SourceBuilder::new(profile.id)
        .custom_name("Cooking Hour")
        .build(&pool)
        .await;

    let wanted = seed_completed_video(&pool, source.id, "search_hit", 1).await;
    let _unwanted = seed_completed_video(&pool, other.id, "search_miss", 1).await;

    let found = db::list_videos_paginated(&pool, None, Some("astronomy"), 50, 0)
        .await
        .expect("search videos");

    assert_eq!(found.len(), 1, "expected only the astronomy source's video");
    assert_eq!(found[0].id, wanted);

    let count = db::count_videos(&pool, None, Some("astronomy"))
        .await
        .expect("count videos");
    assert_eq!(count, 1, "count must match the listed rows");
}

/// A video linked to several sources must not be returned more than once.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn search_does_not_duplicate_multi_source_videos(pool: PgPool) {
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let first = SourceBuilder::new(profile.id)
        .custom_name("Shared Alpha")
        .build(&pool)
        .await;
    let second = SourceBuilder::new(profile.id)
        .custom_name("Shared Beta")
        .build(&pool)
        .await;

    let video = seed_completed_video(&pool, first.id, "shared_video", 1).await;
    db::link_video_to_source(&pool, second.id, video)
        .await
        .expect("link second source");

    let found = db::list_videos_paginated(&pool, None, Some("Shared"), 50, 0)
        .await
        .expect("search videos");

    assert_eq!(found.len(), 1, "multi-source video must appear once");
}
