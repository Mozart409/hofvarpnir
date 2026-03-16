//! User domain type.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Authentication and ownership boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Ulid,
    pub email: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Database row representation for User (with String id).
#[derive(Debug, sqlx::FromRow)]
pub struct UserRow {
    pub id: String,
    pub email: String,
    pub name: String,
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
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}
