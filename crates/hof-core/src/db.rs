//! Database connection pool and query helpers.

use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("DATABASE_URL environment variable is not set")]
    MissingDatabaseUrl,

    #[error("Failed to connect to database: {0}")]
    ConnectionError(#[from] sqlx::Error),
}

/// Create a new `PostgreSQL` connection pool with optimized settings for a download manager.
///
/// Pool configuration:
/// - `max_connections: 20` - Sufficient for concurrent downloads + API requests
/// - `min_connections: 2` - Keep warm connections for quick queries
/// - `acquire_timeout: 30s` - Generous timeout (downloads aren't time-critical)
/// - `idle_timeout: 600s` - Close idle connections after 10 minutes
/// - `max_lifetime: 1800s` - Recycle connections every 30 minutes
///
/// # Errors
///
/// Returns `DbError::MissingDatabaseUrl` if the `DATABASE_URL` environment variable is not set.
/// Returns `DbError::ConnectionError` if the database connection fails.
pub async fn create_pool() -> Result<PgPool, DbError> {
    let database_url = std::env::var("DATABASE_URL").map_err(|_| DbError::MissingDatabaseUrl)?;

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(30))
        .idle_timeout(Duration::from_secs(600))
        .max_lifetime(Duration::from_secs(1800))
        .connect(&database_url)
        .await?;

    Ok(pool)
}
