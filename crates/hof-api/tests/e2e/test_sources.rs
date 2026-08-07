//! Source endpoint CRUD tests.

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::helpers::{ApiKeyBuilder, ProfileBuilder, SourceBuilder, TestApp, UserBuilder};

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn list_sources_returns_array(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/sources")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn create_source_returns_201(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/sources")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "profile_id": profile.id.to_string(),
            "url": "https://youtube.com/@testchannel",
            "source_type": "Channel",
            "cutoff_date": "2024-01-01"
        }))
        .await;

    response.assert_status(StatusCode::CREATED);

    let body: serde_json::Value = response.json();
    assert_eq!(body["url"], "https://youtube.com/@testchannel");
    assert_eq!(body["source_type"], "Channel");
    assert!(body["id"].is_string());
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn create_source_playlist(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/sources")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "profile_id": profile.id.to_string(),
            "url": "https://youtube.com/playlist?list=PLxxxx",
            "source_type": "Playlist",
            "custom_name": "My Playlist",
            "cutoff_date": "2024-06-01",
            "index_frequency_secs": 7200,
            "retention_days": 60
        }))
        .await;

    response.assert_status(StatusCode::CREATED);

    let body: serde_json::Value = response.json();
    assert_eq!(body["source_type"], "Playlist");
    assert_eq!(body["custom_name"], "My Playlist");
    assert_eq!(body["index_frequency_secs"], 7200);
    assert_eq!(body["retention_days"], 60);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn create_source_invalid_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/sources")
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "profile_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "url": "https://youtube.com/@test",
            "source_type": "Channel",
            "cutoff_date": "2024-01-01"
        }))
        .await;

    // Foreign key constraint should fail
    response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_source_returns_source(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id)
        .custom_name("Test Source")
        .build(&pool)
        .await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert_eq!(body["id"], source.id.to_string());
    assert_eq!(body["custom_name"], "Test Source");
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn get_source_not_found(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let response = app
        .server
        .get("/api/v1/sources/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn update_source_partial(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .put(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({
            "custom_name": "Updated Name",
            "index_frequency_secs": 1800
        }))
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert_eq!(body["custom_name"], "Updated Name");
    assert_eq!(body["index_frequency_secs"], 1800);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn update_source_clear_custom_name(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id)
        .custom_name("Original Name")
        .build(&pool)
        .await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .put(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .json(&serde_json::json!({"custom_name": null}))
        .await;

    response.assert_status_ok();

    let body: serde_json::Value = response.json();
    assert!(body["custom_name"].is_null());
}

// Note: The `enabled` field is not part of UpdateSourceRequest.
// Sources cannot be disabled via the API (only via direct DB update).

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn delete_source_returns_204(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).full_access().build(&pool).await;

    let response = app
        .server
        .delete(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NO_CONTENT);

    // Verify it's gone
    let get_response = app
        .server
        .get(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .await;
    get_response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn trigger_index_returns_accepted_or_conflict(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post(&format!("/api/v1/sources/{}/index", source.id))
        .add_header("Authorization", key.bearer())
        .await;

    // 202 Accepted for async operation, or 409 Conflict if scheduler already started indexing
    let status = response.status_code();
    assert!(
        status == StatusCode::ACCEPTED || status == StatusCode::CONFLICT,
        "Expected 202 or 409, got {status}"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn trigger_index_source_not_found(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_write().build(&pool).await;

    let response = app
        .server
        .post("/api/v1/sources/01ARZ3NDEKTSV4RRFFQ69G5FAV/index")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn list_sources_filter_by_profile(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile1 = ProfileBuilder::new(user.id).build(&pool).await;
    let profile2 = ProfileBuilder::new(user.id).build(&pool).await;

    // Create sources for both profiles
    SourceBuilder::new(profile1.id).build(&pool).await;
    SourceBuilder::new(profile2.id).build(&pool).await;

    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    // Filter by profile1
    let response = app
        .server
        .get(&format!("/api/v1/sources?profile_id={}", profile1.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();

    let body: Vec<serde_json::Value> = response.json();
    for source in &body {
        assert_eq!(source["profile_id"], profile1.id.to_string());
    }
}

// ---------------------------------------------------------------------------
// Cleanup exclusion
// ---------------------------------------------------------------------------

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn source_defaults_to_included_in_cleanup(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    assert!(!source.exclude_from_cleanup);

    let response = app
        .server
        .get(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["exclude_from_cleanup"], false);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn excluded_source_is_reported_in_api_response(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id)
        .exclude_from_cleanup()
        .build(&pool)
        .await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    assert!(source.exclude_from_cleanup);

    let response = app
        .server
        .get(&format!("/api/v1/sources/{}", source.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["exclude_from_cleanup"], true);
}

/// The whole point of the flag: a long-expired video belonging to an excluded
/// source must never be offered up for retention cleanup.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn excluded_source_videos_are_never_past_retention(pool: PgPool) {
    use hof_core::db;

    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;

    let kept = SourceBuilder::new(profile.id)
        .retention_days(1)
        .exclude_from_cleanup()
        .build(&pool)
        .await;
    let collected = SourceBuilder::new(profile.id)
        .retention_days(1)
        .build(&pool)
        .await;

    let protected = seed_expired_video(&pool, kept.id, "kept_video").await;
    let expired = seed_expired_video(&pool, collected.id, "collected_video").await;

    let past = db::list_videos_past_retention(&pool, Some(1))
        .await
        .expect("list past retention");
    let ids: Vec<_> = past.iter().map(|v| v.id).collect();

    assert!(
        ids.contains(&expired),
        "non-excluded source's expired video should be collected"
    );
    assert!(
        !ids.contains(&protected),
        "excluded source's video must never be collected"
    );
}

/// A video shared between an excluded and a non-excluded source stays
/// protected: the exclusion is a veto, not a vote.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn exclusion_protects_videos_shared_with_other_sources(pool: PgPool) {
    use hof_core::db;

    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;

    let excluded = SourceBuilder::new(profile.id)
        .retention_days(1)
        .exclude_from_cleanup()
        .build(&pool)
        .await;
    let normal = SourceBuilder::new(profile.id)
        .retention_days(1)
        .build(&pool)
        .await;

    let video = seed_expired_video(&pool, normal.id, "shared_expired").await;
    db::link_video_to_source(&pool, excluded.id, video)
        .await
        .expect("link excluded source");

    let past = db::list_videos_past_retention(&pool, Some(1))
        .await
        .expect("list past retention");

    assert!(
        !past.iter().any(|v| v.id == video),
        "a video linked to any excluded source must be protected"
    );
}

/// Seed a completed video downloaded long enough ago to be past any retention.
async fn seed_expired_video(
    pool: &PgPool,
    source_id: ulid::Ulid,
    platform_video_id: &str,
) -> ulid::Ulid {
    use hof_core::db;

    let video = db::create_video(
        pool,
        db::CreateVideo {
            platform: "youtube",
            platform_video_id,
            title: "Expired Video",
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
            downloaded_at = NOW() - make_interval(days => 400),
            file_path = '/tmp/expired.mp4',
            file_size_bytes = 1000
        WHERE id = $1
        ",
    )
    .bind(video.id.to_string())
    .execute(pool)
    .await
    .expect("mark video expired");

    db::link_video_to_source(pool, source_id, video.id)
        .await
        .expect("link video to source");

    video.id
}
