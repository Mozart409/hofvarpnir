//! Database connection pool and query helpers.

mod activity;
mod profile;
mod source;
mod user;
mod video;

use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

pub use activity::*;
pub use profile::*;
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

    #[error("Failed to connect to database: {0}")]
    ConnectionError(#[from] sqlx::Error),

    #[error("Migration failed: {0}")]
    MigrationError(#[from] sqlx::migrate::MigrateError),

    #[error("Invalid ULID: {0}")]
    InvalidUlid(#[from] ulid::DecodeError),

    #[error("Entity not found")]
    NotFound,
}

/// Create a new `PostgreSQL` connection pool with optimized settings for a download manager.
///
/// Pool configuration:
/// - `max_connections: 20` - Sufficient for concurrent downloads + API requests
/// - `min_connections: 2` - Keep warm connections for quick queries
/// - `acquire_timeout: 5s` - Generous timeout (downloads aren't time-critical)
/// - `idle_timeout: 300s` - Close idle connections after 5 minutes
/// - `max_lifetime: 600` - Recycle connections every 10 minutes
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
        .acquire_timeout(Duration::from_secs(5))
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
