//! OIDC client for provider interaction.

use std::ops::Deref;
use std::sync::Arc;

use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndUserEmail, EndUserName,
    EndUserPictureUrl, IssuerUrl, LocalizedClaim, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
    reqwest as oidc_reqwest,
};
use tracing::{debug, info};
use url::Url;

use super::config::OidcConfig;
use super::error::OidcError;
use super::identity::OidcClaims;

/// OIDC client for a single provider.
///
/// Created once at application startup via `discover()`.
/// Thread-safe and can be shared across request handlers.
#[derive(Clone)]
pub struct OidcClient {
    config: OidcConfig,
    provider_metadata: Arc<CoreProviderMetadata>,
    client_id: ClientId,
    client_secret: ClientSecret,
    http_client: oidc_reqwest::Client,
    end_session_endpoint: Option<Url>,
}

impl OidcClient {
    /// Discover provider metadata and create the client.
    ///
    /// Fetches the `OpenID` Connect discovery document from the provider
    /// and configures the client with the discovered endpoints.
    ///
    /// # Errors
    ///
    /// Returns `OidcError::DiscoveryFailed` if the provider cannot be reached
    /// or returns invalid metadata.
    pub async fn discover(config: OidcConfig) -> Result<Self, OidcError> {
        let issuer_url = IssuerUrl::new(config.issuer_url.clone())
            .map_err(|e| OidcError::DiscoveryFailed(format!("Invalid issuer URL: {e}")))?;

        info!(issuer = %config.issuer_url, "Discovering OIDC provider metadata");

        // Build HTTP client with appropriate settings
        let http_client = oidc_reqwest::ClientBuilder::new()
            .redirect(oidc_reqwest::redirect::Policy::none())
            .timeout(config.discovery_timeout)
            .build()
            .map_err(|e| OidcError::DiscoveryFailed(format!("Failed to build HTTP client: {e}")))?;

        let provider_metadata = CoreProviderMetadata::discover_async(issuer_url, &http_client)
            .await
            .map_err(|e| OidcError::DiscoveryFailed(format!("Discovery request failed: {e}")))?;

        // end_session_endpoint is optional - skip for now
        let end_session_endpoint = None;

        let client_id = ClientId::new(config.client_id.clone());
        let client_secret = ClientSecret::new(config.client_secret.clone());

        debug!(
            client_id = %config.client_id,
            end_session = ?end_session_endpoint,
            "OIDC client configured"
        );

        Ok(Self {
            config,
            provider_metadata: Arc::new(provider_metadata),
            client_id,
            client_secret,
            http_client,
            end_session_endpoint,
        })
    }

    /// Generate an authorization URL for the login flow.
    ///
    /// Returns the URL to redirect the user to, along with the CSRF token,
    /// nonce, and PKCE verifier that must be stored in the session.
    ///
    /// # Panics
    ///
    /// Panics if `callback_uri` is not a valid URL (this should never happen
    /// in practice as the callback URI is constructed internally).
    pub fn authorization_url(
        &self,
        callback_uri: &str,
    ) -> (Url, CsrfToken, Nonce, PkceCodeVerifier) {
        // `callback_uri` is built from validated OIDC configuration at startup.
        // Propagating instead would make this a fallible constructor and ripple
        // through every caller of `authorization_url`.
        #[allow(clippy::expect_used)]
        let callback =
            RedirectUrl::new(callback_uri.to_string()).expect("callback_uri should be a valid URL");

        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let client = CoreClient::from_provider_metadata(
            (*self.provider_metadata).clone(),
            self.client_id.clone(),
            Some(self.client_secret.clone()),
        )
        .set_redirect_uri(callback);

        let (url, csrf_token, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .set_pkce_challenge(pkce_challenge)
            .add_scopes(self.config.scopes.iter().map(|s| Scope::new(s.clone())))
            .url();

        debug!(url = %url, "Generated OIDC authorization URL");

        (url, csrf_token, nonce, pkce_verifier)
    }

    /// Exchange an authorization code for tokens and extract claims.
    ///
    /// # Errors
    ///
    /// Returns an error if token exchange fails or the ID token is invalid.
    pub async fn exchange_code(
        &self,
        code: &str,
        callback_uri: &str,
        pkce_verifier: PkceCodeVerifier,
        expected_nonce: &Nonce,
    ) -> Result<OidcClaims, OidcError> {
        let callback = RedirectUrl::new(callback_uri.to_string())
            .map_err(|e| OidcError::TokenExchangeFailed(format!("Invalid redirect URI: {e}")))?;

        let client = CoreClient::from_provider_metadata(
            (*self.provider_metadata).clone(),
            self.client_id.clone(),
            Some(self.client_secret.clone()),
        )
        .set_redirect_uri(callback);

        let token_response = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .map_err(|e| {
                OidcError::TokenExchangeFailed(format!("Failed to create token request: {e}"))
            })?
            .set_pkce_verifier(pkce_verifier)
            .request_async(&self.http_client)
            .await
            .map_err(|e| OidcError::TokenExchangeFailed(format!("{e}")))?;

        let id_token = token_response
            .id_token()
            .ok_or_else(|| OidcError::InvalidToken("No ID token in response".to_string()))?;

        // Verify and extract claims
        let claims = id_token
            .claims(&client.id_token_verifier(), expected_nonce)
            .map_err(|e| OidcError::InvalidToken(format!("Token verification failed: {e}")))?;

        let issuer = claims.issuer().as_str().to_string();
        let subject = claims.subject().as_str().to_string();

        // Extract email
        let email: String = claims
            .email()
            .map(|e: &EndUserEmail| e.deref().clone())
            .ok_or_else(|| OidcError::MissingClaim("email".to_string()))?;

        // Extract name - get the unlocalized value
        let name: Option<String> = claims
            .name()
            .and_then(|n: &LocalizedClaim<EndUserName>| n.get(None))
            .map(|n: &EndUserName| n.deref().clone());

        // Extract picture - get the unlocalized value
        let picture: Option<String> = claims
            .picture()
            .and_then(|p: &LocalizedClaim<EndUserPictureUrl>| p.get(None))
            .map(|p: &EndUserPictureUrl| p.as_str().to_string());

        debug!(
            issuer = %issuer,
            subject = %subject,
            email = %email,
            "Extracted OIDC claims"
        );

        Ok(OidcClaims {
            issuer,
            subject,
            email,
            name,
            picture,
        })
    }

    /// Get the end session URL for RP-initiated logout.
    ///
    /// Returns `None` if the provider doesn't support `end_session_endpoint`
    /// or if `logout_redirect` is disabled in config.
    #[must_use]
    pub fn end_session_url(
        &self,
        id_token_hint: Option<&str>,
        post_logout_redirect: &str,
    ) -> Option<Url> {
        if !self.config.logout_redirect {
            return None;
        }

        let endpoint = self.end_session_endpoint.as_ref()?;
        let mut url = endpoint.clone();

        {
            let mut query = url.query_pairs_mut();
            if let Some(token) = id_token_hint {
                query.append_pair("id_token_hint", token);
            }
            query.append_pair("post_logout_redirect_uri", post_logout_redirect);
        }

        Some(url)
    }

    /// Get the configured issuer URL.
    #[must_use]
    pub fn issuer_url(&self) -> &str {
        &self.config.issuer_url
    }

    /// Check if auto-provisioning is enabled.
    #[must_use]
    pub const fn auto_provision(&self) -> bool {
        self.config.auto_provision
    }
}
