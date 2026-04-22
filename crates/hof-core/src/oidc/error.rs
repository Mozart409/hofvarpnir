//! OIDC authentication errors.

/// Errors that can occur during OIDC authentication.
#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    /// OIDC is not configured (missing env vars).
    #[error("OIDC not configured")]
    NotConfigured,

    /// Failed to discover provider metadata.
    #[error("Discovery failed: {0}")]
    DiscoveryFailed(String),

    /// State parameter mismatch (CSRF protection).
    #[error("Invalid state parameter")]
    InvalidState,

    /// Nonce mismatch (replay protection).
    #[error("Invalid nonce")]
    InvalidNonce,

    /// OIDC flow has expired (> 5 minutes).
    #[error("OIDC flow expired")]
    FlowExpired,

    /// Failed to exchange authorization code for tokens.
    #[error("Token exchange failed: {0}")]
    TokenExchangeFailed(String),

    /// ID token validation failed.
    #[error("Invalid ID token: {0}")]
    InvalidToken(String),

    /// Required claim is missing from ID token.
    #[error("Missing required claim: {0}")]
    MissingClaim(String),

    /// Account not found and auto-provisioning is disabled.
    #[error("Account not found and auto-provision disabled")]
    AccountNotFound,

    /// Database error during OIDC operations.
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
}
