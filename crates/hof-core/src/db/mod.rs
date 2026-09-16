//! Database connection pool and query helpers.

mod activity;
mod api_key;
mod oidc;
mod profile;
mod runtime_settings;
mod source;
mod user;
mod video;

use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};
use tracing::warn;

pub use activity::*;
pub use api_key::*;
pub use oidc::*;
pub use profile::*;
pub use runtime_settings::*;
pub use source::*;
pub use user::*;
pub use video::*;

/// Maximum number of consecutive indexing errors before automatic retries stop.
/// Sources exceeding this limit can still be manually indexed via "Force index".
pub const MAX_INDEX_RETRIES: i32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("DATABASE_URL environment variable is not set")]
    MissingDatabaseUrl,

    /// `Pool::acquire` gave up waiting for a free connection.
    ///
    /// This is a *capacity* fault, not a connectivity fault: the pool is
    /// established and the server is reachable, but all
    /// `max_connections` were checked out for longer than
    /// `ACQUIRE_TIMEOUT`. It is transient by nature and almost always
    /// resolves on its own within a second — see [`with_acquire_retry`].
    #[error("Database pool timed out acquiring a connection after {waited:?}")]
    PoolTimeout { waited: Duration },

    /// The connection itself is broken: socket, TLS, connection string, or a
    /// pool that has been closed out from under the caller.
    #[error("Failed to connect to database: {0}")]
    ConnectionError(#[source] sqlx::Error),

    /// The statement reached the server and the server (or the decoder)
    /// rejected it: syntax, constraint violation, missing row, type mismatch.
    /// Retrying cannot help.
    #[error("Database query failed: {0}")]
    QueryError(#[source] sqlx::Error),

    #[error("Migration failed: {0}")]
    MigrationError(#[from] sqlx::migrate::MigrateError),

    #[error("Invalid ULID: {0}")]
    InvalidUlid(#[from] ulid::DecodeError),

    #[error("Entity not found")]
    NotFound,
}

impl DbError {
    /// Whether this failure is a transient connection-*acquisition* fault that
    /// a retry could plausibly clear.
    ///
    /// Deliberately narrow: only [`DbError::PoolTimeout`] and
    /// [`DbError::ConnectionError`] qualify. A [`DbError::QueryError`] means
    /// the server saw the statement and answered "no" — a constraint
    /// violation or a syntax error will answer "no" just as fast on the
    /// second try, and retrying it only multiplies the log noise.
    ///
    /// Callers must still ensure the retried operation is idempotent. A
    /// mid-statement socket error (`ConnectionError`) cannot prove the
    /// statement did not commit server-side, so a non-idempotent write should
    /// be wrapped in a transaction — or not retried at all.
    pub const fn is_transient_acquire(&self) -> bool {
        matches!(self, Self::PoolTimeout { .. } | Self::ConnectionError(_))
    }

    /// The underlying database error, whichever variant happens to wrap it.
    ///
    /// Call sites that care about a SQLSTATE -- a CHECK violation, a unique
    /// conflict -- should not have to know which variant [`classify`] landed
    /// a given `sqlx::Error` in. That mapping is an implementation detail,
    /// and splitting `ConnectionError` into three variants silently broke one
    /// such call site (`runtime_settings`'s CHECK-constraint assertions) by
    /// moving constraint violations from `ConnectionError` to `QueryError`.
    /// Matching here once means the next reclassification cannot repeat that.
    #[must_use]
    pub fn as_database_error(&self) -> Option<&(dyn sqlx::error::DatabaseError + 'static)> {
        match self {
            Self::ConnectionError(e) | Self::QueryError(e) => e.as_database_error(),
            Self::MissingDatabaseUrl
            | Self::PoolTimeout { .. }
            | Self::MigrationError(_)
            | Self::InvalidUlid(_)
            | Self::NotFound => None,
        }
    }
}

/// Classify a raw sqlx error into the specific [`DbError`] variant.
///
/// Every `sqlx::Error` used to collapse into a single `ConnectionError`
/// variant whose message read "Failed to connect to database". That blanket
/// mapping cost a 27-hour production outage a full investigation: the real
/// fault was a pool-acquire timeout on an established, healthy pool, but the
/// log line said the database was unreachable, so the connectivity of the
/// database was audited instead of the saturation of the pool. Splitting the
/// variants means the log line names the actual failure mode, and it lets
/// [`with_acquire_retry`] decide retryability from the type rather than by
/// string-matching a message.
///
/// The mapping:
/// - `sqlx::Error::PoolTimedOut` -> [`DbError::PoolTimeout`]
/// - `Io` / `Tls` / `Configuration` / `PoolClosed` -> [`DbError::ConnectionError`]
/// - everything else -> [`DbError::QueryError`]
///
/// `sqlx::Error` is `#[non_exhaustive]`, so the fallback arm is required
/// regardless. Defaulting the unknown remainder to `QueryError` is the safe
/// direction: a misclassified query error is merely mislabelled, while a
/// misclassified *connection* error would be handed to the retry loop and
/// replayed.
pub fn classify(e: sqlx::Error) -> DbError {
    // Matched through a reference so the error value can be moved into the
    // chosen variant in the arm body.
    match &e {
        sqlx::Error::PoolTimedOut => DbError::PoolTimeout {
            waited: ACQUIRE_TIMEOUT,
        },
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Configuration(_)
        | sqlx::Error::PoolClosed => DbError::ConnectionError(e),
        _ => DbError::QueryError(e),
    }
}

/// Hand-written stand-in for the `#[from]` that used to live on
/// `ConnectionError`.
///
/// Dozens of `?` sites across `hof-core`, `hof-api` and `hof-web` depend on
/// `sqlx::Error` converting into `DbError` implicitly. Splitting the variants
/// must not turn into a workspace-wide edit, so the conversion stays — it just
/// routes through [`classify`] instead of always producing `ConnectionError`.
impl From<sqlx::Error> for DbError {
    fn from(e: sqlx::Error) -> Self {
        classify(e)
    }
}

/// How long `Pool::acquire` waits for a free connection before giving up with
/// `sqlx::Error::PoolTimedOut`.
///
/// Also the value reported as `waited` on [`DbError::PoolTimeout`]: sqlx's
/// error carries no duration, but by construction a pool-acquire timeout
/// waited exactly this long.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Number of attempts [`with_acquire_retry`] makes, including the first.
const MAX_ACQUIRE_ATTEMPTS: usize = 3;

/// Delay applied *after* a failed attempt, indexed by the (1-based) attempt
/// number that just failed. Roughly a 5x ladder: long enough for a saturated
/// pool to drain a slow query, short enough that a request handler or actor
/// message does not visibly stall.
///
/// The ladder carries one more rung than [`MAX_ACQUIRE_ATTEMPTS`] uses, so the
/// attempt count can be raised without also having to re-derive the schedule;
/// the lookup is bounds-checked rather than indexed (this crate denies
/// `clippy::indexing_slicing`) and falls back to the last rung.
const ACQUIRE_RETRY_BACKOFF: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_secs(2),
];

/// Run a database operation, retrying only transient
/// connection-acquisition faults.
///
/// [`MAX_ACQUIRE_ATTEMPTS`] attempts total, separated by
/// [`ACQUIRE_RETRY_BACKOFF`]. A query, constraint, or decode error returns
/// immediately from the first attempt; only [`DbError::is_transient_acquire`]
/// failures are replayed.
///
/// Why this exists: a five-second `acquire_timeout` on a busy pool produces a
/// hard `Err` that, inside a fire-and-forget kameo `tell()` handler, is
/// escalated to `on_panic` and kills the actor. Absorbing the fault at the
/// query call site keeps a sub-second capacity blip from becoming a dead
/// actor. This is the inner half of the fix; supervision (restarting an actor
/// that dies anyway) lives above this module.
///
/// `op` is a closure rather than a future because a retry needs a *fresh*
/// future each attempt — a future cannot be polled again after it resolves.
///
/// ```ignore
/// let video = with_acquire_retry(|| async {
///     sqlx::query_as::<_, Video>("SELECT * FROM videos WHERE id = $1")
///         .bind(id)
///         .fetch_one(&pool)
///         .await
/// })
/// .await?;
/// ```
///
/// # Errors
///
/// Returns the [`classify`]-ed error from the final attempt. Non-transient
/// errors are returned without any retry or delay.
pub async fn with_acquire_retry<T, F, Fut>(op: F) -> Result<T, DbError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    let mut attempt: usize = 1;

    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(raw) => {
                let err = classify(raw);

                if !err.is_transient_acquire() || attempt >= MAX_ACQUIRE_ATTEMPTS {
                    return Err(err);
                }

                let backoff = ACQUIRE_RETRY_BACKOFF
                    .get(attempt.saturating_sub(1))
                    .or_else(|| ACQUIRE_RETRY_BACKOFF.last())
                    .copied()
                    .unwrap_or(ACQUIRE_TIMEOUT);

                warn!(
                    attempt,
                    max_attempts = MAX_ACQUIRE_ATTEMPTS,
                    ?backoff,
                    error = %err,
                    "Transient database acquire failure; retrying"
                );

                tokio::time::sleep(backoff).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// Create a new `PostgreSQL` connection pool sized for a download manager.
///
/// Pool configuration:
/// - `max_connections: 20` - concurrent downloads + API/web requests
/// - `min_connections: 2` - keep warm connections for quick queries
/// - `acquire_timeout: 5s` - see [`ACQUIRE_TIMEOUT`] and the trade-off below
/// - `idle_timeout: 300s` - close idle connections after 5 minutes
/// - `max_lifetime: 600s` - recycle connections every 10 minutes
///
/// # The acquire timeout is a deadline, not a courtesy
///
/// This doc block used to describe the 5s `acquire_timeout` as a "generous
/// timeout (downloads aren't time-critical)". That reasoning conflated two
/// unrelated clocks. `acquire_timeout` does not bound how long a download
/// runs; it bounds how long a *single database call* waits for one of the 20
/// pooled connections to come free. Against that clock 5s is short, not
/// generous: one slow query holding a connection, or a scheduler tick fanning
/// out across many sources at once, is enough to make an unlucky caller time
/// out on a pool that is fully established and perfectly healthy.
///
/// The trade-off is therefore: a short timeout fails fast and keeps the
/// waiter queue from growing without bound, at the cost of surfacing
/// transient saturation as an error to the caller. Raising the number does
/// not remove that cost, it only converts a fast error into a slow one and
/// lets the queue deepen — so the number stays where it is and the *callers*
/// absorb the blip.
///
/// Concretely, this 5s deadline is what caused a 27-hour download outage: an
/// acquire timeout inside a fire-and-forget kameo `tell()` handler returned
/// `Err`, kameo escalated the error reply to `on_panic`, and the actor was
/// killed and never restarted. Callers on hot paths — actor message handlers,
/// request handlers, anything on the download critical path — should wrap
/// their query in [`with_acquire_retry`] so a sub-second capacity blip is
/// retried rather than propagated. Supervision is the other half of that fix
/// and lives above this module.
///
/// # Errors
///
/// Returns `DbError::MissingDatabaseUrl` if the `DATABASE_URL` environment
/// variable is not set, `DbError::ConnectionError` if the initial connection
/// cannot be established (bad URL, unreachable host, TLS failure), or
/// `DbError::PoolTimeout` if `min_connections` cannot be opened within
/// [`ACQUIRE_TIMEOUT`].
pub async fn create_pool() -> Result<PgPool, DbError> {
    let database_url = std::env::var("DATABASE_URL").map_err(|_| DbError::MissingDatabaseUrl)?;

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .min_connections(2)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(Duration::from_mins(5))
        .max_lifetime(Duration::from_mins(10))
        .connect(&database_url)
        .await?;

    Ok(pool)
}

/// Run pending database migrations.
///
/// # Errors
///
/// Returns an error if migrations fail to run.
pub async fn run_migrations(pool: &PgPool) -> Result<(), DbError> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    // These tests exercise error classification and the retry loop only, so
    // none of them need a live database — unlike the `#[sqlx::test]` cases in
    // the sibling modules, they run under a plain `cargo test`.

    /// Build a `sqlx::Error` that stands in for a broken socket. `Io` is the
    /// variant sqlx produces when the TCP connection to Postgres dies.
    fn io_error() -> sqlx::Error {
        sqlx::Error::Io(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "connection reset by peer",
        ))
    }

    #[test]
    fn classify_maps_pool_timeout_to_pool_timeout() {
        let classified = classify(sqlx::Error::PoolTimedOut);

        assert!(
            matches!(classified, DbError::PoolTimeout { waited } if waited == ACQUIRE_TIMEOUT),
            "expected PoolTimeout carrying the configured acquire timeout, got {classified:?}"
        );
        // The whole point of the split: this must no longer claim the
        // database is unreachable.
        let rendered = classified.to_string();
        assert!(
            !rendered.contains("Failed to connect"),
            "pool timeout must not render as a connection failure: {rendered}"
        );
    }

    #[test]
    fn classify_maps_io_error_to_connection_error() {
        let classified = classify(io_error());

        assert!(
            matches!(classified, DbError::ConnectionError(_)),
            "expected ConnectionError for a socket failure, got {classified:?}"
        );
    }

    #[test]
    fn classify_maps_closed_pool_to_connection_error() {
        let classified = classify(sqlx::Error::PoolClosed);

        assert!(
            matches!(classified, DbError::ConnectionError(_)),
            "expected ConnectionError for a closed pool, got {classified:?}"
        );
    }

    #[test]
    fn classify_maps_decode_and_database_errors_to_query_error() {
        let decode = classify(sqlx::Error::ColumnDecode {
            index: "0".to_owned(),
            source: "invalid utf-8 sequence".into(),
        });
        assert!(
            matches!(decode, DbError::QueryError(_)),
            "expected QueryError for a column decode failure, got {decode:?}"
        );

        let missing_row = classify(sqlx::Error::RowNotFound);
        assert!(
            matches!(missing_row, DbError::QueryError(_)),
            "expected QueryError for a missing row, got {missing_row:?}"
        );
    }

    #[test]
    fn only_acquire_faults_are_transient() {
        assert!(
            classify(sqlx::Error::PoolTimedOut).is_transient_acquire(),
            "pool timeouts must be retryable"
        );
        assert!(
            classify(io_error()).is_transient_acquire(),
            "socket failures must be retryable"
        );
        assert!(
            !classify(sqlx::Error::RowNotFound).is_transient_acquire(),
            "a missing row must never be retried"
        );
        assert!(
            !DbError::NotFound.is_transient_acquire(),
            "domain-level NotFound must never be retried"
        );
    }

    /// `start_paused` lets tokio auto-advance past the backoff sleeps, so the
    /// real 100ms + 500ms ladder costs the test suite nothing.
    #[tokio::test(start_paused = true)]
    async fn with_acquire_retry_recovers_from_a_pool_timeout() {
        let calls = AtomicUsize::new(0);

        let result = with_acquire_retry(|| {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                // Fail the first two attempts, succeed on the third — the
                // last attempt the loop is allowed to make.
                if call < 2 {
                    Err(sqlx::Error::PoolTimedOut)
                } else {
                    Ok(7_u32)
                }
            }
        })
        .await;

        assert_eq!(result.ok(), Some(7));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            MAX_ACQUIRE_ATTEMPTS,
            "expected the operation to be attempted exactly {MAX_ACQUIRE_ATTEMPTS} times"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn with_acquire_retry_gives_up_after_the_attempt_budget() {
        let calls = AtomicUsize::new(0);

        let result = with_acquire_retry(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(sqlx::Error::PoolTimedOut) }
        })
        .await;

        assert!(
            matches!(result, Err(DbError::PoolTimeout { .. })),
            "a persistently saturated pool must surface as PoolTimeout"
        );
        assert_eq!(calls.load(Ordering::SeqCst), MAX_ACQUIRE_ATTEMPTS);
    }

    #[tokio::test(start_paused = true)]
    async fn with_acquire_retry_does_not_retry_a_query_error() {
        let calls = AtomicUsize::new(0);

        let result = with_acquire_retry(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(sqlx::Error::RowNotFound) }
        })
        .await;

        assert!(
            matches!(result, Err(DbError::QueryError(sqlx::Error::RowNotFound))),
            "a missing row must be reported as a query error"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a query error must not be replayed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn with_acquire_retry_passes_through_a_first_attempt_success() {
        let calls = AtomicUsize::new(0);

        let result = with_acquire_retry(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, sqlx::Error>("ok") }
        })
        .await;

        assert_eq!(result.ok(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
