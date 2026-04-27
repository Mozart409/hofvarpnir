//! OIDC identity domain type.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// An OIDC identity linked to a user account.
///
/// Stores the mapping between a user and their identity at an OIDC provider.
/// A user can have multiple OIDC identities (from different providers or
/// different accounts at the same provider in multi-provider setups).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcIdentity {
    /// Unique identifier (ULID).
    pub id: Ulid,
    /// User this identity belongs to.
    pub user_id: Ulid,
    /// OIDC issuer URL (e.g., `https://accounts.google.com`).
    pub issuer: String,
    /// OIDC subject claim (`sub`) - unique identifier at this issuer.
    pub subject: String,
    /// Email from ID token (cached for display).
    pub email: Option<String>,
    /// Name from ID token (cached for display).
    pub name: Option<String>,
    /// Avatar URL from ID token (cached for display).
    pub picture: Option<String>,
    /// When this identity was linked.
    pub created_at: DateTime<Utc>,
    /// When this identity was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Database row representation for `OidcIdentity`.
#[derive(Debug, sqlx::FromRow)]
pub struct OidcIdentityRow {
    pub id: String,
    pub user_id: String,
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub picture: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<OidcIdentityRow> for OidcIdentity {
    type Error = ulid::DecodeError;

    fn try_from(row: OidcIdentityRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Ulid::from_string(&row.id)?,
            user_id: Ulid::from_string(&row.user_id)?,
            issuer: row.issuer,
            subject: row.subject,
            email: row.email,
            name: row.name,
            picture: row.picture,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

/// Claims extracted from an OIDC ID token.
///
/// Used during the callback flow before creating/linking an identity.
#[derive(Debug, Clone)]
pub struct OidcClaims {
    /// Issuer URL.
    pub issuer: String,
    /// Subject (unique user ID at issuer).
    pub subject: String,
    /// Email address (required for Hofvarpnir).
    pub email: String,
    /// Display name.
    pub name: Option<String>,
    /// Avatar URL.
    pub picture: Option<String>,
}

impl OidcClaims {
    /// Extract the username portion of the email for fallback name.
    #[must_use]
    pub fn name_or_email_user(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| self.email.split('@').next().unwrap_or("User").to_string())
    }
}
