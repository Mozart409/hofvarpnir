pub mod auth;
pub mod middleware;
pub mod oidc;
pub mod pages;

use std::sync::Arc;

use axum::Router;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use hof_api::AppState;
use hof_core::oidc::{OidcClient, OidcConfig};
use sqlx::PgPool;
use tower_sessions::SessionManagerLayer;
use tower_sessions_sqlx_store::PostgresStore;
use tracing::info;

/// Build the web frontend router with Maud + htmx routes.
///
/// Mount at `/` in the top-level application.
///
/// # Arguments
///
/// * `state` - The shared application state
/// * `oidc_client` - Optional OIDC client for SSO authentication
pub fn router(state: AppState, oidc_client: Option<&Arc<OidcClient>>) -> Router {
    let oidc_state = crate::oidc::OidcState {
        app: state.clone(),
        oidc_client: oidc_client.cloned(),
    };

    pages::router(state, oidc_client.is_some()).merge(crate::oidc::router(oidc_state))
}

/// Initialize OIDC client if configured.
///
/// Returns `Some(Arc<OidcClient>)` if OIDC is configured via environment variables,
/// `None` otherwise.
///
/// # Errors
///
/// Returns an error if OIDC is configured but discovery fails.
pub async fn init_oidc() -> Result<Option<Arc<OidcClient>>> {
    let Some(config) = OidcConfig::from_env() else {
        info!("OIDC not configured - skipping OIDC client initialization");
        return Ok(None);
    };

    info!(issuer = %config.issuer_url, "Initializing OIDC client");
    let client = OidcClient::discover(config).await?;
    info!("OIDC client initialized successfully");

    Ok(Some(Arc::new(client)))
}

/// Create the session layer for authentication.
///
/// Uses `PostgreSQL` as the session store.
///
/// # Errors
///
/// Returns an error if the session store cannot be configured or migrated.
pub async fn session_layer(pool: PgPool) -> Result<SessionManagerLayer<PostgresStore>> {
    let session_store = PostgresStore::new(pool)
        .with_schema_name("public")
        .map_err(|e| eyre!("invalid schema name: {e}"))?
        .with_table_name("sessions")
        .map_err(|e| eyre!("invalid table name: {e}"))?;

    // Run session store migrations (creates table if not exists)
    session_store.migrate().await?;

    Ok(SessionManagerLayer::new(session_store)
        .with_secure(false) // Set to true in production with HTTPS
        .with_http_only(true)
        .with_same_site(tower_sessions::cookie::SameSite::Lax))
}
