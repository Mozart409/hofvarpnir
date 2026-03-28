//! Profile database operations.

use sqlx::postgres::PgPool;
use ulid::Ulid;

use super::DbError;
use crate::domain::profile::{Profile, ProfileRow, Quality};

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
