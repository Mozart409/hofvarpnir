//! Video database operations.

use chrono::{DateTime, Utc};
use sqlx::postgres::PgPool;
use tracing::instrument;
use ulid::Ulid;

use super::DbError;
use crate::domain::video::{
    Video, VideoPendingDeletion, VideoPendingDeletionRow, VideoRow, VideoStatus,
};

/// Data required to create a new video.
#[derive(Debug, Clone)]
pub struct CreateVideo<'a> {
    pub platform: &'a str,
    pub platform_video_id: &'a str,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub duration_secs: Option<i64>,
    pub published_at: Option<DateTime<Utc>>,
    pub thumbnail_url: Option<&'a str>,
}

/// Data for updating an existing video.
#[derive(Debug, Clone, Default)]
pub struct UpdateVideo<'a> {
    pub title: Option<&'a str>,
    pub description: Option<Option<&'a str>>,
    pub duration_secs: Option<Option<i64>>,
    pub published_at: Option<Option<DateTime<Utc>>>,
    pub thumbnail_url: Option<Option<&'a str>>,
    pub status: Option<VideoStatus>,
    pub attempts: Option<i32>,
    pub next_retry: Option<Option<DateTime<Utc>>>,
    pub last_error: Option<Option<&'a str>>,
    pub file_path: Option<Option<&'a str>>,
    pub file_size_bytes: Option<Option<i64>>,
    pub downloaded_at: Option<Option<DateTime<Utc>>>,
}

/// Create a new video.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool, data), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn create_video(pool: &PgPool, data: CreateVideo<'_>) -> Result<Video, DbError> {
    let id = Ulid::r#gen();
    let row = sqlx::query_as::<_, VideoRow>(
        r"
        INSERT INTO videos (id, platform, platform_video_id, title, description,
                            duration_secs, published_at, thumbnail_url)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, platform, platform_video_id, title, description,
                  duration_secs, published_at, thumbnail_url, status, attempts,
                  next_retry, last_error, file_path, file_size_bytes,
                  downloaded_at, created_at, updated_at
        ",
    )
    .bind(id.to_string())
    .bind(data.platform)
    .bind(data.platform_video_id)
    .bind(data.title)
    .bind(data.description)
    .bind(data.duration_secs)
    .bind(data.published_at)
    .bind(data.thumbnail_url)
    .fetch_one(pool)
    .await?;

    Ok(Video::try_from(row)?)
}

/// Find or create a video by platform and platform video ID.
/// This is the primary upsert operation used during indexing.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool, data), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn upsert_video(pool: &PgPool, data: CreateVideo<'_>) -> Result<Video, DbError> {
    let id = Ulid::r#gen();
    let row = sqlx::query_as::<_, VideoRow>(
        r"
        INSERT INTO videos (id, platform, platform_video_id, title, description,
                            duration_secs, published_at, thumbnail_url)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (platform, platform_video_id) DO UPDATE
        SET title = EXCLUDED.title,
            description = COALESCE(EXCLUDED.description, videos.description),
            duration_secs = COALESCE(EXCLUDED.duration_secs, videos.duration_secs),
            published_at = COALESCE(EXCLUDED.published_at, videos.published_at),
            thumbnail_url = COALESCE(EXCLUDED.thumbnail_url, videos.thumbnail_url)
        RETURNING id, platform, platform_video_id, title, description,
                  duration_secs, published_at, thumbnail_url, status, attempts,
                  next_retry, last_error, file_path, file_size_bytes,
                  downloaded_at, created_at, updated_at
        ",
    )
    .bind(id.to_string())
    .bind(data.platform)
    .bind(data.platform_video_id)
    .bind(data.title)
    .bind(data.description)
    .bind(data.duration_secs)
    .bind(data.published_at)
    .bind(data.thumbnail_url)
    .fetch_one(pool)
    .await?;

    Ok(Video::try_from(row)?)
}

/// Get a video by ID.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the video doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn get_video(pool: &PgPool, id: Ulid) -> Result<Video, DbError> {
    let row = sqlx::query_as::<_, VideoRow>(
        r"
        SELECT id, platform, platform_video_id, title, description,
               duration_secs, published_at, thumbnail_url, status, attempts,
               next_retry, last_error, file_path, file_size_bytes,
               downloaded_at, created_at, updated_at
        FROM videos
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    Ok(Video::try_from(row)?)
}

/// Get a video by platform and platform video ID.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the video doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn get_video_by_platform_id(
    pool: &PgPool,
    platform: &str,
    platform_video_id: &str,
) -> Result<Video, DbError> {
    let row = sqlx::query_as::<_, VideoRow>(
        r"
        SELECT id, platform, platform_video_id, title, description,
               duration_secs, published_at, thumbnail_url, status, attempts,
               next_retry, last_error, file_path, file_size_bytes,
               downloaded_at, created_at, updated_at
        FROM videos
        WHERE platform = $1 AND platform_video_id = $2
        ",
    )
    .bind(platform)
    .bind(platform_video_id)
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    Ok(Video::try_from(row)?)
}

/// List all videos with optional status filter.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_videos(
    pool: &PgPool,
    status_filter: Option<VideoStatus>,
) -> Result<Vec<Video>, DbError> {
    let rows = match status_filter {
        Some(status) => {
            sqlx::query_as::<_, VideoRow>(
                r"
                SELECT id, platform, platform_video_id, title, description,
                       duration_secs, published_at, thumbnail_url, status, attempts,
                       next_retry, last_error, file_path, file_size_bytes,
                       downloaded_at, created_at, updated_at
                FROM videos
                WHERE status = $1
                ORDER BY created_at DESC
                ",
            )
            .bind(&status)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, VideoRow>(
                r"
                SELECT id, platform, platform_video_id, title, description,
                       duration_secs, published_at, thumbnail_url, status, attempts,
                       next_retry, last_error, file_path, file_size_bytes,
                       downloaded_at, created_at, updated_at
                FROM videos
                ORDER BY created_at DESC
                ",
            )
            .fetch_all(pool)
            .await?
        }
    };

    rows.into_iter()
        .map(Video::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// List videos with optional status/title filters and pagination.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_videos_paginated(
    pool: &PgPool,
    status_filter: Option<VideoStatus>,
    search: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<Video>, DbError> {
    let rows = sqlx::query_as::<_, VideoRow>(
        r"
        SELECT id, platform, platform_video_id, title, description,
               duration_secs, published_at, thumbnail_url, status, attempts,
               next_retry, last_error, file_path, file_size_bytes,
               downloaded_at, created_at, updated_at
        FROM videos
        WHERE ($1::video_status IS NULL OR status = $1)
          AND ($2::text IS NULL OR title ILIKE '%' || $2 || '%')
        ORDER BY created_at DESC
        LIMIT $3 OFFSET $4
        ",
    )
    .bind(status_filter)
    .bind(search)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(Video::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Count videos with optional status/title filters.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn count_videos(
    pool: &PgPool,
    status_filter: Option<VideoStatus>,
    search: Option<&str>,
) -> Result<i64, DbError> {
    let row: (i64,) = sqlx::query_as(
        r"
        SELECT COUNT(*)
        FROM videos
        WHERE ($1::video_status IS NULL OR status = $1)
          AND ($2::text IS NULL OR title ILIKE '%' || $2 || '%')
        ",
    )
    .bind(status_filter)
    .bind(search)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// List videos for a specific source (via join table).
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_videos_for_source(pool: &PgPool, source_id: Ulid) -> Result<Vec<Video>, DbError> {
    let rows = sqlx::query_as::<_, VideoRow>(
        r"
        SELECT v.id, v.platform, v.platform_video_id, v.title, v.description,
               v.duration_secs, v.published_at, v.thumbnail_url, v.status, v.attempts,
               v.next_retry, v.last_error, v.file_path, v.file_size_bytes,
               v.downloaded_at, v.created_at, v.updated_at
        FROM videos v
        INNER JOIN source_videos sv ON sv.video_id = v.id
        WHERE sv.source_id = $1
        ORDER BY v.created_at DESC
        ",
    )
    .bind(source_id.to_string())
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(Video::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Find videos that are ready to be downloaded (pending) or ready to retry.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_videos_ready_for_download(pool: &PgPool) -> Result<Vec<Video>, DbError> {
    let rows = sqlx::query_as::<_, VideoRow>(
        r"
        SELECT id, platform, platform_video_id, title, description,
               duration_secs, published_at, thumbnail_url, status, attempts,
               next_retry, last_error, file_path, file_size_bytes,
               downloaded_at, created_at, updated_at
        FROM videos
        WHERE status = 'pending'
           OR (status = 'failed' AND next_retry IS NOT NULL AND next_retry <= NOW())
        ORDER BY created_at ASC
        ",
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(Video::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Find videos past their retention period for cleanup.
/// Returns videos that are completed and all referencing sources have expired retention.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_videos_past_retention(
    pool: &PgPool,
    global_retention_days: Option<i32>,
) -> Result<Vec<Video>, DbError> {
    // Complex query: video is past retention when ALL sources referencing it
    // have an effective retention that has expired.
    // Effective retention: source.retention_days ?? profile.retention_days ?? global
    let rows = sqlx::query_as::<_, VideoRow>(
        r"
        SELECT v.id, v.platform, v.platform_video_id, v.title, v.description,
               v.duration_secs, v.published_at, v.thumbnail_url, v.status, v.attempts,
               v.next_retry, v.last_error, v.file_path, v.file_size_bytes,
               v.downloaded_at, v.created_at, v.updated_at
        FROM videos v
        WHERE v.status = 'completed'
          AND v.downloaded_at IS NOT NULL
          AND NOT EXISTS (
            SELECT 1 FROM source_videos sv
            INNER JOIN sources s ON s.id = sv.source_id
            INNER JOIN profiles p ON p.id = s.profile_id
            WHERE sv.video_id = v.id
              AND v.downloaded_at + make_interval(days => COALESCE(s.retention_days, p.retention_days, $1)) > NOW()
          )
        ORDER BY v.downloaded_at ASC
        ",
    )
    .bind(global_retention_days)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(Video::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// List completed videos that are scheduled for retention deletion, soonest first.
///
/// A video's scheduled deletion is the latest expiry across all referencing
/// sources, where per-source retention is
/// `COALESCE(source.retention_days, profile.retention_days, global)`. Videos
/// with any keep-forever (NULL effective retention) source are excluded.
///
/// * `within_days` — when set, only videos whose scheduled deletion falls within
///   the next N days are returned.
/// * `profile_id` — when set, only videos that have at least one source under
///   that profile are returned (the deletion time is still computed across all
///   sources).
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_videos_pending_deletion(
    pool: &PgPool,
    global_retention_days: Option<i32>,
    profile_id: Option<Ulid>,
    within_days: Option<i32>,
    limit: i64,
) -> Result<Vec<VideoPendingDeletion>, DbError> {
    let profile_id = profile_id.map(|id| id.to_string());
    let rows = sqlx::query_as::<_, VideoPendingDeletionRow>(
        r"
        SELECT v.id, v.platform, v.platform_video_id, v.title, v.description,
               v.duration_secs, v.published_at, v.thumbnail_url, v.status, v.attempts,
               v.next_retry, v.last_error, v.file_path, v.file_size_bytes,
               v.downloaded_at, v.created_at, v.updated_at,
               MAX(v.downloaded_at + make_interval(days => COALESCE(s.retention_days, p.retention_days, $1)))
                   AS scheduled_deletion_at,
               MAX(COALESCE(s.retention_days, p.retention_days, $1)) AS effective_retention_days
        FROM videos v
        JOIN source_videos sv ON sv.video_id = v.id
        JOIN sources s ON s.id = sv.source_id
        JOIN profiles p ON p.id = s.profile_id
        WHERE v.status = 'completed'
          AND v.downloaded_at IS NOT NULL
          AND ($2::text IS NULL OR EXISTS (
                SELECT 1 FROM source_videos sv2
                JOIN sources s2 ON s2.id = sv2.source_id
                WHERE sv2.video_id = v.id AND s2.profile_id = $2))
        GROUP BY v.id, v.platform, v.platform_video_id, v.title, v.description,
                 v.duration_secs, v.published_at, v.thumbnail_url, v.status, v.attempts,
                 v.next_retry, v.last_error, v.file_path, v.file_size_bytes,
                 v.downloaded_at, v.created_at, v.updated_at
        HAVING bool_and(COALESCE(s.retention_days, p.retention_days, $1) IS NOT NULL)
           AND ($3::int IS NULL OR
                MAX(v.downloaded_at + make_interval(days => COALESCE(s.retention_days, p.retention_days, $1)))
                    <= NOW() + make_interval(days => $3))
        ORDER BY scheduled_deletion_at ASC
        LIMIT $4
        ",
    )
    .bind(global_retention_days)
    .bind(profile_id)
    .bind(within_days)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(VideoPendingDeletion::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Update a video.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the video doesn't exist.
#[allow(clippy::too_many_lines)]
#[instrument(skip(pool, data), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn update_video(
    pool: &PgPool,
    id: Ulid,
    data: UpdateVideo<'_>,
) -> Result<Video, DbError> {
    let row = sqlx::query_as::<_, VideoRow>(
        r"
        UPDATE videos
        SET title = COALESCE($2, title),
            description = CASE WHEN $3 THEN $4 ELSE description END,
            duration_secs = CASE WHEN $5 THEN $6 ELSE duration_secs END,
            published_at = CASE WHEN $7 THEN $8 ELSE published_at END,
            thumbnail_url = CASE WHEN $9 THEN $10 ELSE thumbnail_url END,
            status = COALESCE($11, status),
            attempts = COALESCE($12, attempts),
            next_retry = CASE WHEN $13 THEN $14 ELSE next_retry END,
            last_error = CASE WHEN $15 THEN $16 ELSE last_error END,
            file_path = CASE WHEN $17 THEN $18 ELSE file_path END,
            file_size_bytes = CASE WHEN $19 THEN $20 ELSE file_size_bytes END,
            downloaded_at = CASE WHEN $21 THEN $22 ELSE downloaded_at END
        WHERE id = $1
        RETURNING id, platform, platform_video_id, title, description,
                  duration_secs, published_at, thumbnail_url, status, attempts,
                  next_retry, last_error, file_path, file_size_bytes,
                  downloaded_at, created_at, updated_at
        ",
    )
    .bind(id.to_string())
    .bind(data.title)
    .bind(data.description.is_some())
    .bind(data.description.flatten())
    .bind(data.duration_secs.is_some())
    .bind(data.duration_secs.flatten())
    .bind(data.published_at.is_some())
    .bind(data.published_at.flatten())
    .bind(data.thumbnail_url.is_some())
    .bind(data.thumbnail_url.flatten())
    .bind(data.status.as_ref())
    .bind(data.attempts)
    .bind(data.next_retry.is_some())
    .bind(data.next_retry.flatten())
    .bind(data.last_error.is_some())
    .bind(data.last_error.flatten())
    .bind(data.file_path.is_some())
    .bind(data.file_path.flatten())
    .bind(data.file_size_bytes.is_some())
    .bind(data.file_size_bytes.flatten())
    .bind(data.downloaded_at.is_some())
    .bind(data.downloaded_at.flatten())
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    Ok(Video::try_from(row)?)
}

/// Update video status (convenience function for common status transitions).
///
/// # Errors
///
/// Returns `DbError::NotFound` if the video doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn update_video_status(
    pool: &PgPool,
    id: Ulid,
    status: VideoStatus,
) -> Result<(), DbError> {
    let result = sqlx::query(
        r"
        UPDATE videos
        SET status = $2
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .bind(&status)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

/// Mark a video as downloading and increment attempts.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the video doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn mark_video_downloading(pool: &PgPool, id: Ulid) -> Result<(), DbError> {
    let result = sqlx::query(
        r"
        UPDATE videos
        SET status = 'downloading',
            attempts = attempts + 1,
            next_retry = NULL,
            last_error = NULL
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

/// Mark a video as completed with file details.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the video doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn mark_video_completed(
    pool: &PgPool,
    id: Ulid,
    file_path: &str,
    file_size_bytes: i64,
) -> Result<(), DbError> {
    let result = sqlx::query(
        r"
        UPDATE videos
        SET status = 'completed',
            file_path = $2,
            file_size_bytes = $3,
            downloaded_at = NOW(),
            next_retry = NULL,
            last_error = NULL
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .bind(file_path)
    .bind(file_size_bytes)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

/// Mark a video as failed with error and next retry time.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the video doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn mark_video_failed(
    pool: &PgPool,
    id: Ulid,
    error: &str,
    next_retry: Option<DateTime<Utc>>,
) -> Result<(), DbError> {
    let status = if next_retry.is_some() {
        VideoStatus::Failed
    } else {
        VideoStatus::PermanentlyFailed
    };

    let result = sqlx::query(
        r"
        UPDATE videos
        SET status = $2,
            last_error = $3,
            next_retry = $4
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .bind(&status)
    .bind(error)
    .bind(next_retry)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

/// Reset videos stuck in downloading status back to pending.
/// Used during crash recovery on startup.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn reset_stuck_downloads(pool: &PgPool) -> Result<u64, DbError> {
    let result = sqlx::query(
        r"
        UPDATE videos
        SET status = 'pending'
        WHERE status = 'downloading'
        ",
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Bulk-fetch source display names for a list of videos.
///
/// Returns a map of `video_id -> source_display_name` (picks the first source
/// per video using `COALESCE(custom_name, channel_title, url)`).
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn get_source_names_for_videos(
    pool: &PgPool,
) -> Result<std::collections::HashMap<Ulid, String>, DbError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        r"
        SELECT DISTINCT ON (sv.video_id)
               sv.video_id,
               COALESCE(s.custom_name, s.channel_title, s.url) AS source_name
        FROM source_videos sv
        INNER JOIN sources s ON s.id = sv.source_id
        ORDER BY sv.video_id, s.created_at ASC
        ",
    )
    .fetch_all(pool)
    .await?;

    let map = rows
        .into_iter()
        .filter_map(|(vid, name)| Ulid::from_string(&vid).ok().map(|id| (id, name)))
        .collect();

    Ok(map)
}

/// Delete a video.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the video doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn delete_video(pool: &PgPool, id: Ulid) -> Result<(), DbError> {
    let result = sqlx::query("DELETE FROM videos WHERE id = $1")
        .bind(id.to_string())
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}
