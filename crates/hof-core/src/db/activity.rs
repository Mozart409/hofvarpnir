//! Activity event database operations.

use chrono::{DateTime, Utc};
use sqlx::postgres::PgPool;
use ulid::Ulid;

use super::DbError;
use crate::domain::activity::{
    ActivityEvent, ActivityEventRow, ActivityEventType, ActivitySeverity,
};

/// Data required to create a new activity event.
#[derive(Debug, Clone)]
pub struct CreateActivityEvent<'a> {
    pub event_type: ActivityEventType,
    pub severity: ActivitySeverity,
    pub message: &'a str,
    pub source_id: Option<Ulid>,
    pub video_id: Option<Ulid>,
    pub profile_id: Option<Ulid>,
}

/// Create a new activity event.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn create_activity_event(
    pool: &PgPool,
    data: CreateActivityEvent<'_>,
) -> Result<ActivityEvent, DbError> {
    let id = Ulid::new();
    let row = sqlx::query_as::<_, ActivityEventRow>(
        r"
        INSERT INTO activity_events (id, event_type, severity, message, source_id, video_id, profile_id)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, event_type, severity, message, source_id, video_id, profile_id, created_at
        ",
    )
    .bind(id.to_string())
    .bind(data.event_type)
    .bind(data.severity)
    .bind(data.message)
    .bind(data.source_id.map(|id| id.to_string()))
    .bind(data.video_id.map(|id| id.to_string()))
    .bind(data.profile_id.map(|id| id.to_string()))
    .fetch_one(pool)
    .await?;

    Ok(ActivityEvent::try_from(row)?)
}

/// List activity events in reverse-chronological order.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn list_activity_events(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    severity: Option<ActivitySeverity>,
    event_type: Option<ActivityEventType>,
    source_id: Option<Ulid>,
) -> Result<Vec<ActivityEvent>, DbError> {
    let rows = sqlx::query_as::<_, ActivityEventRow>(
        r"
        SELECT id, event_type, severity, message, source_id, video_id, profile_id, created_at
        FROM activity_events
        WHERE ($1::activity_severity IS NULL OR severity = $1)
          AND ($2::activity_event_type IS NULL OR event_type = $2)
          AND ($3::text IS NULL OR source_id = $3)
        ORDER BY created_at DESC
        LIMIT $4 OFFSET $5
        ",
    )
    .bind(severity)
    .bind(event_type)
    .bind(source_id.map(|id| id.to_string()))
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(ActivityEvent::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Count activity events (for pagination).
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn count_activity_events(
    pool: &PgPool,
    severity: Option<ActivitySeverity>,
    event_type: Option<ActivityEventType>,
    source_id: Option<Ulid>,
) -> Result<i64, DbError> {
    let row: (i64,) = sqlx::query_as(
        r"
        SELECT COUNT(*)
        FROM activity_events
        WHERE ($1::activity_severity IS NULL OR severity = $1)
          AND ($2::activity_event_type IS NULL OR event_type = $2)
          AND ($3::text IS NULL OR source_id = $3)
        ",
    )
    .bind(severity)
    .bind(event_type)
    .bind(source_id.map(|id| id.to_string()))
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Delete activity events older than a given timestamp.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn cleanup_old_activity_events(
    pool: &PgPool,
    before: DateTime<Utc>,
) -> Result<u64, DbError> {
    let result = sqlx::query("DELETE FROM activity_events WHERE created_at < $1")
        .bind(before)
        .execute(pool)
        .await?;

    Ok(result.rows_affected())
}

/// Fire-and-forget activity event logging.
///
/// Logs errors via tracing but never fails. Intended for use in actors
/// where we don't want event logging to block or disrupt the main flow.
pub async fn log_activity(
    pool: &PgPool,
    event_type: ActivityEventType,
    severity: ActivitySeverity,
    message: &str,
    source_id: Option<Ulid>,
    video_id: Option<Ulid>,
    profile_id: Option<Ulid>,
) {
    if let Err(e) = create_activity_event(
        pool,
        CreateActivityEvent {
            event_type,
            severity,
            message,
            source_id,
            video_id,
            profile_id,
        },
    )
    .await
    {
        tracing::warn!(error = %e, "Failed to log activity event");
    }
}
