//! Database connection pool and query helpers.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;
use ulid::Ulid;

use crate::domain::{
    profile::{Profile, ProfileRow, Quality},
    source::{Source, SourceRow, SourceType},
    user::{User, UserRow},
    video::{Video, VideoRow, VideoStatus},
};

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

/// Run pending database migrations.
///
/// # Errors
///
/// Returns an error if migrations fail to run.
pub async fn run_migrations(pool: &PgPool) -> Result<(), DbError> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

// ============================================================================
// User CRUD
// ============================================================================

/// Data required to create a new user.
#[derive(Debug, Clone)]
pub struct CreateUser<'a> {
    pub email: &'a str,
    pub name: &'a str,
    pub password_hash: &'a str,
}

/// Data for updating an existing user.
#[derive(Debug, Clone)]
pub struct UpdateUser<'a> {
    pub email: Option<&'a str>,
    pub name: Option<&'a str>,
    pub password_hash: Option<&'a str>,
}

/// Create a new user.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn create_user(pool: &PgPool, data: CreateUser<'_>) -> Result<User, DbError> {
    let id = Ulid::new();
    let row = sqlx::query_as::<_, UserRow>(
        r"
        INSERT INTO users (id, email, name, password_hash)
        VALUES ($1, $2, $3, $4)
        RETURNING id, email, name, password_hash, created_at, updated_at
        ",
    )
    .bind(id.to_string())
    .bind(data.email)
    .bind(data.name)
    .bind(data.password_hash)
    .fetch_one(pool)
    .await?;

    Ok(User::try_from(row)?)
}

/// Get a user by ID.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the user doesn't exist.
pub async fn get_user(pool: &PgPool, id: Ulid) -> Result<User, DbError> {
    let row = sqlx::query_as::<_, UserRow>(
        r"
        SELECT id, email, name, password_hash, created_at, updated_at
        FROM users
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    Ok(User::try_from(row)?)
}

/// Get a user by email.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the user doesn't exist.
pub async fn get_user_by_email(pool: &PgPool, email: &str) -> Result<User, DbError> {
    let row = sqlx::query_as::<_, UserRow>(
        r"
        SELECT id, email, name, password_hash, created_at, updated_at
        FROM users
        WHERE email = $1
        ",
    )
    .bind(email)
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    Ok(User::try_from(row)?)
}

/// List all users.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn list_users(pool: &PgPool) -> Result<Vec<User>, DbError> {
    let rows = sqlx::query_as::<_, UserRow>(
        r"
        SELECT id, email, name, password_hash, created_at, updated_at
        FROM users
        ORDER BY created_at DESC
        ",
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(User::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Update a user.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the user doesn't exist.
pub async fn update_user(pool: &PgPool, id: Ulid, data: UpdateUser<'_>) -> Result<User, DbError> {
    let row = sqlx::query_as::<_, UserRow>(
        r"
        UPDATE users
        SET email = COALESCE($2, email),
            name = COALESCE($3, name),
            password_hash = COALESCE($4, password_hash)
        WHERE id = $1
        RETURNING id, email, name, password_hash, created_at, updated_at
        ",
    )
    .bind(id.to_string())
    .bind(data.email)
    .bind(data.name)
    .bind(data.password_hash)
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    Ok(User::try_from(row)?)
}

/// Delete a user.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the user doesn't exist.
pub async fn delete_user(pool: &PgPool, id: Ulid) -> Result<(), DbError> {
    let result = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id.to_string())
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

// ============================================================================
// Profile CRUD
// ============================================================================

/// Data required to create a new profile.
#[derive(Debug, Clone)]
pub struct CreateProfile<'a> {
    pub user_id: Ulid,
    pub name: &'a str,
    pub quality: Quality,
    pub naming_template: &'a str,
    pub output_dir: &'a str,
    pub include_livestreams: bool,
    pub include_shorts: bool,
    pub storage_quota_bytes: i64,
    pub retention_days: Option<i32>,
}

/// Data for updating an existing profile.
#[derive(Debug, Clone, Default)]
pub struct UpdateProfile<'a> {
    pub name: Option<&'a str>,
    pub quality: Option<Quality>,
    pub naming_template: Option<&'a str>,
    pub output_dir: Option<&'a str>,
    pub include_livestreams: Option<bool>,
    pub include_shorts: Option<bool>,
    pub storage_quota_bytes: Option<i64>,
    pub retention_days: Option<Option<i32>>,
}

/// Create a new profile.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn create_profile(pool: &PgPool, data: CreateProfile<'_>) -> Result<Profile, DbError> {
    let id = Ulid::new();
    let row = sqlx::query_as::<_, ProfileRow>(
        r"
        INSERT INTO profiles (id, user_id, name, quality, naming_template, output_dir,
                              include_livestreams, include_shorts, storage_quota_bytes, retention_days)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        RETURNING id, user_id, name, quality, naming_template, output_dir,
                  include_livestreams, include_shorts, storage_quota_bytes, retention_days,
                  created_at, updated_at
        ",
    )
    .bind(id.to_string())
    .bind(data.user_id.to_string())
    .bind(data.name)
    .bind(&data.quality)
    .bind(data.naming_template)
    .bind(data.output_dir)
    .bind(data.include_livestreams)
    .bind(data.include_shorts)
    .bind(data.storage_quota_bytes)
    .bind(data.retention_days)
    .fetch_one(pool)
    .await?;

    Ok(Profile::try_from(row)?)
}

/// Get a profile by ID.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the profile doesn't exist.
pub async fn get_profile(pool: &PgPool, id: Ulid) -> Result<Profile, DbError> {
    let row = sqlx::query_as::<_, ProfileRow>(
        r"
        SELECT id, user_id, name, quality, naming_template, output_dir,
               include_livestreams, include_shorts, storage_quota_bytes, retention_days,
               created_at, updated_at
        FROM profiles
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    Ok(Profile::try_from(row)?)
}

/// List all profiles for a user.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn list_profiles_for_user(pool: &PgPool, user_id: Ulid) -> Result<Vec<Profile>, DbError> {
    let rows = sqlx::query_as::<_, ProfileRow>(
        r"
        SELECT id, user_id, name, quality, naming_template, output_dir,
               include_livestreams, include_shorts, storage_quota_bytes, retention_days,
               created_at, updated_at
        FROM profiles
        WHERE user_id = $1
        ORDER BY created_at DESC
        ",
    )
    .bind(user_id.to_string())
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(Profile::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// List all profiles.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn list_profiles(pool: &PgPool) -> Result<Vec<Profile>, DbError> {
    let rows = sqlx::query_as::<_, ProfileRow>(
        r"
        SELECT id, user_id, name, quality, naming_template, output_dir,
               include_livestreams, include_shorts, storage_quota_bytes, retention_days,
               created_at, updated_at
        FROM profiles
        ORDER BY created_at DESC
        ",
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(Profile::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Update a profile.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the profile doesn't exist.
pub async fn update_profile(
    pool: &PgPool,
    id: Ulid,
    data: UpdateProfile<'_>,
) -> Result<Profile, DbError> {
    let row = sqlx::query_as::<_, ProfileRow>(
        r"
        UPDATE profiles
        SET name = COALESCE($2, name),
            quality = COALESCE($3, quality),
            naming_template = COALESCE($4, naming_template),
            output_dir = COALESCE($5, output_dir),
            include_livestreams = COALESCE($6, include_livestreams),
            include_shorts = COALESCE($7, include_shorts),
            storage_quota_bytes = COALESCE($8, storage_quota_bytes),
            retention_days = CASE WHEN $9 THEN $10 ELSE retention_days END
        WHERE id = $1
        RETURNING id, user_id, name, quality, naming_template, output_dir,
                  include_livestreams, include_shorts, storage_quota_bytes, retention_days,
                  created_at, updated_at
        ",
    )
    .bind(id.to_string())
    .bind(data.name)
    .bind(data.quality.as_ref())
    .bind(data.naming_template)
    .bind(data.output_dir)
    .bind(data.include_livestreams)
    .bind(data.include_shorts)
    .bind(data.storage_quota_bytes)
    .bind(data.retention_days.is_some())
    .bind(data.retention_days.flatten())
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    Ok(Profile::try_from(row)?)
}

/// Delete a profile.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the profile doesn't exist.
pub async fn delete_profile(pool: &PgPool, id: Ulid) -> Result<(), DbError> {
    let result = sqlx::query("DELETE FROM profiles WHERE id = $1")
        .bind(id.to_string())
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }

    Ok(())
}

// ============================================================================
// Source CRUD
// ============================================================================

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

// SQL fragment for selecting all source columns
const SOURCE_COLUMNS: &str = r"
    id, profile_id, url, source_type, custom_name,
    index_frequency_secs, cutoff_date, retention_days,
    last_indexed_at, last_error, index_error_count, created_at, updated_at,
    channel_id, channel_title, channel_description, channel_thumbnail_url, jellyfin_metadata_at
";

/// Create a new source.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn create_source(pool: &PgPool, data: CreateSource<'_>) -> Result<Source, DbError> {
    let id = Ulid::new();
    let query = format!(
        r"
        INSERT INTO sources (id, profile_id, url, source_type, custom_name,
                             index_frequency_secs, cutoff_date, retention_days)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING {SOURCE_COLUMNS}
        "
    );
    let row = sqlx::query_as::<_, SourceRow>(&query)
        .bind(id.to_string())
        .bind(data.profile_id.to_string())
        .bind(data.url)
        .bind(&data.source_type)
        .bind(data.custom_name)
        .bind(data.index_frequency_secs)
        .bind(data.cutoff_date)
        .bind(data.retention_days)
        .fetch_one(pool)
        .await?;

    Ok(Source::try_from(row)?)
}

/// Get a source by ID.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
pub async fn get_source(pool: &PgPool, id: Ulid) -> Result<Source, DbError> {
    let query = format!(
        r"
        SELECT {SOURCE_COLUMNS}
        FROM sources
        WHERE id = $1
        "
    );
    let row = sqlx::query_as::<_, SourceRow>(&query)
        .bind(id.to_string())
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
pub async fn list_sources_for_profile(
    pool: &PgPool,
    profile_id: Ulid,
) -> Result<Vec<Source>, DbError> {
    let query = format!(
        r"
        SELECT {SOURCE_COLUMNS}
        FROM sources
        WHERE profile_id = $1
        ORDER BY created_at DESC
        "
    );
    let rows = sqlx::query_as::<_, SourceRow>(&query)
        .bind(profile_id.to_string())
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
pub async fn list_sources(pool: &PgPool) -> Result<Vec<Source>, DbError> {
    let query = format!(
        r"
        SELECT {SOURCE_COLUMNS}
        FROM sources
        ORDER BY created_at DESC
        "
    );
    let rows = sqlx::query_as::<_, SourceRow>(&query)
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
/// - `last_indexed_at` is NULL, OR
/// - `last_indexed_at + index_frequency_secs` < NOW
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn list_sources_due_for_indexing(pool: &PgPool) -> Result<Vec<Source>, DbError> {
    let query = format!(
        r"
        SELECT {SOURCE_COLUMNS}
        FROM sources
        WHERE last_indexed_at IS NULL
           OR last_indexed_at + make_interval(secs => index_frequency_secs) < NOW()
        ORDER BY last_indexed_at NULLS FIRST
        "
    );
    let rows = sqlx::query_as::<_, SourceRow>(&query)
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
pub async fn update_source(
    pool: &PgPool,
    id: Ulid,
    data: UpdateSource<'_>,
) -> Result<Source, DbError> {
    let query = format!(
        r"
        UPDATE sources
        SET url = COALESCE($2, url),
            source_type = COALESCE($3, source_type),
            custom_name = CASE WHEN $4 THEN $5 ELSE custom_name END,
            index_frequency_secs = COALESCE($6, index_frequency_secs),
            cutoff_date = COALESCE($7, cutoff_date),
            retention_days = CASE WHEN $8 THEN $9 ELSE retention_days END
        WHERE id = $1
        RETURNING {SOURCE_COLUMNS}
        "
    );
    let row = sqlx::query_as::<_, SourceRow>(&query)
        .bind(id.to_string())
        .bind(data.url)
        .bind(data.source_type.as_ref())
        .bind(data.custom_name.is_some())
        .bind(data.custom_name.flatten())
        .bind(data.index_frequency_secs)
        .bind(data.cutoff_date)
        .bind(data.retention_days.is_some())
        .bind(data.retention_days.flatten())
        .fetch_optional(pool)
        .await?
        .ok_or(DbError::NotFound)?;

    Ok(Source::try_from(row)?)
}

/// Update the last indexed timestamp for a source and clear any error state.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
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

/// Data for updating channel metadata on a source.
#[derive(Debug, Clone, Default)]
pub struct UpdateChannelMetadata<'a> {
    pub channel_id: Option<&'a str>,
    pub channel_title: Option<&'a str>,
    pub channel_description: Option<&'a str>,
    pub channel_thumbnail_url: Option<&'a str>,
}

/// Update channel metadata for a source (from indexing results).
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
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
pub async fn list_sources_needing_jellyfin_metadata(pool: &PgPool) -> Result<Vec<Source>, DbError> {
    let query = format!(
        r"
        SELECT {SOURCE_COLUMNS}
        FROM sources
        WHERE jellyfin_metadata_at IS NULL
        ORDER BY created_at ASC
        "
    );
    let rows = sqlx::query_as::<_, SourceRow>(&query)
        .fetch_all(pool)
        .await?;

    rows.into_iter()
        .map(Source::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
}

/// Delete a source.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the source doesn't exist.
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
// Video CRUD
// ============================================================================

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
pub async fn create_video(pool: &PgPool, data: CreateVideo<'_>) -> Result<Video, DbError> {
    let id = Ulid::new();
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
pub async fn upsert_video(pool: &PgPool, data: CreateVideo<'_>) -> Result<Video, DbError> {
    let id = Ulid::new();
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

/// List videos for a specific source (via join table).
///
/// # Errors
///
/// Returns an error if the database operation fails.
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

/// Update a video.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the video doesn't exist.
#[allow(clippy::too_many_lines)]
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

/// Delete a video.
///
/// # Errors
///
/// Returns `DbError::NotFound` if the video doesn't exist.
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

// ============================================================================
// Source-Video Link Operations (Join Table)
// ============================================================================

/// Link a video to a source.
///
/// # Errors
///
/// Returns an error if the database operation fails (e.g., duplicate link).
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

#[cfg(test)]
mod tests {
    use super::*;

    // Integration tests require a running database.
    // Run with: DATABASE_URL=postgres://... cargo test -p hof-core --all-features -- --include-ignored

    #[tokio::test]
    #[ignore = "requires database"]
    async fn test_create_user() {
        let pool = create_pool().await.expect("Failed to create pool");
        run_migrations(&pool)
            .await
            .expect("Failed to run migrations");

        let user = create_user(
            &pool,
            CreateUser {
                email: "test@example.com",
                name: "Test User",
                password_hash: "$argon2id$v=19$m=16,t=2,p=1$dGVzdHNhbHQ$test",
            },
        )
        .await
        .expect("Failed to create user");

        assert_eq!(user.email, "test@example.com");
        assert_eq!(user.name, "Test User");

        // Cleanup
        delete_user(&pool, user.id)
            .await
            .expect("Failed to delete user");
    }

    #[tokio::test]
    #[ignore = "requires database"]
    async fn test_user_crud() {
        let pool = create_pool().await.expect("Failed to create pool");
        run_migrations(&pool)
            .await
            .expect("Failed to run migrations");

        // Create
        let user = create_user(
            &pool,
            CreateUser {
                email: "crud@example.com",
                name: "CRUD User",
                password_hash: "$argon2id$v=19$m=16,t=2,p=1$dGVzdHNhbHQ$test",
            },
        )
        .await
        .expect("Failed to create user");

        // Read
        let fetched = get_user(&pool, user.id).await.expect("Failed to get user");
        assert_eq!(fetched.id, user.id);

        // Update
        let updated = update_user(
            &pool,
            user.id,
            UpdateUser {
                name: Some("Updated Name"),
                email: None,
                password_hash: None,
            },
        )
        .await
        .expect("Failed to update user");
        assert_eq!(updated.name, "Updated Name");

        // Delete
        delete_user(&pool, user.id)
            .await
            .expect("Failed to delete user");

        // Verify deleted
        let result = get_user(&pool, user.id).await;
        assert!(matches!(result, Err(DbError::NotFound)));
    }
}
