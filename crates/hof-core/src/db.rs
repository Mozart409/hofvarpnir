//! Database connection pool and query helpers.

use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

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
/// Returns an error if the database connection fails or if the `DATABASE_URL`
/// environment variable is not set.
pub async fn create_pool() -> Result<PgPool, sqlx::Error> {
    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set in environment");

    PgPoolOptions::new()
        .max_connections(20)
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(30))
        .idle_timeout(Duration::from_secs(600))
        .max_lifetime(Duration::from_secs(1800))
        .connect(&database_url)
        .await
}
