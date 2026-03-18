//! Authentication middleware and session management.

use axum::{
    extract::FromRequestParts,
    http::request::Parts,
    response::{IntoResponse, Redirect, Response},
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use ulid::Ulid;

/// Session key for storing the authenticated user ID.
const USER_ID_KEY: &str = "user_id";

/// Authenticated user extracted from session.
///
/// Use this extractor in route handlers that require authentication.
/// If no valid session exists, the request will be redirected to `/login`.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Ulid,
}

/// Session data stored for authenticated users.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub user_id: String,
}

impl AuthUser {
    /// Store user ID in session after successful login.
    ///
    /// # Errors
    ///
    /// Returns an error if session storage fails.
    pub async fn login(session: &Session, user_id: Ulid) -> Result<(), SessionError> {
        session
            .insert(USER_ID_KEY, user_id.to_string())
            .await
            .map_err(|_| SessionError::StorageFailed)?;
        Ok(())
    }

    /// Remove user from session (logout).
    ///
    /// # Errors
    ///
    /// Returns an error if session deletion fails.
    pub async fn logout(session: &Session) -> Result<(), SessionError> {
        session
            .flush()
            .await
            .map_err(|_| SessionError::StorageFailed)?;
        Ok(())
    }
}

/// Errors that can occur during session operations.
#[derive(Debug)]
pub enum SessionError {
    /// No valid session found.
    NotAuthenticated,
    /// Failed to store/retrieve session data.
    StorageFailed,
    /// Invalid user ID in session.
    InvalidUserId,
}

impl IntoResponse for SessionError {
    fn into_response(self) -> Response {
        // Redirect to login page for auth errors
        Redirect::to("/login").into_response()
    }
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = SessionError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        // Extract the session from the request
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|_| SessionError::NotAuthenticated)?;

        // Get user ID from session
        let user_id_str: String = session
            .get(USER_ID_KEY)
            .await
            .map_err(|_| SessionError::StorageFailed)?
            .ok_or(SessionError::NotAuthenticated)?;

        // Parse ULID
        let user_id = Ulid::from_string(&user_id_str).map_err(|_| SessionError::InvalidUserId)?;

        Ok(AuthUser { user_id })
    }
}

/// Optional auth extractor - doesn't reject if not authenticated.
///
/// Use this for pages that can work with or without authentication.
#[derive(Debug, Clone)]
pub struct MaybeAuthUser(pub Option<AuthUser>);

impl<S> FromRequestParts<S> for MaybeAuthUser
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth_user = AuthUser::from_request_parts(parts, state).await.ok();
        Ok(MaybeAuthUser(auth_user))
    }
}
