//! Database-level lifecycle of the quality gate's `awaiting_quality` hold.
//!
//! The download worker counts an attempt (`mark_video_downloading`) before it
//! knows the platform will only serve a low rendition, then parks the video
//! with `defer_video_for_quality`. These tests drive that loop against a real
//! database and assert on the persisted rows: the deferral must hand the
//! attempt back (otherwise a patient hold would burn through `MAX_ATTEMPTS`
//! and end `permanently_failed`), the retry sweep must honour the recheck
//! time, and leaving the hold must drop the recheck.

#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use chrono::{Duration, Utc};
use hof_core::db::{self, CreateProfile, CreateSource, CreateUser, CreateVideo};
use hof_core::domain::profile::{OutputPreset, Quality};
use hof_core::domain::source::SourceType;
use hof_core::domain::video::VideoStatus;
use sqlx::PgPool;
use ulid::Ulid;

async fn seed_source(pool: &PgPool, name: &str) -> Ulid {
    let user = db::create_user(
        pool,
        CreateUser {
            email: &format!("{name}@example.com"),
            name,
            password_hash: Some("$argon2id$v=19$m=16,t=2,p=1$dGVzdHNhbHQ$test"),
        },
    )
    .await
    .expect("create user");

    let profile = db::create_profile(
        pool,
        CreateProfile {
            user_id: user.id,
            name,
            quality: Quality::Best,
            output_preset: OutputPreset::Auto,
            naming_template: "{title}-{id}.{ext}",
            output_dir: "/tmp/hofvarpnir-quality-gate-test",
            include_livestreams: false,
            include_shorts: false,
            storage_quota_bytes: 1_000_000_000,
            retention_days: None,
            quality_wait_hours: Some(12),
        },
    )
    .await
    .expect("create profile");

    db::create_source(
        pool,
        CreateSource {
            profile_id: profile.id,
            url: &format!("https://youtube.com/@{name}"),
            source_type: SourceType::Channel,
            custom_name: Some(name),
            index_frequency_secs: 259_200,
            cutoff_date: chrono::NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date"),
            retention_days: None,
        },
    )
    .await
    .expect("create source")
    .id
}

/// Create a video linked to `source_id`, optionally with a delivered height.
async fn seed_video(pool: &PgPool, source_id: Ulid, pvid: &str, height: Option<i32>) -> Ulid {
    let video = db::create_video(
        pool,
        CreateVideo {
            platform: "youtube",
            platform_video_id: pvid,
            title: pvid,
            description: None,
            duration_secs: Some(100),
            published_at: None,
            thumbnail_url: None,
        },
    )
    .await
    .expect("create video");

    db::link_video_to_source(pool, source_id, video.id)
        .await
        .expect("link video");

    if let Some(h) = height {
        sqlx::query("UPDATE videos SET video_height = $2 WHERE id = $1")
            .bind(video.id.to_string())
            .bind(h)
            .execute(pool)
            .await
            .expect("set height");
    }

    video.id
}

async fn ready_ids(pool: &PgPool) -> Vec<Ulid> {
    db::list_videos_ready_for_download(pool)
        .await
        .expect("list ready")
        .into_iter()
        .map(|v| v.id)
        .collect()
}

/// Repeated download-attempt / defer cycles never consume the attempt budget,
/// the recheck time gates the retry sweep, and cancelling out of the hold
/// drops the recheck so the video is not auto-retried.
#[sqlx::test(migrations = "./migrations")]
async fn deferral_hands_back_attempts_and_gates_the_retry_sweep(pool: PgPool) {
    let source_id = seed_source(&pool, "gate-lifecycle").await;
    let video_id = seed_video(&pool, source_id, "held", None).await;

    let note = "Only 480p available (wanted 1080p); rechecking in 6h";

    for cycle in 1..=5 {
        db::mark_video_downloading(&pool, video_id)
            .await
            .expect("mark downloading");
        let mid = db::get_video(&pool, video_id).await.expect("get");
        assert_eq!(mid.status, VideoStatus::Downloading, "cycle {cycle}");
        assert_eq!(mid.attempts, 1, "cycle {cycle}: the attempt is counted");

        db::defer_video_for_quality(&pool, video_id, Utc::now() + Duration::hours(6), note)
            .await
            .expect("defer");

        let held = db::get_video(&pool, video_id).await.expect("get");
        assert_eq!(held.status, VideoStatus::AwaitingQuality, "cycle {cycle}");
        assert_eq!(held.attempts, 0, "cycle {cycle}: attempt handed back");
        assert_eq!(held.last_error.as_deref(), Some(note));
        assert!(held.next_retry.is_some_and(|t| t > Utc::now()));
    }

    // A future recheck is not eligible for the sweep.
    assert!(
        !ready_ids(&pool).await.contains(&video_id),
        "a hold whose recheck is in the future must not be picked up"
    );

    // Deferring a video that never had an attempt counted must not underflow.
    db::defer_video_for_quality(&pool, video_id, Utc::now() + Duration::hours(1), note)
        .await
        .expect("defer again");
    assert_eq!(
        db::get_video(&pool, video_id).await.expect("get").attempts,
        0
    );

    // Once the recheck time passes, the sweep picks it up.
    db::defer_video_for_quality(&pool, video_id, Utc::now() - Duration::minutes(1), note)
        .await
        .expect("defer into the past");
    assert!(
        ready_ids(&pool).await.contains(&video_id),
        "a hold whose recheck has passed must be retried"
    );

    // Cancelling out of the hold clears the recheck, so the sweep leaves it.
    db::update_video_status(&pool, video_id, VideoStatus::Failed)
        .await
        .expect("cancel to failed");
    let failed = db::get_video(&pool, video_id).await.expect("get");
    assert_eq!(failed.status, VideoStatus::Failed);
    assert!(
        failed.next_retry.is_none(),
        "leaving awaiting_quality must clear next_retry"
    );
    assert!(
        !ready_ids(&pool).await.contains(&video_id),
        "a cancelled hold must not be auto-retried"
    );
}

/// The best delivered height is the maximum over the source's own videos,
/// ignores NULL heights, and does not leak across sources.
#[sqlx::test(migrations = "./migrations")]
async fn best_delivered_height_is_scoped_to_the_source(pool: PgPool) {
    let source_a = seed_source(&pool, "height-a").await;
    let source_b = seed_source(&pool, "height-b").await;

    assert_eq!(
        db::source_best_delivered_height(&pool, source_a)
            .await
            .expect("query"),
        None,
        "a source with no videos has no delivered height"
    );

    seed_video(&pool, source_a, "a_unknown", None).await;
    assert_eq!(
        db::source_best_delivered_height(&pool, source_a)
            .await
            .expect("query"),
        None,
        "videos without a recorded height contribute nothing"
    );

    seed_video(&pool, source_a, "a_480", Some(480)).await;
    seed_video(&pool, source_a, "a_1080", Some(1080)).await;
    seed_video(&pool, source_a, "a_720", Some(720)).await;
    seed_video(&pool, source_a, "a_null", None).await;

    // A very tall video on another source must not count for `a`.
    seed_video(&pool, source_b, "b_2160", Some(2160)).await;

    assert_eq!(
        db::source_best_delivered_height(&pool, source_a)
            .await
            .expect("query"),
        Some(1080)
    );
    assert_eq!(
        db::source_best_delivered_height(&pool, source_b)
            .await
            .expect("query"),
        Some(2160)
    );

    // A video shared by both sources counts for both.
    let shared = seed_video(&pool, source_a, "shared_4320", Some(4320)).await;
    db::link_video_to_source(&pool, source_b, shared)
        .await
        .expect("link shared");
    assert_eq!(
        db::source_best_delivered_height(&pool, source_b)
            .await
            .expect("query"),
        Some(4320)
    );
}

/// "Download now" releases a held video to `pending` with the gate skipped and
/// the recheck dropped; a video that is not held is left alone and errors.
#[sqlx::test(migrations = "./migrations")]
async fn release_from_quality_gate_only_affects_held_videos(pool: PgPool) {
    let source_id = seed_source(&pool, "gate-release").await;
    let held_id = seed_video(&pool, source_id, "held_release", None).await;
    let done_id = seed_video(&pool, source_id, "done_release", None).await;

    assert!(
        !db::video_skips_quality_gate(&pool, held_id)
            .await
            .expect("read flag"),
        "the gate is on by default"
    );

    db::mark_video_downloading(&pool, held_id)
        .await
        .expect("mark downloading");
    db::defer_video_for_quality(
        &pool,
        held_id,
        Utc::now() + Duration::hours(6),
        "Only 480p available",
    )
    .await
    .expect("defer");

    let released = db::release_from_quality_gate(&pool, held_id)
        .await
        .expect("release");
    assert_eq!(released.status, VideoStatus::Pending);
    assert!(released.next_retry.is_none());
    assert!(released.last_error.is_none());
    assert!(
        db::video_skips_quality_gate(&pool, held_id)
            .await
            .expect("read flag")
    );

    // Releasing again is refused: it is no longer awaiting quality.
    assert!(db::release_from_quality_gate(&pool, held_id).await.is_err());

    // A completed video is untouched.
    db::update_video_status(&pool, done_id, VideoStatus::Completed)
        .await
        .expect("complete");
    assert!(db::release_from_quality_gate(&pool, done_id).await.is_err());
    let done = db::get_video(&pool, done_id).await.expect("get");
    assert_eq!(done.status, VideoStatus::Completed);
    assert!(
        !db::video_skips_quality_gate(&pool, done_id)
            .await
            .expect("read flag")
    );
}
