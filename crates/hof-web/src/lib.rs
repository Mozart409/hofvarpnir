pub mod auth;
pub mod middleware;
pub mod oidc;
pub mod pages;
pub mod runtime;

use std::sync::Arc;

use axum::Router;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use hof_api::AppState;
use hof_core::oidc::{OidcClient, OidcConfig};
use sqlx::PgPool;
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::PostgresStore;
use tracing::{info, warn};

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
/// The session lifetime is a sliding window sized by `SESSION_TIMEOUT_HOURS`:
/// an active session is extended on every request and an idle one lapses after
/// that many hours. This needs `always_save`: tower-sessions otherwise only
/// saves (and so only re-stamps the expiry of) a session whose data changed,
/// which here is little more than login, so the window would stay anchored
/// there. The cost is one store write per request carrying a non-empty
/// session. When the variable is unset, empty, unparseable or zero the
/// `tower-sessions` default applies instead — a browser-session cookie (no
/// `Max-Age`, so closing the browser logs you out) whose server-side record
/// lives two weeks past its last write.
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

    let layer = SessionManagerLayer::new(session_store)
        .with_secure(false) // Set to true in production with HTTPS
        .with_http_only(true)
        .with_same_site(tower_sessions::cookie::SameSite::Lax);

    Ok(match session_timeout_hours() {
        Some(hours) => layer
            .with_expiry(Expiry::OnInactivity(time::Duration::hours(i64::from(
                hours,
            ))))
            .with_always_save(true),
        None => layer,
    })
}

/// Session lifetime from `SESSION_TIMEOUT_HOURS`, in hours.
///
/// `None` means "keep the `tower-sessions` default": the variable is absent,
/// blank, zero or not a `u32` hour count. A bad value is logged and ignored
/// rather than fatal — the web tier should still start, and falling back to a
/// browser-session cookie is a far smaller problem than a server that will not.
///
/// `u32` (not `u64`) is deliberate: `time::Duration::hours` panics above
/// `i64::MAX / 3600`, and every `u32` hour count is far below that.
fn session_timeout_hours() -> Option<u32> {
    let raw = std::env::var("SESSION_TIMEOUT_HOURS").ok()?;
    match raw.trim().parse::<u32>() {
        Ok(0) => {
            warn!(
                "SESSION_TIMEOUT_HOURS=0 is not a session lifetime; keeping the tower-sessions default"
            );
            None
        }
        Ok(hours) => Some(hours),
        Err(e) => {
            warn!(value = %raw, error = %e, "invalid SESSION_TIMEOUT_HOURS; keeping the tower-sessions default");
            None
        }
    }
}
