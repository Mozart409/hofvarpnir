//! User test builder.

use hof_core::{db, domain::user::User};
use sqlx::PgPool;
use ulid::Ulid;

/// Builder for creating test users.
pub struct UserBuilder {
    name: String,
    email: String,
}

impl UserBuilder {
    /// Create a new user builder with random defaults.
    #[must_use]
    pub fn new() -> Self {
        let id = Ulid::r#gen();
        Self {
            name: format!("Test User {id}"),
            email: format!("test_{id}@example.com"),
        }
    }

    /// Set the name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the email.
    #[must_use]
    pub fn email(mut self, email: impl Into<String>) -> Self {
        self.email = email.into();
        self
    }

    /// Build and insert the user into the database.
    pub async fn build(self, pool: &PgPool) -> User {
        db::create_user(
            pool,
            db::CreateUser {
                name: &self.name,
                email: &self.email,
                password_hash: Some("test_hash_not_used"),
            },
        )
        .await
        .expect("Failed to create test user")
    }
}

impl Default for UserBuilder {
    fn default() -> Self {
        Self::new()
    }
}
