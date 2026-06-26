//! Vendored subset of `tower-sessions-sqlx-store` (postgres store only),
//! patched for sqlx 0.9. See this crate's `Cargo.toml` for details.

// Vendored third-party code: don't hold upstream's style to our lint config.
#![allow(clippy::all, clippy::pedantic, clippy::nursery)]

pub use sqlx;
use tower_sessions_core::session_store;

pub use self::postgres_store::PostgresStore;

mod postgres_store;

/// An error type for SQLx stores.
#[derive(thiserror::Error, Debug)]
pub enum SqlxStoreError {
    /// A variant to map `sqlx` errors.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    /// A variant to map `rmp_serde` encode errors.
    #[error(transparent)]
    Encode(#[from] rmp_serde::encode::Error),

    /// A variant to map `rmp_serde` decode errors.
    #[error(transparent)]
    Decode(#[from] rmp_serde::decode::Error),
}

impl From<SqlxStoreError> for session_store::Error {
    fn from(err: SqlxStoreError) -> Self {
        match err {
            SqlxStoreError::Sqlx(inner) => session_store::Error::Backend(inner.to_string()),
            SqlxStoreError::Decode(inner) => session_store::Error::Decode(inner.to_string()),
            SqlxStoreError::Encode(inner) => session_store::Error::Encode(inner.to_string()),
        }
    }
}
