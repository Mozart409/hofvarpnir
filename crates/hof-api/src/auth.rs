//! API authentication via session or API key.
//!
//! Provides a unified `Auth` extractor that supports both session-based authentication
//! (for web UI users) and API key authentication (for programmatic access).

use axum::{
    Json,
    extract::FromRequestParts,
    http::{StatusCode, header::AUTHORIZATION, request::Parts},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use ulid::Ulid;
use utoipa::ToSchema;

use hof_core::{auth::hash_api_key, db, domain::api_key::ApiKeyScope};

use crate::AppState;

/// Session key for storing the authenticated user ID.
const USER_ID_KEY: &str = "user_id";

/// Bearer token prefix for API keys.
const BEARER_PREFIX: &str = "Bearer ";

/// Unified authentication for API endpoints.
///
/// Supports both session-based auth (web UI) and API key auth (programmatic access).
/// Session-authenticated users have full access; API key users are restricted by scopes.
#[derive(Debug, Clone)]
pub enum Auth {
    /// Session-based authentication (web UI users).
    Session { user_id: Ulid },
    /// API key authentication.
    ApiKey {
        user_id: Ulid,
        key_id: Ulid,
        scopes: Vec<ApiKeyScope>,
    },
}

impl Auth {
    /// Returns the authenticated user ID regardless of auth method.
    #[must_use]
    pub const fn user_id(&self) -> Ulid {
        match self {
            Self::Session { user_id } | Self::ApiKey { user_id, .. } => *user_id,
        }
    }

    /// Returns the API key ID if authenticated via API key.
    #[must_use]
    pub const fn key_id(&self) -> Option<Ulid> {
        match self {
            Self::ApiKey { key_id, .. } => Some(*key_id),
            Self::Session { .. } => None,
        }
    }

    /// Returns scopes if authenticated via API key, None for session auth.
    /// Session-authenticated users have full access (no scope restrictions).
    #[must_use]
    pub fn scopes(&self) -> Option<&[ApiKeyScope]> {
        match self {
            Self::ApiKey { scopes, .. } => Some(scopes),
            Self::Session { .. } => None,
        }
    }

    /// Require a specific scope. Returns `Ok(())` for session auth or
    /// if the API key has the required scope.
    ///
    /// # Errors
    ///
    /// Returns `ApiError::InsufficientScope` if the API key lacks the required scope.
    pub fn require_scope(&self, scope: ApiKeyScope) -> Result<(), ApiError> {
        match self.scopes() {
            None => Ok(()), // session auth: full access
            Some(scopes) if scopes.contains(&scope) => Ok(()),
            Some(_) => Err(ApiError::InsufficientScope(scope)),
        }
    }
}

/// API authentication errors.
#[derive(Debug)]
pub enum ApiError {
    /// No authentication provided.
    Unauthorized,
    /// API key not found or invalid.
    InvalidApiKey,
    /// API key has expired.
    ApiKeyExpired,
    /// API key missing required scope.
    InsufficientScope(ApiKeyScope),
    /// Internal error during authentication.
    Internal(String),
}

/// JSON error response body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiErrorResponse {
    pub error: String,
    pub message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, message) = match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Authentication required".to_string(),
            ),
            Self::InvalidApiKey => (
                StatusCode::UNAUTHORIZED,
                "invalid_api_key",
                "API key not found or invalid".to_string(),
            ),
            Self::ApiKeyExpired => (
                StatusCode::UNAUTHORIZED,
                "api_key_expired",
                "API key has expired".to_string(),
            ),
            Self::InsufficientScope(scope) => (
                StatusCode::FORBIDDEN,
                "insufficient_scope",
                format!("API key missing required scope: {scope:?}").to_lowercase(),
            ),
            Self::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", msg),
        };

        (
            status,
            Json(ApiErrorResponse {
                error: error.to_string(),
                message,
            }),
        )
            .into_response()
    }
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // Try session auth first (fast cookie lookup)
        if let Ok(session) = Session::from_request_parts(parts, state).await
            && let Ok(Some(user_id_str)) = session.get::<String>(USER_ID_KEY).await
            && let Ok(user_id) = Ulid::from_string(&user_id_str)
        {
            return Ok(Self::Session { user_id });
        }

        // No valid session - try API key from Authorization header
        let auth_header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok());

        let Some(header_value) = auth_header else {
            return Err(ApiError::Unauthorized);
        };

        // Must be Bearer token
        let Some(token) = header_value.strip_prefix(BEARER_PREFIX) else {
            return Err(ApiError::Unauthorized);
        };

        // Validate token format (should start with hof_sk_)
        if !token.starts_with("hof_sk_") {
            return Err(ApiError::InvalidApiKey);
        }

        // Hash the token and look up in database
        let key_hash = hash_api_key(token);
        let api_key = db::get_api_key_by_hash(&state.pool, &key_hash)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or(ApiError::InvalidApiKey)?;

        // Check expiration
        if api_key.is_expired() {
            return Err(ApiError::ApiKeyExpired);
        }

        // Spawn best-effort background task to update last_used
        let pool = state.pool.clone();
        let key_id = api_key.id;
        let ip = parts
            .headers
            .get("x-forwarded-for")
            .or_else(|| parts.headers.get("x-real-ip"))
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string);

        tokio::spawn(async move {
            db::touch_api_key_last_used(&pool, key_id, ip.as_deref()).await;
        });

        Ok(Self::ApiKey {
            user_id: api_key.user_id,
            key_id: api_key.id,
            scopes: api_key.scopes,
        })
    }
}

/// Optional auth extractor - doesn't reject if not authenticated.
///
/// Use this for endpoints that can work with or without authentication.
#[derive(Debug, Clone)]
pub struct MaybeAuth(pub Option<Auth>);

impl FromRequestParts<AppState> for MaybeAuth {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = Auth::from_request_parts(parts, state).await.ok();
        Ok(Self(auth))
    }
}
