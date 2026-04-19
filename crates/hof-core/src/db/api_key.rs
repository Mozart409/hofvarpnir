//! API key database operations.

use chrono::{DateTime, Utc};
use sqlx::postgres::PgPool;
use tracing::instrument;
use ulid::Ulid;

use super::DbError;
use crate::domain::api_key::{
    ApiKey, ApiKeyEvent, ApiKeyEventRow, ApiKeyEventType, ApiKeyRow, ApiKeyScope,
};

/// Data required to create a new API key.
#[derive(Debug, Clone)]
pub struct CreateApiKey<'a> {
    pub user_id: Ulid,
    pub name: &'a str,
    pub prefix: &'a str,
    pub key_hash: &'a str,
    pub scopes: &'a [ApiKeyScope],
    pub expires_at: Option<DateTime<Utc>>,
}

/// Create a new API key and log a "created" event.
///
/// # Errors
///
/// Returns an error if the database operation fails (e.g., duplicate name for user).
#[instrument(skip(pool, data), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn create_api_key(pool: &PgPool, data: CreateApiKey<'_>) -> Result<ApiKey, DbError> {
    let id = Ulid::new();
    let row = sqlx::query_as::<_, ApiKeyRow>(
        r"
        INSERT INTO api_keys (id, user_id, name, prefix, key_hash, scopes, expires_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, user_id, name, prefix, key_hash, scopes, expires_at, last_used_at, last_used_ip, created_at, updated_at
        ",
    )
    .bind(id.to_string())
    .bind(data.user_id.to_string())
    .bind(data.name)
    .bind(data.prefix)
    .bind(data.key_hash)
    .bind(data.scopes)
    .bind(data.expires_at)
    .fetch_one(pool)
    .await?;

    let api_key = ApiKey::try_from(row)?;

    // Log the created event
    log_api_key_event(pool, id, data.user_id, ApiKeyEventType::Created, None).await;

    Ok(api_key)
}

/// List all API keys for a user.
///
/// Returns keys in reverse-chronological order (newest first).
/// Never returns the key hash.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_api_keys(pool: &PgPool, user_id: Ulid) -> Result<Vec<ApiKey>, DbError> {
    let rows = sqlx::query_as::<_, ApiKeyRow>(
        r"
        SELECT id, user_id, name, prefix, key_hash, scopes, expires_at, last_used_at, last_used_ip, created_at, updated_at
        FROM api_keys
        WHERE user_id = $1
        ORDER BY created_at DESC
        ",
    )
    .bind(user_id.to_string())
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(ApiKey::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Look up an API key by its hash.
///
/// Used by the auth middleware to validate incoming Bearer tokens.
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool, key_hash), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn get_api_key_by_hash(pool: &PgPool, key_hash: &str) -> Result<Option<ApiKey>, DbError> {
    let row = sqlx::query_as::<_, ApiKeyRow>(
        r"
        SELECT id, user_id, name, prefix, key_hash, scopes, expires_at, last_used_at, last_used_ip, created_at, updated_at
        FROM api_keys
        WHERE key_hash = $1
        ",
    )
    .bind(key_hash)
    .fetch_optional(pool)
    .await?;

    row.map(ApiKey::try_from).transpose().map_err(DbError::from)
}

/// Get an API key by ID (for user ownership verification).
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn get_api_key(pool: &PgPool, key_id: Ulid) -> Result<Option<ApiKey>, DbError> {
    let row = sqlx::query_as::<_, ApiKeyRow>(
        r"
        SELECT id, user_id, name, prefix, key_hash, scopes, expires_at, last_used_at, last_used_ip, created_at, updated_at
        FROM api_keys
        WHERE id = $1
        ",
    )
    .bind(key_id.to_string())
    .fetch_optional(pool)
    .await?;

    row.map(ApiKey::try_from).transpose().map_err(DbError::from)
}

/// Update the last-used timestamp and IP for an API key.
///
/// This is a best-effort operation - failures are logged but don't propagate.
/// Intended to be spawned as a background task.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn touch_api_key_last_used(pool: &PgPool, key_id: Ulid, ip: Option<&str>) {
    let result = sqlx::query(
        r"
        UPDATE api_keys
        SET last_used_at = now(), last_used_ip = $2
        WHERE id = $1
        ",
    )
    .bind(key_id.to_string())
    .bind(ip)
    .execute(pool)
    .await;

    if let Err(e) = result {
        tracing::warn!(error = %e, key_id = %key_id, "Failed to update API key last_used");
    }
}

/// Roll an API key (replace with new hash/prefix, keeping metadata).
///
/// Logs a "rolled" event. The old token immediately stops working.
///
/// # Errors
///
/// Returns an error if the key doesn't exist or the database operation fails.
#[instrument(skip(pool, new_prefix, new_key_hash), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn roll_api_key(
    pool: &PgPool,
    key_id: Ulid,
    user_id: Ulid,
    new_prefix: &str,
    new_key_hash: &str,
    new_expires_at: Option<DateTime<Utc>>,
    ip: Option<&str>,
) -> Result<ApiKey, DbError> {
    let row = sqlx::query_as::<_, ApiKeyRow>(
        r"
        UPDATE api_keys
        SET prefix = $2, key_hash = $3, expires_at = $4, last_used_at = NULL, last_used_ip = NULL
        WHERE id = $1 AND user_id = $5
        RETURNING id, user_id, name, prefix, key_hash, scopes, expires_at, last_used_at, last_used_ip, created_at, updated_at
        ",
    )
    .bind(key_id.to_string())
    .bind(new_prefix)
    .bind(new_key_hash)
    .bind(new_expires_at)
    .bind(user_id.to_string())
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    let api_key = ApiKey::try_from(row)?;

    // Log the rolled event
    log_api_key_event(pool, key_id, user_id, ApiKeyEventType::Rolled, ip).await;

    Ok(api_key)
}

/// Delete an API key.
///
/// Logs a "deleted" event (events are retained for audit trail).
///
/// # Errors
///
/// Returns an error if the key doesn't exist or the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn delete_api_key(
    pool: &PgPool,
    key_id: Ulid,
    user_id: Ulid,
    ip: Option<&str>,
) -> Result<(), DbError> {
    let result = sqlx::query(
        r"
        DELETE FROM api_keys
        WHERE id = $1 AND user_id = $2
        ",
    )
    .bind(key_id.to_string())
    .bind(user_id.to_string())
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    // Log the deleted event (events survive key deletion)
    log_api_key_event(pool, key_id, user_id, ApiKeyEventType::Deleted, ip).await;

    Ok(())
}

/// List lifecycle events for an API key.
///
/// Returns events in chronological order (oldest first).
///
/// # Errors
///
/// Returns an error if the database operation fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
pub async fn list_api_key_events(
    pool: &PgPool,
    api_key_id: Ulid,
) -> Result<Vec<ApiKeyEvent>, DbError> {
    let rows = sqlx::query_as::<_, ApiKeyEventRow>(
        r"
        SELECT id, api_key_id, user_id, event_type, ip_address, created_at
        FROM api_key_events
        WHERE api_key_id = $1
        ORDER BY created_at ASC
        ",
    )
    .bind(api_key_id.to_string())
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(ApiKeyEvent::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Fire-and-forget API key event logging.
///
/// Logs errors via tracing but never fails.
#[instrument(skip(pool), fields(otel.kind = "client", db.system = "postgresql"))]
async fn log_api_key_event(
    pool: &PgPool,
    api_key_id: Ulid,
    user_id: Ulid,
    event_type: ApiKeyEventType,
    ip_address: Option<&str>,
) {
    let id = Ulid::new();
    let result = sqlx::query(
        r"
        INSERT INTO api_key_events (id, api_key_id, user_id, event_type, ip_address)
        VALUES ($1, $2, $3, $4, $5)
        ",
    )
    .bind(id.to_string())
    .bind(api_key_id.to_string())
    .bind(user_id.to_string())
    .bind(event_type)
    .bind(ip_address)
    .execute(pool)
    .await;

    if let Err(e) = result {
        tracing::warn!(error = %e, api_key_id = %api_key_id, "Failed to log API key event");
    }
}
