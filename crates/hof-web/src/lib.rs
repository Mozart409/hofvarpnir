pub mod auth;
pub mod pages;

use axum::Router;
use hof_api::AppState;
use sqlx::PgPool;
use tower_sessions::SessionManagerLayer;
use tower_sessions_sqlx_store::PostgresStore;

/// Build the web frontend router with Maud + htmx routes.
///
/// Mount at `/` in the top-level application.
pub fn router(state: AppState) -> Router {
    pages::router(state)
}

/// Create the session layer for authentication.
///
/// Uses `PostgreSQL` as the session store.
///
/// # Panics
///
/// Panics if the session store cannot be configured or migrated.
pub async fn session_layer(pool: PgPool) -> SessionManagerLayer<PostgresStore> {
    let session_store = PostgresStore::new(pool)
        .with_schema_name("public")
        .expect("Invalid schema name")
        .with_table_name("sessions")
        .expect("Invalid table name");

    // Run session store migrations (creates table if not exists)
    session_store
        .migrate()
        .await
        .expect("Failed to migrate session store");

    SessionManagerLayer::new(session_store)
        .with_secure(false) // Set to true in production with HTTPS
        .with_http_only(true)
        .with_same_site(tower_sessions::cookie::SameSite::Lax)
}
