//! User database operations.

use sqlx::postgres::PgPool;
use ulid::Ulid;

use super::DbError;
use crate::domain::user::{User, UserRow};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_pool, run_migrations};

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
