//! API key test builder.

use chrono::{DateTime, Duration, Utc};
use hof_core::{
    auth::generate_api_key,
    db::{self, CreateApiKey},
    domain::api_key::ApiKeyScope,
};
use sqlx::PgPool;
use ulid::Ulid;

/// Result of building an API key - includes the token for use in tests.
pub struct TestApiKey {
    /// The full token to use in Authorization header.
    pub token: String,
    /// The API key ID.
    pub id: Ulid,
    /// The scopes granted to this key.
    pub scopes: Vec<ApiKeyScope>,
}

impl TestApiKey {
    /// Format the token as a Bearer header value.
    #[must_use]
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

/// Builder for creating test API keys.
pub struct ApiKeyBuilder {
    user_id: Ulid,
    name: String,
    scopes: Vec<ApiKeyScope>,
    expires_at: Option<DateTime<Utc>>,
}

impl ApiKeyBuilder {
    /// Create a new API key builder for a user.
    #[must_use]
    pub fn new(user_id: Ulid) -> Self {
        let id = Ulid::r#gen();
        Self {
            user_id,
            name: format!("test_key_{id}"),
            scopes: vec![ApiKeyScope::Read],
            expires_at: None,
        }
    }

    /// Set the key name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the scopes.
    #[must_use]
    pub fn scopes(mut self, scopes: Vec<ApiKeyScope>) -> Self {
        self.scopes = scopes;
        self
    }

    /// Create a read-only key.
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.scopes = vec![ApiKeyScope::Read];
        self
    }

    /// Create a read-write key.
    #[must_use]
    pub fn read_write(mut self) -> Self {
        self.scopes = vec![ApiKeyScope::Read, ApiKeyScope::Write];
        self
    }

    /// Create a key with delete scope only.
    #[must_use]
    pub fn delete_only(mut self) -> Self {
        self.scopes = vec![ApiKeyScope::Delete];
        self
    }

    /// Create a key with all scopes.
    #[must_use]
    pub fn full_access(mut self) -> Self {
        self.scopes = vec![ApiKeyScope::Read, ApiKeyScope::Write, ApiKeyScope::Delete];
        self
    }

    /// Set expiration time.
    #[must_use]
    pub fn expires_at(mut self, expires_at: DateTime<Utc>) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Create an already-expired key (for testing expiration).
    #[must_use]
    pub fn expired(mut self) -> Self {
        self.expires_at = Some(Utc::now() - Duration::hours(1));
        self
    }

    /// Build and insert the API key into the database.
    pub async fn build(self, pool: &PgPool) -> TestApiKey {
        let generated = generate_api_key();

        let api_key = db::create_api_key(
            pool,
            CreateApiKey {
                user_id: self.user_id,
                name: &self.name,
                prefix: &generated.prefix,
                key_hash: &generated.hash,
                scopes: &self.scopes,
                expires_at: self.expires_at,
            },
        )
        .await
        .expect("Failed to create test API key");

        TestApiKey {
            token: generated.token,
            id: api_key.id,
            scopes: self.scopes,
        }
    }
}
