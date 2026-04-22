//! User domain type.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Authentication and ownership boundary.
///
/// Note: `password_hash` is intentionally excluded from serialization
/// to prevent accidental exposure in API responses.
///
/// `password_hash` is `None` for OIDC-only users who haven't set a password.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Ulid,
    pub email: String,
    pub name: String,
    #[serde(skip_serializing)]
    pub password_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl User {
    /// Check if this user has a password set.
    #[must_use]
    pub fn has_password(&self) -> bool {
        self.password_hash.as_ref().is_some_and(|h| !h.is_empty())
    }
}

/// Database row representation for User (with String id).
#[derive(Debug, sqlx::FromRow)]
pub struct UserRow {
    pub id: String,
    pub email: String,
    pub name: String,
    pub password_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<UserRow> for User {
    type Error = ulid::DecodeError;

    fn try_from(row: UserRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Ulid::from_string(&row.id)?,
            email: row.email,
            name: row.name,
            password_hash: row.password_hash,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}
