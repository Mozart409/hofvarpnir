use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use utoipa::ToSchema;

/// Scope defining what actions an API key can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "api_key_scope", rename_all = "lowercase")]
pub enum ApiKeyScope {
    Read,
    Write,
    Delete,
}

/// Event types for API key lifecycle audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "api_key_event_type", rename_all = "lowercase")]
pub enum ApiKeyEventType {
    Created,
    Rolled,
    Deleted,
}

/// An API key for authenticating API requests.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKey {
    pub id: Ulid,
    pub user_id: Ulid,
    pub name: String,
    /// Display prefix (e.g., `hof_sk_Ab3xY`) - never the full key.
    pub prefix: String,
    pub scopes: Vec<ApiKeyScope>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ApiKey {
    /// Check if the API key has expired.
    pub fn is_expired(&self) -> bool {
        self.expires_at.is_some_and(|expires| expires <= Utc::now())
    }

    /// Check if the API key has a specific scope.
    pub fn has_scope(&self, scope: ApiKeyScope) -> bool {
        self.scopes.contains(&scope)
    }
}

/// Database row representation for `ApiKey` (with String ids).
#[derive(Debug, sqlx::FromRow)]
pub struct ApiKeyRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub prefix: String,
    pub key_hash: String,
    pub scopes: Vec<ApiKeyScope>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub last_used_ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<ApiKeyRow> for ApiKey {
    type Error = ulid::DecodeError;

    fn try_from(row: ApiKeyRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Ulid::from_string(&row.id)?,
            user_id: Ulid::from_string(&row.user_id)?,
            name: row.name,
            prefix: row.prefix,
            scopes: row.scopes,
            expires_at: row.expires_at,
            last_used_at: row.last_used_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

/// An audit event for API key lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyEvent {
    pub id: Ulid,
    pub api_key_id: Ulid,
    pub user_id: Ulid,
    pub event_type: ApiKeyEventType,
    pub ip_address: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Database row representation for `ApiKeyEvent` (with String ids).
#[derive(Debug, sqlx::FromRow)]
pub struct ApiKeyEventRow {
    pub id: String,
    pub api_key_id: String,
    pub user_id: String,
    pub event_type: ApiKeyEventType,
    pub ip_address: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl TryFrom<ApiKeyEventRow> for ApiKeyEvent {
    type Error = ulid::DecodeError;

    fn try_from(row: ApiKeyEventRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Ulid::from_string(&row.id)?,
            api_key_id: Ulid::from_string(&row.api_key_id)?,
            user_id: Ulid::from_string(&row.user_id)?,
            event_type: row.event_type,
            ip_address: row.ip_address,
            created_at: row.created_at,
        })
    }
}
