//! Source database operations.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::PgPool;
use tracing::instrument;
use ulid::Ulid;

use super::{DbError, MAX_INDEX_RETRIES};
use crate::domain::source::{EntryOrder, Source, SourceRow, SourceType};

/// Data required to create a new source.
#[derive(Debug, Clone)]
pub struct CreateSource<'a> {
    pub profile_id: Ulid,
    pub url: &'a str,
    pub source_type: SourceType,
    pub custom_name: Option<&'a str>,
    pub index_frequency_secs: i64,
    pub cutoff_date: NaiveDate,
    pub retention_days: Option<i32>,
}

/// Data for updating an existing source.
#[derive(Debug, Clone, Default)]
pub struct UpdateSource<'a> {
    pub url: Option<&'a str>,
    pub source_type: Option<SourceType>,
    pub custom_name: Option<Option<&'a str>>,
    pub index_frequency_secs: Option<i64>,
    pub cutoff_date: Option<NaiveDate>,
    pub retention_days: Option<Option<i32>>,
}

/// Data for updating channel metadata on a source.
#[derive(Debug, Clone, Default)]
pub struct UpdateChannelMetadata<'a> {
    pub channel_id: Option<&'a str>,
    pub channel_title: Option<&'a str>,
    pub channel_description: Option<&'a str>,
    pub channel_thumbnail_url: Option<&'a str>,
}

/// Create a new source.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool, data), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn create_source(pool: &PgPool, data: CreateSource<'_>) -> Result<Source, DbError> {
    let id = Ulid::generate();
    let id_str = id.to_string();
    let profile_id_str = data.profile_id.to_string();
    let row = sqlx::query_as!(
        SourceRow,
        r#"
        INSERT INTO sources (id, profile_id, url, source_type, custom_name,
                             index_frequency_secs, cutoff_date, retention_days)
        VALUES ($1, $2, $3, $4::source_type, $5, $6, $7, $8)
        RETURNING
            id, profile_id, url,
            source_type AS "source_type: SourceType",
            custom_name, enabled, exclude_from_cleanup, index_frequency_secs,
            cutoff_date AS "cutoff_date: NaiveDate",
            retention_days,
            entry_order AS "entry_order: EntryOrder",
            entry_order_detected_at AS "entry_order_detected_at: DateTime<Utc>",
            last_indexed_at AS "last_indexed_at: DateTime<Utc>",
            last_error, index_error_count,
            created_at AS "created_at: DateTime<Utc>",
            updated_at AS "updated_at: DateTime<Utc>",
            channel_id, channel_title, channel_description, channel_thumbnail_url,
            jellyfin_metadata_at AS "jellyfin_metadata_at: DateTime<Utc>"
        "#,
        id_str,
        profile_id_str,
        data.url,
        data.source_type as _,
        data.custom_name,
        data.index_frequency_secs,
        data.cutoff_date as _,
        data.retention_days,
    )
    .fetch_one(pool)
    .await?;

    Ok(Source::try_from(row)?)
}

/// Get a source by ID.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn get_source(pool: &PgPool, id: Ulid) -> Result<Source, DbError> {
    let row = sqlx::query_as!(
        SourceRow,
        r#"
        SELECT
            id, profile_id, url,
            source_type AS "source_type: SourceType",
            custom_name, enabled, exclude_from_cleanup, index_frequency_secs,
            cutoff_date AS "cutoff_date: NaiveDate",
            retention_days,
            entry_order AS "entry_order: EntryOrder",
            entry_order_detected_at AS "entry_order_detected_at: DateTime<Utc>",
            last_indexed_at AS "last_indexed_at: DateTime<Utc>",
            last_error, index_error_count,
            created_at AS "created_at: DateTime<Utc>",
            updated_at AS "updated_at: DateTime<Utc>",
            channel_id, channel_title, channel_description, channel_thumbnail_url,
            jellyfin_metadata_at AS "jellyfin_metadata_at: DateTime<Utc>"
        FROM sources
        WHERE id = $1
        "#,
        id.to_string()
    )
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    Ok(Source::try_from(row)?)
}

/// List all sources for a profile.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_sources_for_profile(
    pool: &PgPool,
    profile_id: Ulid,
) -> Result<Vec<Source>, DbError> {
    let rows = sqlx::query_as!(
        SourceRow,
        r#"
        SELECT
            id, profile_id, url,
            source_type AS "source_type: SourceType",
            custom_name, enabled, exclude_from_cleanup, index_frequency_secs,
            cutoff_date AS "cutoff_date: NaiveDate",
            retention_days,
            entry_order AS "entry_order: EntryOrder",
            entry_order_detected_at AS "entry_order_detected_at: DateTime<Utc>",
            last_indexed_at AS "last_indexed_at: DateTime<Utc>",
            last_error, index_error_count,
            created_at AS "created_at: DateTime<Utc>",
            updated_at AS "updated_at: DateTime<Utc>",
            channel_id, channel_title, channel_description, channel_thumbnail_url,
            jellyfin_metadata_at AS "jellyfin_metadata_at: DateTime<Utc>"
        FROM sources
        WHERE profile_id = $1
        ORDER BY created_at DESC
        "#,
        profile_id.to_string()
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(Source::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// List all sources.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_sources(pool: &PgPool) -> Result<Vec<Source>, DbError> {
    let rows = sqlx::query_as!(
        SourceRow,
        r#"
        SELECT
            id, profile_id, url,
            source_type AS "source_type: SourceType",
            custom_name, enabled, exclude_from_cleanup, index_frequency_secs,
            cutoff_date AS "cutoff_date: NaiveDate",
            retention_days,
            entry_order AS "entry_order: EntryOrder",
            entry_order_detected_at AS "entry_order_detected_at: DateTime<Utc>",
            last_indexed_at AS "last_indexed_at: DateTime<Utc>",
            last_error, index_error_count,
            created_at AS "created_at: DateTime<Utc>",
            updated_at AS "updated_at: DateTime<Utc>",
            channel_id, channel_title, channel_description, channel_thumbnail_url,
            jellyfin_metadata_at AS "jellyfin_metadata_at: DateTime<Utc>"
        FROM sources
        ORDER BY created_at DESC
        "#
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(Source::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Find sources that are due for indexing.
///
/// A source is due for indexing if:
/// - `enabled` is true, AND
/// - `last_indexed_at` is NULL, OR
/// - `last_indexed_at + index_frequency_secs` < NOW
///
/// Sources with `index_error_count >= MAX_INDEX_RETRIES` are excluded from
/// automatic indexing. They can still be manually indexed via "Force index".
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_sources_due_for_indexing(pool: &PgPool) -> Result<Vec<Source>, DbError> {
    let rows = sqlx::query_as!(
        SourceRow,
        r#"
        SELECT
            id, profile_id, url,
            source_type AS "source_type: SourceType",
            custom_name, enabled, exclude_from_cleanup, index_frequency_secs,
            cutoff_date AS "cutoff_date: NaiveDate",
            retention_days,
            entry_order AS "entry_order: EntryOrder",
            entry_order_detected_at AS "entry_order_detected_at: DateTime<Utc>",
            last_indexed_at AS "last_indexed_at: DateTime<Utc>",
            last_error, index_error_count,
            created_at AS "created_at: DateTime<Utc>",
            updated_at AS "updated_at: DateTime<Utc>",
            channel_id, channel_title, channel_description, channel_thumbnail_url,
            jellyfin_metadata_at AS "jellyfin_metadata_at: DateTime<Utc>"
        FROM sources
        WHERE enabled = true
          AND index_error_count < $1
          AND (last_indexed_at IS NULL
               OR last_indexed_at + make_interval(secs => index_frequency_secs) < NOW())
        ORDER BY last_indexed_at NULLS FIRST
        "#,
        MAX_INDEX_RETRIES
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(Source::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Update a source.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
#[instrument(skip(pool, data), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn update_source(
    pool: &PgPool,
    id: Ulid,
    data: UpdateSource<'_>,
) -> Result<Source, DbError> {
    let row = sqlx::query_as!(
        SourceRow,
        r#"
        UPDATE sources
        SET url = COALESCE($2, url),
            source_type = COALESCE($3::source_type, source_type),
            custom_name = CASE WHEN $4 THEN $5 ELSE custom_name END,
            index_frequency_secs = COALESCE($6, index_frequency_secs),
            cutoff_date = COALESCE($7, cutoff_date),
            retention_days = CASE WHEN $8 THEN $9 ELSE retention_days END
        WHERE id = $1
        RETURNING
            id, profile_id, url,
            source_type AS "source_type: SourceType",
            custom_name, enabled, exclude_from_cleanup, index_frequency_secs,
            cutoff_date AS "cutoff_date: NaiveDate",
            retention_days,
            entry_order AS "entry_order: EntryOrder",
            entry_order_detected_at AS "entry_order_detected_at: DateTime<Utc>",
            last_indexed_at AS "last_indexed_at: DateTime<Utc>",
            last_error, index_error_count,
            created_at AS "created_at: DateTime<Utc>",
            updated_at AS "updated_at: DateTime<Utc>",
            channel_id, channel_title, channel_description, channel_thumbnail_url,
            jellyfin_metadata_at AS "jellyfin_metadata_at: DateTime<Utc>"
        "#,
        id.to_string(),
        data.url,
        data.source_type.as_ref() as _,
        data.custom_name.is_some(),
        data.custom_name.flatten(),
        data.index_frequency_secs,
        data.cutoff_date as _,
        data.retention_days.is_some(),
        data.retention_days.flatten(),
    )
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    Ok(Source::try_from(row)?)
}

/// Set the enabled state for a source.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn set_source_enabled(pool: &PgPool, id: Ulid, enabled: bool) -> Result<(), DbError> {
    let result = sqlx::query(
        r"
        UPDATE sources
        SET enabled = $2
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .bind(enabled)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

/// Set whether a source's videos are exempt from automatic cleanup.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn set_source_exclude_from_cleanup(
    pool: &PgPool,
    id: Ulid,
    exclude: bool,
) -> Result<(), DbError> {
    let result = sqlx::query(
        r"
        UPDATE sources
        SET exclude_from_cleanup = $2
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .bind(exclude)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

/// Update the last indexed timestamp for a source and clear any error state.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn update_source_last_indexed(
    pool: &PgPool,
    id: Ulid,
    last_indexed_at: DateTime<Utc>,
) -> Result<(), DbError> {
    let result = sqlx::query(
        r"
        UPDATE sources
        SET last_indexed_at = $2,
            last_error = NULL,
            index_error_count = 0
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .bind(last_indexed_at)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

/// Record an indexing error for a source.
///
/// Increments the error count and stores the error message.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn record_source_indexing_error(
    pool: &PgPool,
    id: Ulid,
    error: &str,
) -> Result<(), DbError> {
    let result = sqlx::query(
        r"
        UPDATE sources
        SET last_error = $2,
            index_error_count = index_error_count + 1
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .bind(error)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

/// Reset the indexing error count for a source.
///
/// Called when a user manually triggers "Force index" to give the source
/// fresh retry attempts.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn reset_source_indexing_errors(pool: &PgPool, id: Ulid) -> Result<(), DbError> {
    let result = sqlx::query(
        r"
        UPDATE sources
        SET last_error = NULL,
            index_error_count = 0
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

/// Update channel metadata for a source (from indexing results).
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
#[instrument(skip(pool, data), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn update_source_channel_metadata(
    pool: &PgPool,
    id: Ulid,
    data: UpdateChannelMetadata<'_>,
) -> Result<(), DbError> {
    let result = sqlx::query(
        r"
        UPDATE sources
        SET channel_id = COALESCE($2, channel_id),
            channel_title = COALESCE($3, channel_title),
            channel_description = COALESCE($4, channel_description),
            channel_thumbnail_url = COALESCE($5, channel_thumbnail_url)
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .bind(data.channel_id)
    .bind(data.channel_title)
    .bind(data.channel_description)
    .bind(data.channel_thumbnail_url)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

/// Update the Jellyfin metadata generation timestamp.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn update_source_jellyfin_metadata_at(
    pool: &PgPool,
    id: Ulid,
    timestamp: DateTime<Utc>,
) -> Result<(), DbError> {
    let result = sqlx::query(
        r"
        UPDATE sources
        SET jellyfin_metadata_at = $2
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .bind(timestamp)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

/// List sources that need Jellyfin metadata generation.
///
/// A source needs metadata generation if:
/// - `jellyfin_metadata_at` is NULL (never generated), OR
/// - Metadata files are missing (checked externally)
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_sources_needing_jellyfin_metadata(pool: &PgPool) -> Result<Vec<Source>, DbError> {
    let rows = sqlx::query_as!(
        SourceRow,
        r#"
        SELECT
            id, profile_id, url,
            source_type AS "source_type: SourceType",
            custom_name, enabled, exclude_from_cleanup, index_frequency_secs,
            cutoff_date AS "cutoff_date: NaiveDate",
            retention_days,
            entry_order AS "entry_order: EntryOrder",
            entry_order_detected_at AS "entry_order_detected_at: DateTime<Utc>",
            last_indexed_at AS "last_indexed_at: DateTime<Utc>",
            last_error, index_error_count,
            created_at AS "created_at: DateTime<Utc>",
            updated_at AS "updated_at: DateTime<Utc>",
            channel_id, channel_title, channel_description, channel_thumbnail_url,
            jellyfin_metadata_at AS "jellyfin_metadata_at: DateTime<Utc>"
        FROM sources
        WHERE jellyfin_metadata_at IS NULL
        ORDER BY created_at ASC
        "#
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(Source::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Update the detected entry order for a source.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn update_source_entry_order(
    pool: &PgPool,
    id: Ulid,
    entry_order: EntryOrder,
) -> Result<(), DbError> {
    // Set detected_at timestamp when order is detected (not Unknown)
    // Clear it when resetting to Unknown
    let result = sqlx::query(
        r"
        UPDATE sources
        SET entry_order = $2,
            entry_order_detected_at = CASE
                WHEN $2 = 'unknown' THEN NULL
                ELSE NOW()
            END
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .bind(entry_order)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

/// Delete a source.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn delete_source(pool: &PgPool, id: Ulid) -> Result<(), DbError> {
    let result = sqlx::query("DELETE FROM sources WHERE id = $1")
        .bind(id.to_string())
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

// ============================================================================
// Source-Video Link Operations (Join Table)
// ============================================================================

/// Link a video to a source.
///
/// # Errors
///
/// Returns an error if the database operation fails (e.g., duplicate link).
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn link_video_to_source(
    pool: &PgPool,
    source_id: Ulid,
    video_id: Ulid,
) -> Result<(), DbError> {
    sqlx::query(
        r"
        INSERT INTO source_videos (source_id, video_id)
        VALUES ($1, $2)
        ON CONFLICT (source_id, video_id) DO NOTHING
        ",
    )
    .bind(source_id.to_string())
    .bind(video_id.to_string())
    .execute(pool)
    .await?;

    Ok(())
}

/// Unlink a video from a source.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn unlink_video_from_source(
    pool: &PgPool,
    source_id: Ulid,
    video_id: Ulid,
) -> Result<bool, DbError> {
    let result = sqlx::query(
        r"
        DELETE FROM source_videos
        WHERE source_id = $1 AND video_id = $2
        ",
    )
    .bind(source_id.to_string())
    .bind(video_id.to_string())
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Get all source IDs linked to a video.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn get_sources_for_video(pool: &PgPool, video_id: Ulid) -> Result<Vec<Ulid>, DbError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        r"
        SELECT source_id
        FROM source_videos
        WHERE video_id = $1
        ",
    )
    .bind(video_id.to_string())
    .fetch_all(pool)
    .await?;

    let ulids = rows
        .into_iter()
        .filter_map(|(id,)| Ulid::from_string(&id).ok())
        .collect();

    Ok(ulids)
}

/// Get all video IDs linked to a source.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn get_video_ids_for_source(
    pool: &PgPool,
    source_id: Ulid,
) -> Result<Vec<Ulid>, DbError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        r"
        SELECT video_id
        FROM source_videos
        WHERE source_id = $1
        ",
    )
    .bind(source_id.to_string())
    .fetch_all(pool)
    .await?;

    let ulids = rows
        .into_iter()
        .filter_map(|(id,)| Ulid::from_string(&id).ok())
        .collect();

    Ok(ulids)
}
