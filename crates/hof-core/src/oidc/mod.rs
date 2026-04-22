//! OIDC authentication support.
//!
//! Provides `OpenID` Connect authentication for single-provider SSO.
//! Configuration is loaded from environment variables at runtime.
//!
//! # Configuration
//!
//! Set these environment variables to enable OIDC:
//!
//! - `OIDC_ISSUER`: Provider issuer URL (required)
//! - `OIDC_CLIENT_ID`: `OAuth2` client ID (required)
//! - `OIDC_CLIENT_SECRET`: `OAuth2` client secret (required)
//! - `OIDC_SCOPES`: Comma-separated scopes (default: "openid,profile,email")
//! - `OIDC_AUTO_PROVISION`: Create user on first login (default: true)
//! - `OIDC_REDIRECT_BASE_URL`: Override callback URL base
//! - `OIDC_LOGOUT_REDIRECT`: RP-initiated logout (default: false)
//! - `OIDC_DISCOVERY_TIMEOUT`: Discovery timeout in seconds (default: 30)
//!
//! # Usage
//!
//! ```ignore
//! use hof_core::oidc::{OidcConfig, OidcClient};
//!
//! // At startup, check if OIDC is configured
//! if let Some(config) = OidcConfig::from_env() {
//!     let client = OidcClient::discover(config).await?;
//!     // Store client in app state
//! }
//! ```

mod client;
mod config;
mod error;
mod identity;

pub use client::OidcClient;
pub use config::OidcConfig;
pub use error::OidcError;
pub use identity::{OidcClaims, OidcIdentity, OidcIdentityRow};

// Re-export types needed for OIDC flow handling
pub use openidconnect::{Nonce, PkceCodeVerifier};
