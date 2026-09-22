//! Database helpers for e2e tests.

use sqlx::{PgPool, Row};
use ulid::Ulid;

/// Fetch a profile by ID and return key fields.
pub async fn fetch_profile_fields(
    pool: &PgPool,
    profile_id: Ulid,
) -> Result<(String, String, Option<i32>), sqlx::Error> {
    sqlx::query(r"SELECT name, quality::text, retention_days FROM profiles WHERE id = $1")
        .bind(profile_id.to_string())
        .fetch_one(pool)
        .await
        .map(|row| {
            (
                row.get::<String, _>("name"),
                row.get::<String, _>("quality"),
                row.get::<Option<i32>, _>("retention_days"),
            )
        })
}

/// Check if a profile exists.
pub async fn profile_exists(pool: &PgPool, profile_id: Ulid) -> Result<bool, sqlx::Error> {
    sqlx::query("SELECT 1 FROM profiles WHERE id = $1")
        .bind(profile_id.to_string())
        .fetch_optional(pool)
        .await
        .map(|opt| opt.is_some())
}

/// Fetch a source by ID and return key fields.
pub async fn fetch_source_fields(
    pool: &PgPool,
    source_id: Ulid,
) -> Result<(String, Option<String>, i64), sqlx::Error> {
    sqlx::query(r"SELECT url, custom_name, index_frequency_secs FROM sources WHERE id = $1")
        .bind(source_id.to_string())
        .fetch_one(pool)
        .await
        .map(|row| {
            (
                row.get::<String, _>("url"),
                row.get::<Option<String>, _>("custom_name"),
                row.get::<i64, _>("index_frequency_secs"),
            )
        })
}

/// Check if a source exists.
pub async fn source_exists(pool: &PgPool, source_id: Ulid) -> Result<bool, sqlx::Error> {
    sqlx::query("SELECT 1 FROM sources WHERE id = $1")
        .bind(source_id.to_string())
        .fetch_optional(pool)
        .await
        .map(|opt| opt.is_some())
}

/// Fetch video status by ID.
pub async fn fetch_video_status(pool: &PgPool, video_id: Ulid) -> Result<String, sqlx::Error> {
    sqlx::query("SELECT status::text FROM videos WHERE id = $1")
        .bind(video_id.to_string())
        .fetch_one(pool)
        .await
        .map(|row| row.get::<String, _>("status"))
}

/// Check if a video exists.
pub async fn video_exists(pool: &PgPool, video_id: Ulid) -> Result<bool, sqlx::Error> {
    sqlx::query("SELECT 1 FROM videos WHERE id = $1")
        .bind(video_id.to_string())
        .fetch_optional(pool)
        .await
        .map(|opt| opt.is_some())
}

/// Count how many videos are linked to a source.
pub async fn count_videos_for_source(pool: &PgPool, source_id: Ulid) -> Result<i64, sqlx::Error> {
    sqlx::query("SELECT COUNT(*) as cnt FROM source_videos WHERE source_id = $1")
        .bind(source_id.to_string())
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>("cnt"))
}
