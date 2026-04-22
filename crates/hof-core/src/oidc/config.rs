//! OIDC provider configuration from environment variables.

use std::time::Duration;

/// OIDC provider configuration loaded from environment variables.
///
/// Required env vars (OIDC disabled if not set):
/// - `OIDC_ISSUER`: Provider issuer URL
/// - `OIDC_CLIENT_ID`: Client ID from provider
/// - `OIDC_CLIENT_SECRET`: Client secret from provider
///
/// Optional env vars:
/// - `OIDC_SCOPES`: Comma-separated scopes (default: "openid,profile,email")
/// - `OIDC_AUTO_PROVISION`: Create user on first login (default: true)
/// - `OIDC_REDIRECT_BASE_URL`: Override callback URL base
/// - `OIDC_LOGOUT_REDIRECT`: Redirect to provider on logout (default: false)
/// - `OIDC_DISCOVERY_TIMEOUT`: Discovery timeout in seconds (default: 30)
#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// OIDC issuer URL (e.g., `https://accounts.google.com`).
    pub issuer_url: String,
    /// `OAuth2` client ID.
    pub client_id: String,
    /// `OAuth2` client secret.
    pub client_secret: String,
    /// Requested scopes.
    pub scopes: Vec<String>,
    /// Create user on first OIDC login if not found.
    pub auto_provision: bool,
    /// Override for redirect URI base (e.g., `https://hof.example.com`).
    pub redirect_base_url: Option<String>,
    /// Redirect to provider's `end_session_endpoint` on logout.
    pub logout_redirect: bool,
    /// HTTP timeout for discovery and token exchange.
    pub discovery_timeout: Duration,
}

impl OidcConfig {
    /// Load configuration from environment variables.
    ///
    /// Returns `None` if required env vars are not set (OIDC disabled).
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let issuer_url = std::env::var("OIDC_ISSUER").ok()?;
        let client_id = std::env::var("OIDC_CLIENT_ID").ok()?;
        let client_secret = std::env::var("OIDC_CLIENT_SECRET").ok()?;

        let scopes = std::env::var("OIDC_SCOPES").map_or_else(
            |_| {
                vec![
                    "openid".to_string(),
                    "profile".to_string(),
                    "email".to_string(),
                ]
            },
            |s| s.split(',').map(|s| s.trim().to_string()).collect(),
        );

        let auto_provision = std::env::var("OIDC_AUTO_PROVISION")
            .map_or(true, |s| s.eq_ignore_ascii_case("true") || s == "1");

        let redirect_base_url = std::env::var("OIDC_REDIRECT_BASE_URL").ok();

        let logout_redirect = std::env::var("OIDC_LOGOUT_REDIRECT")
            .is_ok_and(|s| s.eq_ignore_ascii_case("true") || s == "1");

        let discovery_timeout = std::env::var("OIDC_DISCOVERY_TIMEOUT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map_or(Duration::from_secs(30), Duration::from_secs);

        Some(Self {
            issuer_url,
            client_id,
            client_secret,
            scopes,
            auto_provision,
            redirect_base_url,
            logout_redirect,
            discovery_timeout,
        })
    }

    /// Build the redirect URI for OIDC callbacks.
    ///
    /// Uses `redirect_base_url` if set, otherwise derives from the request base.
    #[must_use]
    pub fn redirect_uri(&self, request_base: &str) -> String {
        let base = self.redirect_base_url.as_deref().unwrap_or(request_base);
        format!("{}/auth/oidc/callback", base.trim_end_matches('/'))
    }

    /// Check if OIDC is configured (env vars present).
    #[must_use]
    pub fn is_configured() -> bool {
        std::env::var("OIDC_ISSUER").is_ok()
            && std::env::var("OIDC_CLIENT_ID").is_ok()
            && std::env::var("OIDC_CLIENT_SECRET").is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redirect_uri_with_base() {
        let config = OidcConfig {
            issuer_url: "https://auth.example.com".to_string(),
            client_id: "test".to_string(),
            client_secret: "secret".to_string(),
            scopes: vec!["openid".to_string()],
            auto_provision: true,
            redirect_base_url: Some("https://hof.example.com".to_string()),
            logout_redirect: false,
            discovery_timeout: Duration::from_secs(30),
        };

        assert_eq!(
            config.redirect_uri("https://localhost:8080"),
            "https://hof.example.com/auth/oidc/callback"
        );
    }

    #[test]
    fn test_redirect_uri_from_request() {
        let config = OidcConfig {
            issuer_url: "https://auth.example.com".to_string(),
            client_id: "test".to_string(),
            client_secret: "secret".to_string(),
            scopes: vec!["openid".to_string()],
            auto_provision: true,
            redirect_base_url: None,
            logout_redirect: false,
            discovery_timeout: Duration::from_secs(30),
        };

        assert_eq!(
            config.redirect_uri("https://localhost:8080/"),
            "https://localhost:8080/auth/oidc/callback"
        );
    }
}
