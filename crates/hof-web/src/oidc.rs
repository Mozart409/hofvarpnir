//! OIDC authentication routes.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Query, State},
    response::{IntoResponse, Redirect},
    routing::get,
};
use chrono::{DateTime, Utc};
use hof_api::AppState;
use hof_core::{
    db,
    oidc::{Nonce, OidcClient, OidcConfig, PkceCodeVerifier},
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use tracing::{debug, error, info, warn};

use crate::auth::AuthUser;

/// Session key for OIDC flow state.
const OIDC_FLOW_KEY: &str = "oidc_flow";

/// Flow state expiration time.
const FLOW_EXPIRATION_SECS: i64 = 300; // 5 minutes

/// OIDC flow state stored in session.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OidcFlowState {
    /// CSRF token (state parameter).
    state: String,
    /// Replay protection.
    nonce: String,
    /// PKCE verifier (base64-encoded).
    pkce_verifier: String,
    /// Post-login redirect URL.
    return_to: Option<String>,
    /// Flow creation time (for expiration check).
    created_at: DateTime<Utc>,
}

/// Query parameters from OIDC callback.
#[derive(Debug, Deserialize)]
pub struct OidcCallbackQuery {
    /// Authorization code from provider.
    code: Option<String>,
    /// State parameter (CSRF token).
    state: Option<String>,
    /// Error code from provider.
    error: Option<String>,
    /// Error description from provider.
    error_description: Option<String>,
}

/// Extended app state with optional OIDC client.
pub struct OidcState {
    /// Inner app state.
    pub app: AppState,
    /// OIDC client (None if not configured).
    pub oidc_client: Option<Arc<OidcClient>>,
}

impl Clone for OidcState {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            oidc_client: self.oidc_client.clone(),
        }
    }
}

/// Create OIDC routes.
///
/// Returns empty router if OIDC is not configured.
pub fn router(state: OidcState) -> Router {
    if state.oidc_client.is_none() {
        return Router::new();
    }

    Router::new()
        .route("/auth/oidc/login", get(oidc_login))
        .route("/auth/oidc/callback", get(oidc_callback))
        .with_state(state)
}

/// Initiate OIDC login flow.
///
/// Generates PKCE, state, and nonce, stores in session, redirects to provider.
async fn oidc_login(State(state): State<OidcState>, session: Session) -> impl IntoResponse {
    let Some(client) = &state.oidc_client else {
        warn!("OIDC login attempted but not configured");
        return Redirect::to("/login?error=oidc_not_configured").into_response();
    };

    // Build callback URI
    let callback_uri = OidcConfig::from_env()
        .map_or_else(|| "/auth/oidc/callback".to_string(), |c| c.redirect_uri(""));

    // Generate authorization URL with PKCE
    let (auth_url, csrf_token, nonce, pkce_verifier) = client.authorization_url(&callback_uri);

    // Store flow state in session
    let flow_state = OidcFlowState {
        state: csrf_token.secret().clone(),
        nonce: nonce.secret().clone(),
        pkce_verifier: pkce_verifier.secret().clone(),
        return_to: None,
        created_at: Utc::now(),
    };

    if let Err(e) = session.insert(OIDC_FLOW_KEY, &flow_state).await {
        error!(error = ?e, "Failed to store OIDC flow state in session");
        return Redirect::to("/login?error=session_error").into_response();
    }

    debug!(url = %auth_url, "Redirecting to OIDC provider");
    Redirect::to(auth_url.as_str()).into_response()
}

/// Handle OIDC callback from provider.
///
/// Validates state, exchanges code for tokens, creates/links user.
// Allow longer function - this is a complex OIDC flow handler
#[allow(clippy::too_many_lines)]
async fn oidc_callback(
    State(state): State<OidcState>,
    session: Session,
    Query(params): Query<OidcCallbackQuery>,
) -> impl IntoResponse {
    let Some(client) = &state.oidc_client else {
        warn!("OIDC callback but not configured");
        return Redirect::to("/login?error=oidc_not_configured").into_response();
    };

    // Check for error from provider
    if let Some(error) = &params.error {
        warn!(
            error = %error,
            description = ?params.error_description,
            "OIDC provider returned error"
        );
        return Redirect::to("/login?error=provider_error").into_response();
    }

    // Validate required parameters
    let Some(code) = params.code else {
        warn!("OIDC callback missing code parameter");
        return Redirect::to("/login?error=missing_code").into_response();
    };

    let Some(returned_state) = params.state else {
        warn!("OIDC callback missing state parameter");
        return Redirect::to("/login?error=missing_state").into_response();
    };

    // Retrieve and validate flow state from session
    let flow_state: OidcFlowState = match session.get(OIDC_FLOW_KEY).await {
        Ok(Some(state)) => state,
        Ok(None) => {
            warn!("OIDC callback but no flow state in session");
            return Redirect::to("/login?error=no_flow_state").into_response();
        }
        Err(e) => {
            error!(error = ?e, "Failed to retrieve OIDC flow state");
            return Redirect::to("/login?error=session_error").into_response();
        }
    };

    // Remove flow state immediately (one-time use)
    let _ = session.remove::<OidcFlowState>(OIDC_FLOW_KEY).await;

    // Check expiration
    let elapsed = Utc::now().signed_duration_since(flow_state.created_at);
    if elapsed.num_seconds() > FLOW_EXPIRATION_SECS {
        warn!("OIDC flow state expired");
        return Redirect::to("/login?error=flow_expired").into_response();
    }

    // Validate state (CSRF protection)
    if returned_state != flow_state.state {
        warn!("OIDC state mismatch (possible CSRF attack)");
        return Redirect::to("/login?error=invalid_state").into_response();
    }

    // Build callback URI for token exchange
    let callback_uri = OidcConfig::from_env()
        .map_or_else(|| "/auth/oidc/callback".to_string(), |c| c.redirect_uri(""));

    // Exchange code for tokens
    let claims = match client
        .exchange_code(
            &code,
            &callback_uri,
            PkceCodeVerifier::new(flow_state.pkce_verifier),
            &Nonce::new(flow_state.nonce),
        )
        .await
    {
        Ok(claims) => claims,
        Err(e) => {
            warn!(error = ?e, "OIDC token exchange failed");
            return Redirect::to("/login?error=token_exchange_failed").into_response();
        }
    };

    // Look up existing OIDC identity
    let pool = &state.app.pool;
    let existing_identity =
        match db::get_oidc_identity_by_subject(pool, &claims.issuer, &claims.subject).await {
            Ok(identity) => identity,
            Err(e) => {
                error!(error = ?e, "Failed to look up OIDC identity");
                return Redirect::to("/login?error=database_error").into_response();
            }
        };

    let user_id = if let Some(identity) = existing_identity {
        // Existing OIDC identity - update cached claims and log in
        if let Err(e) = db::update_oidc_identity_claims(
            pool,
            identity.id,
            Some(&claims.email),
            claims.name.as_deref(),
            claims.picture.as_deref(),
        )
        .await
        {
            warn!(error = ?e, "Failed to update OIDC identity claims");
        }

        info!(
            user_id = %identity.user_id,
            issuer = %claims.issuer,
            subject = %claims.subject,
            "OIDC login successful (existing identity)"
        );

        identity.user_id
    } else {
        // New OIDC identity - check for existing user by email
        let user = match db::get_user_by_email(pool, &claims.email).await {
            Ok(user) => {
                // Link OIDC identity to existing user
                info!(
                    user_id = %user.id,
                    email = %claims.email,
                    issuer = %claims.issuer,
                    "Linking OIDC identity to existing user"
                );
                user
            }
            Err(db::DbError::NotFound) => {
                // Check if auto-provisioning is enabled
                if !client.auto_provision() {
                    warn!(
                        email = %claims.email,
                        issuer = %claims.issuer,
                        "OIDC user not found and auto-provisioning disabled"
                    );
                    return Redirect::to("/login?error=account_not_found").into_response();
                }

                // Auto-provision new user
                let name = claims.name.clone().unwrap_or_else(|| {
                    claims.email.split('@').next().unwrap_or("User").to_string()
                });

                match db::create_user(
                    pool,
                    db::CreateUser {
                        email: &claims.email,
                        name: &name,
                        password_hash: None, // OIDC-only user
                    },
                )
                .await
                {
                    Ok(user) => {
                        info!(
                            user_id = %user.id,
                            email = %claims.email,
                            issuer = %claims.issuer,
                            "OIDC user auto-provisioned"
                        );
                        user
                    }
                    Err(e) => {
                        error!(error = ?e, email = %claims.email, "Failed to create user");
                        return Redirect::to("/login?error=user_creation_failed").into_response();
                    }
                }
            }
            Err(e) => {
                error!(error = ?e, "Failed to look up user by email");
                return Redirect::to("/login?error=database_error").into_response();
            }
        };

        // Create OIDC identity link
        if let Err(e) = db::create_oidc_identity(
            pool,
            user.id,
            &claims.issuer,
            &claims.subject,
            Some(&claims.email),
            claims.name.as_deref(),
            claims.picture.as_deref(),
        )
        .await
        {
            error!(error = ?e, "Failed to create OIDC identity");
            return Redirect::to("/login?error=identity_creation_failed").into_response();
        }

        user.id
    };

    // Create session
    if let Err(e) = AuthUser::login(&session, user_id).await {
        error!(error = ?e, "Failed to create session after OIDC login");
        return Redirect::to("/login?error=session_error").into_response();
    }

    // Redirect to dashboard or return_to URL
    let redirect_to = flow_state.return_to.as_deref().unwrap_or("/dashboard");
    Redirect::to(redirect_to).into_response()
}

/// Check if OIDC is configured (for conditional UI rendering).
#[must_use]
pub fn is_configured() -> bool {
    OidcConfig::is_configured()
}
