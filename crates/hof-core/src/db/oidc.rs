//! Database operations for OIDC identities.

use sqlx::PgPool;
use ulid::Ulid;

use crate::oidc::{OidcIdentity, OidcIdentityRow};

/// Look up an OIDC identity by issuer and subject.
///
/// Returns `None` if no identity exists for this issuer/subject combination.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub async fn get_oidc_identity_by_subject(
    pool: &PgPool,
    issuer: &str,
    subject: &str,
) -> Result<Option<OidcIdentity>, sqlx::Error> {
    let row: Option<OidcIdentityRow> = sqlx::query_as(
        r"
        SELECT id, user_id, issuer, subject, email, name, picture, created_at, updated_at
        FROM oidc_identities
        WHERE issuer = $1 AND subject = $2
        ",
    )
    .bind(issuer)
    .bind(subject)
    .fetch_optional(pool)
    .await?;

    row.map(OidcIdentity::try_from)
        .transpose()
        .map_err(|e| sqlx::Error::Decode(Box::new(e)))
}

/// Create a new OIDC identity link.
///
/// # Errors
///
/// Returns an error if the insert fails (e.g., duplicate issuer/subject).
pub async fn create_oidc_identity(
    pool: &PgPool,
    user_id: Ulid,
    issuer: &str,
    subject: &str,
    email: Option<&str>,
    name: Option<&str>,
    picture: Option<&str>,
) -> Result<OidcIdentity, sqlx::Error> {
    let id = Ulid::new();

    let row: OidcIdentityRow = sqlx::query_as(
        r"
        INSERT INTO oidc_identities (id, user_id, issuer, subject, email, name, picture)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, user_id, issuer, subject, email, name, picture, created_at, updated_at
        ",
    )
    .bind(id.to_string())
    .bind(user_id.to_string())
    .bind(issuer)
    .bind(subject)
    .bind(email)
    .bind(name)
    .bind(picture)
    .fetch_one(pool)
    .await?;

    OidcIdentity::try_from(row).map_err(|e| sqlx::Error::Decode(Box::new(e)))
}

/// Get all OIDC identities for a user.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub async fn get_oidc_identities_for_user(
    pool: &PgPool,
    user_id: Ulid,
) -> Result<Vec<OidcIdentity>, sqlx::Error> {
    let rows: Vec<OidcIdentityRow> = sqlx::query_as(
        r"
        SELECT id, user_id, issuer, subject, email, name, picture, created_at, updated_at
        FROM oidc_identities
        WHERE user_id = $1
        ORDER BY created_at DESC
        ",
    )
    .bind(user_id.to_string())
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(OidcIdentity::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| sqlx::Error::Decode(Box::new(e)))
}

/// Delete an OIDC identity.
///
/// Only deletes if the identity belongs to the specified user (ownership check).
///
/// Returns `true` if a row was deleted, `false` if not found or not owned.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub async fn delete_oidc_identity(
    pool: &PgPool,
    id: Ulid,
    user_id: Ulid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        r"
        DELETE FROM oidc_identities
        WHERE id = $1 AND user_id = $2
        ",
    )
    .bind(id.to_string())
    .bind(user_id.to_string())
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Update cached claims for an OIDC identity.
///
/// Called on each login to keep cached email/name/picture up to date.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub async fn update_oidc_identity_claims(
    pool: &PgPool,
    id: Ulid,
    email: Option<&str>,
    name: Option<&str>,
    picture: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        UPDATE oidc_identities
        SET email = $2, name = $3, picture = $4
        WHERE id = $1
        ",
    )
    .bind(id.to_string())
    .bind(email)
    .bind(name)
    .bind(picture)
    .execute(pool)
    .await?;

    Ok(())
}

/// Check if a user has any OIDC identities linked.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub async fn user_has_oidc_identity(pool: &PgPool, user_id: Ulid) -> Result<bool, sqlx::Error> {
    let count: (i64,) = sqlx::query_as(
        r"
        SELECT COUNT(*) FROM oidc_identities WHERE user_id = $1
        ",
    )
    .bind(user_id.to_string())
    .fetch_one(pool)
    .await?;

    Ok(count.0 > 0)
}
