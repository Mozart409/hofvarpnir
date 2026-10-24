//! API key database operations.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::postgres::PgPool;
use tokio::sync::mpsc;
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
    let id = Ulid::generate();
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

/// Touches buffered before a batch is written.
pub const TOUCH_BATCH_SIZE: usize = 5;

/// Longest the first touch in a batch waits before the batch is written, so a
/// partial batch is never held indefinitely.
pub const TOUCH_BATCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Touches queued behind a slow write before new ones are dropped. A touch is
/// best-effort bookkeeping and must never back-pressure a request.
const TOUCH_QUEUE_CAPACITY: usize = 1024;

/// One authenticated use of an API key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyTouch {
    pub key_id: Ulid,
    pub used_at: DateTime<Utc>,
    pub ip: Option<String>,
}

/// Handle for recording API key use.
///
/// Every API-key request used to spawn its own `UPDATE api_keys`. Parallel
/// requests with one key then queued on that row's lock, and in production
/// those single-row updates took 1-7s each, almost all of it waiting. A
/// background task now collects touches and writes them in one statement once
/// [`TOUCH_BATCH_SIZE`] have arrived or [`TOUCH_BATCH_TIMEOUT`] has passed
/// since the first, whichever comes first.
///
/// Cloning is cheap; all clones feed the same task. When the last clone is
/// dropped the task writes what it holds and exits. Touches still buffered
/// when the process is killed are lost, which is acceptable for a last-used
/// timestamp.
#[derive(Debug, Clone)]
pub struct ApiKeyTouches {
    tx: mpsc::Sender<ApiKeyTouch>,
}

impl ApiKeyTouches {
    /// Spawn the batching task on the current tokio runtime.
    ///
    /// # Panics
    ///
    /// Panics if called outside a tokio runtime (`tokio::spawn`).
    #[must_use]
    pub fn spawn(pool: PgPool) -> Self {
        let (tx, rx) = mpsc::channel(TOUCH_QUEUE_CAPACITY);
        tokio::spawn(run_touch_batches(
            rx,
            TOUCH_BATCH_SIZE,
            TOUCH_BATCH_TIMEOUT,
            move |batch| {
                let pool = pool.clone();
                async move { write_api_key_touches(&pool, batch).await }
            },
        ));
        Self { tx }
    }

    /// Queue a touch for `key_id`. Never waits: a full queue drops the touch.
    pub fn record(&self, key_id: Ulid, ip: Option<String>) {
        let touch = ApiKeyTouch {
            key_id,
            used_at: Utc::now(),
            ip,
        };
        if let Err(e) = self.tx.try_send(touch) {
            tracing::warn!(error = %e, key_id = %key_id, "Dropped API key last_used touch");
        }
    }
}

/// Collect touches from `rx` and hand each batch to `write`.
///
/// A batch is written as soon as it holds `batch_size` touches, or once
/// `timeout` has passed since its first touch arrived. The deadline is fixed
/// at the first touch, so a steady trickle cannot keep a batch open. Returns
/// after writing the last partial batch once every sender is gone.
async fn run_touch_batches<F, Fut>(
    mut rx: mpsc::Receiver<ApiKeyTouch>,
    batch_size: usize,
    timeout: Duration,
    mut write: F,
) where
    F: FnMut(Vec<ApiKeyTouch>) -> Fut,
    Fut: Future<Output = ()>,
{
    while let Some(first) = rx.recv().await {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut batch = Vec::with_capacity(batch_size);
        batch.push(first);

        while batch.len() < batch_size {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(touch)) => batch.push(touch),
                // Deadline passed, or all senders dropped.
                Ok(None) | Err(_) => break,
            }
        }

        write(batch).await;
    }
}

/// Write a batch of touches in one statement.
///
/// A key touched more than once in the batch is written once, with its latest
/// use. Best-effort: a failure is logged and the batch is dropped.
#[instrument(skip_all, fields(otel.kind = "client", db.system = "postgresql", batch_size = batch.len()))]
async fn write_api_key_touches(pool: &PgPool, batch: Vec<ApiKeyTouch>) {
    let mut latest: HashMap<Ulid, ApiKeyTouch> = HashMap::with_capacity(batch.len());
    for touch in batch {
        match latest.get(&touch.key_id) {
            Some(seen) if seen.used_at >= touch.used_at => {}
            _ => {
                latest.insert(touch.key_id, touch);
            }
        }
    }

    let mut ids = Vec::with_capacity(latest.len());
    let mut used_at = Vec::with_capacity(latest.len());
    let mut ips = Vec::with_capacity(latest.len());
    for touch in latest.into_values() {
        ids.push(touch.key_id.to_string());
        used_at.push(touch.used_at);
        ips.push(touch.ip);
    }

    let result = sqlx::query(
        r"
        UPDATE api_keys AS k
        SET last_used_at = t.used_at, last_used_ip = t.ip
        FROM UNNEST($1::text [], $2::timestamptz [], $3::text []) AS t (id, used_at, ip)
        WHERE k.id = t.id
        ",
    )
    .bind(&ids)
    .bind(&used_at)
    .bind(&ips)
    .execute(pool)
    .await;

    if let Err(e) = result {
        tracing::warn!(error = %e, keys = ids.len(), "Failed to update API key last_used");
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
    let id = Ulid::generate();
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::db::{CreateUser, create_user};

    // Ways the batcher could fail, each pinned by a test below:
    // 1. A partial batch (4 touches) is never written.
    // 2. A full batch waits for the timeout instead of being written at once.
    // 3. Touches past a full batch are lost instead of starting the next one.
    // 4. The deadline restarts on every touch, so a trickle holds a batch open.
    // 5. Shutdown drops the touches still buffered.
    // 6. A key touched twice in one batch keeps an older use, or the write
    //    touches keys that were not in the batch.

    type Written = Arc<Mutex<Vec<(Duration, Vec<ApiKeyTouch>)>>>;

    fn touch(n: u8) -> ApiKeyTouch {
        ApiKeyTouch {
            key_id: Ulid::from_parts(0, u128::from(n)),
            used_at: Utc::now(),
            ip: Some(format!("10.0.0.{n}")),
        }
    }

    /// Start the batcher with the production size and timeout; each write is
    /// recorded with the (paused) time it happened at.
    fn start() -> (
        mpsc::Sender<ApiKeyTouch>,
        Written,
        tokio::task::JoinHandle<()>,
    ) {
        let (tx, rx) = mpsc::channel(TOUCH_QUEUE_CAPACITY);
        let written: Written = Arc::default();
        let sink = Arc::clone(&written);
        let started = tokio::time::Instant::now();
        let task = tokio::spawn(run_touch_batches(
            rx,
            TOUCH_BATCH_SIZE,
            TOUCH_BATCH_TIMEOUT,
            move |batch| {
                sink.lock().unwrap().push((started.elapsed(), batch));
                async {}
            },
        ));
        (tx, written, task)
    }

    fn sizes(written: &Written) -> Vec<(Duration, usize)> {
        written
            .lock()
            .unwrap()
            .iter()
            .map(|(at, batch)| (*at, batch.len()))
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn partial_batch_is_written_after_the_timeout() {
        let (tx, written, _task) = start();
        for n in 0..4 {
            tx.send(touch(n)).await.unwrap();
        }

        tokio::time::sleep(Duration::from_millis(9_999)).await;
        assert!(sizes(&written).is_empty(), "written before the timeout");

        tokio::time::sleep(Duration::from_millis(2)).await;
        assert_eq!(sizes(&written), vec![(TOUCH_BATCH_TIMEOUT, 4)]);
    }

    #[tokio::test(start_paused = true)]
    async fn full_batch_is_written_at_once_and_the_rest_starts_a_new_one() {
        let (tx, written, _task) = start();
        for n in 0..7 {
            tx.send(touch(n)).await.unwrap();
        }

        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(sizes(&written), vec![(Duration::ZERO, 5)]);

        tokio::time::sleep(TOUCH_BATCH_TIMEOUT).await;
        assert_eq!(
            sizes(&written),
            vec![(Duration::ZERO, 5), (TOUCH_BATCH_TIMEOUT, 2)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_is_fixed_at_the_first_touch() {
        let (tx, written, _task) = start();
        // One touch every 3s: 0, 3, 6, 9 fall inside the first batch's 10s.
        for n in 0..4 {
            tx.send(touch(n)).await.unwrap();
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        // Now at 12s; the first batch closed at 10s with four touches.
        assert_eq!(sizes(&written), vec![(TOUCH_BATCH_TIMEOUT, 4)]);
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_writes_the_buffered_touches() {
        let (tx, written, task) = start();
        for n in 0..3 {
            tx.send(touch(n)).await.unwrap();
        }
        drop(tx);

        task.await.unwrap();
        assert_eq!(sizes(&written), vec![(Duration::ZERO, 3)]);
    }

    #[sqlx::test]
    async fn write_keeps_the_latest_use_per_key(pool: PgPool) {
        let user = create_user(
            &pool,
            CreateUser {
                email: "touch@example.com",
                name: "Touch User",
                password_hash: None,
            },
        )
        .await
        .unwrap();
        let mut keys = Vec::new();
        for name in ["a", "b", "untouched"] {
            let key = create_api_key(
                &pool,
                CreateApiKey {
                    user_id: user.id,
                    name,
                    prefix: name,
                    key_hash: &format!("hash-{name}"),
                    scopes: &[ApiKeyScope::Read],
                    expires_at: None,
                },
            )
            .await
            .unwrap();
            keys.push(key.id);
        }

        let t0 = Utc::now() - chrono::Duration::minutes(5);
        let use_of = |key_id, minutes, ip: &str| ApiKeyTouch {
            key_id,
            used_at: t0 + chrono::Duration::minutes(minutes),
            ip: Some(ip.to_string()),
        };
        // Key a's latest use arrives first, so arrival order must not win.
        write_api_key_touches(
            &pool,
            vec![
                use_of(keys[0], 3, "10.0.0.3"),
                use_of(keys[1], 1, "10.0.0.9"),
                use_of(keys[0], 2, "10.0.0.2"),
            ],
        )
        .await;

        let last_used = |key_id: Ulid| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (Option<DateTime<Utc>>, Option<String>)>(
                    "SELECT last_used_at, last_used_ip FROM api_keys WHERE id = $1",
                )
                .bind(key_id.to_string())
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };

        let (a_at, a_ip) = last_used(keys[0]).await;
        assert_eq!(a_ip.as_deref(), Some("10.0.0.3"));
        assert_eq!(
            a_at.map(|t| t.timestamp_micros()),
            Some((t0 + chrono::Duration::minutes(3)).timestamp_micros())
        );
        let (_, b_ip) = last_used(keys[1]).await;
        assert_eq!(b_ip.as_deref(), Some("10.0.0.9"));
        assert_eq!(last_used(keys[2]).await, (None, None));
    }
}
