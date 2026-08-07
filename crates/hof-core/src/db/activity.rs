//! Activity event database operations.

use chrono::{DateTime, Utc};
use sqlx::postgres::PgPool;
use tokio::sync::broadcast;
use tracing::instrument;
use ulid::Ulid;

use super::DbError;
use crate::domain::activity::{
    ActivityEvent, ActivityEventRow, ActivityEventType, ActivitySeverity, UnhealthySource,
    UnhealthySourceRow,
};

/// Broadcaster for real-time SSE notifications.
///
/// Holds channels for broadcasting activity events and generic invalidation
/// signals. Clone cheaply — senders share the underlying channel.
#[derive(Clone, Debug)]
pub struct ActivityBroadcaster {
    /// Signals that a new activity event was logged.
    pub activity_tx: broadcast::Sender<()>,
    /// Signals that any state has changed (profiles, sources, downloads).
    pub invalidate_tx: broadcast::Sender<()>,
}

impl ActivityBroadcaster {
    /// Create a new broadcaster with dedicated channels.
    #[must_use]
    pub fn new() -> Self {
        let (activity_tx, _) = broadcast::channel(256);
        let (invalidate_tx, _) = broadcast::channel(256);
        Self {
            activity_tx,
            invalidate_tx,
        }
    }

    /// Subscribe to activity events.
    #[must_use]
    pub fn subscribe_activity(&self) -> broadcast::Receiver<()> {
        self.activity_tx.subscribe()
    }

    /// Subscribe to invalidation signals.
    #[must_use]
    pub fn subscribe_invalidate(&self) -> broadcast::Receiver<()> {
        self.invalidate_tx.subscribe()
    }

    /// Send an invalidation signal (ignores errors when there are no subscribers).
    pub fn invalidate(&self) {
        let _ = self.invalidate_tx.send(());
    }

    /// Log an activity event to the database and broadcast signals on both channels.
    #[allow(clippy::too_many_arguments)]
    pub async fn log_and_broadcast(
        &self,
        pool: &PgPool,
        event_type: ActivityEventType,
        severity: ActivitySeverity,
        message: &str,
        source_id: Option<Ulid>,
        video_id: Option<Ulid>,
        profile_id: Option<Ulid>,
    ) {
        log_activity(
            pool, event_type, severity, message, source_id, video_id, profile_id,
        )
        .await;
        let _ = self.activity_tx.send(());
        let _ = self.invalidate_tx.send(());
    }
}

impl Default for ActivityBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

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
#[instrument(skip(pool, data), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn create_activity_event(
    pool: &PgPool,
    data: CreateActivityEvent<'_>,
) -> Result<ActivityEvent, DbError> {
    let id = Ulid::generate();
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
/// `search` matches the event message case-insensitively.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_activity_events(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    severity: Option<ActivitySeverity>,
    event_type: Option<ActivityEventType>,
    source_id: Option<Ulid>,
    search: Option<&str>,
) -> Result<Vec<ActivityEvent>, DbError> {
    let rows = sqlx::query_as::<_, ActivityEventRow>(
        r"
        SELECT id, event_type, severity, message, source_id, video_id, profile_id, created_at
        FROM activity_events
        WHERE ($1::activity_severity IS NULL OR severity = $1)
          AND ($2::activity_event_type IS NULL OR event_type = $2)
          AND ($3::text IS NULL OR source_id = $3)
          AND ($4::text IS NULL OR message ILIKE '%' || $4 || '%')
        ORDER BY created_at DESC
        LIMIT $5 OFFSET $6
        ",
    )
    .bind(severity)
    .bind(event_type)
    .bind(source_id.map(|id| id.to_string()))
    .bind(search)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(ActivityEvent::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// List sources that are currently failing to index.
///
/// Returns sources whose most recent run of consecutive `SourceError` events
/// (since their last `SourceIndexed` success, or since the start of history if
/// they never succeeded) is at least `min_consecutive_errors`. Ordered by the
/// longest failing streak first.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_unhealthy_sources(
    pool: &PgPool,
    min_consecutive_errors: i64,
) -> Result<Vec<UnhealthySource>, DbError> {
    let rows = sqlx::query_as::<_, UnhealthySourceRow>(
        r"
        WITH last_success AS (
            SELECT source_id, MAX(created_at) AS last_success_at
            FROM activity_events
            WHERE event_type = 'source_indexed'::activity_event_type
              AND source_id IS NOT NULL
            GROUP BY source_id
        ),
        recent_errors AS (
            SELECT ae.source_id AS source_id,
                   COUNT(*) AS consecutive_errors,
                   MIN(ae.created_at) AS first_error_at,
                   MAX(ae.created_at) AS last_error_at
            FROM activity_events ae
            LEFT JOIN last_success ls ON ls.source_id = ae.source_id
            WHERE ae.event_type = 'source_error'::activity_event_type
              AND ae.source_id IS NOT NULL
              AND (ls.last_success_at IS NULL OR ae.created_at > ls.last_success_at)
            GROUP BY ae.source_id
            HAVING COUNT(*) >= $1
        )
        SELECT re.source_id AS source_id,
               s.custom_name AS custom_name,
               s.url AS url,
               s.enabled AS enabled,
               re.consecutive_errors AS consecutive_errors,
               re.first_error_at AS first_error_at,
               re.last_error_at AS last_error_at,
               (SELECT ae2.message
                  FROM activity_events ae2
                  WHERE ae2.source_id = re.source_id
                    AND ae2.event_type = 'source_error'::activity_event_type
                  ORDER BY ae2.created_at DESC
                  LIMIT 1) AS last_error_message
        FROM recent_errors re
        JOIN sources s ON s.id = re.source_id
        ORDER BY re.consecutive_errors DESC, re.last_error_at DESC
        ",
    )
    .bind(min_consecutive_errors)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(UnhealthySource::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Count activity events (for pagination).
///
/// Must apply exactly the same predicate as [`list_activity_events`], or the
/// pagination controls will disagree with the rows actually returned.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn count_activity_events(
    pool: &PgPool,
    severity: Option<ActivitySeverity>,
    event_type: Option<ActivityEventType>,
    source_id: Option<Ulid>,
    search: Option<&str>,
) -> Result<i64, DbError> {
    let row: (i64,) = sqlx::query_as(
        r"
        SELECT COUNT(*)
        FROM activity_events
        WHERE ($1::activity_severity IS NULL OR severity = $1)
          AND ($2::activity_event_type IS NULL OR event_type = $2)
          AND ($3::text IS NULL OR source_id = $3)
          AND ($4::text IS NULL OR message ILIKE '%' || $4 || '%')
        ",
    )
    .bind(severity)
    .bind(event_type)
    .bind(source_id.map(|id| id.to_string()))
    .bind(search)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Delete activity events older than a given timestamp.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
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
#[instrument(skip(pool, message), fields(otel.kind = "client", db.system = "postgresql"))]
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
