//! OIDC client for provider interaction.
//!
//! The OAuth 2.0 side (authorization URL, PKCE, code exchange) is `oauth2`;
//! discovery and the provider's signing keys are fetched here, and the ID
//! token is validated by [`super::verify`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::Algorithm;
use jsonwebtoken::jwk::JwkSet;
use oauth2::basic::{
    BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse,
    BasicTokenType,
};
use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet,
    EndpointSet, ExtraTokenFields, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    StandardRevocableToken, StandardTokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, instrument};
use url::Url;

use super::config::OidcConfig;
use super::error::OidcError;
use super::identity::OidcClaims;
use super::verify::{Expected, Rejection, accepted_algorithms, unix_now, verify_id_token};

/// The JWKS is fetched again for an unknown key id at most this often, so
/// tokens naming made-up keys cannot make us hammer the provider.
const JWKS_REFETCH_INTERVAL: Duration = Duration::from_secs(60);

/// A login nonce (replay protection): sent with the authorization request and
/// required back in the ID token.
#[derive(Clone)]
pub struct Nonce(String);

impl std::fmt::Debug for Nonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Nonce([redacted])")
    }
}

impl Nonce {
    /// Wrap a nonce stored with the login flow.
    #[must_use]
    pub const fn new(nonce: String) -> Self {
        Self(nonce)
    }

    /// A fresh nonce: 128 random bits, base64url, like the CSRF state.
    #[must_use]
    pub fn new_random() -> Self {
        Self(CsrfToken::new_random().into_secret())
    }

    #[must_use]
    pub const fn secret(&self) -> &String {
        &self.0
    }
}

/// The extra field OIDC adds to the OAuth 2.0 token response.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct IdTokenFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id_token: Option<String>,
}

impl ExtraTokenFields for IdTokenFields {}

/// An OAuth 2.0 client with the authorization and token endpoints set, whose
/// token response carries the ID token.
type FlowClient = oauth2::Client<
    BasicErrorResponse,
    StandardTokenResponse<IdTokenFields, BasicTokenType>,
    BasicTokenIntrospectionResponse,
    StandardRevocableToken,
    BasicRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
>;

/// The parts of the provider's discovery document we use.
#[derive(Deserialize)]
struct ProviderMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    end_session_endpoint: Option<String>,
    #[serde(default)]
    id_token_signing_alg_values_supported: Vec<String>,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Vec<String>,
}

/// The provider's signing keys, and when they were last fetched (or tried).
struct Keys {
    set: JwkSet,
    fetched: Instant,
}

/// OIDC client for a single provider.
///
/// Created once at application startup via `discover()`.
/// Thread-safe and can be shared across request handlers.
#[derive(Clone)]
pub struct OidcClient {
    config: OidcConfig,
    client: FlowClient,
    /// The issuer as the provider names itself (equal to the configured one).
    issuer: String,
    /// The ID-token algorithms the provider advertises and we accept.
    algorithms: Vec<Algorithm>,
    jwks_uri: String,
    keys: Arc<RwLock<Keys>>,
    http_client: reqwest::Client,
    end_session_endpoint: Option<Url>,
}

impl OidcClient {
    /// Discover provider metadata and create the client.
    ///
    /// Fetches the `OpenID` Connect discovery document from the provider,
    /// checks that it names the configured issuer, and fetches the provider's
    /// signing keys.
    ///
    /// # Errors
    ///
    /// Returns `OidcError::DiscoveryFailed` if the provider cannot be reached
    /// or returns invalid metadata.
    #[instrument(skip_all, fields(otel.kind = "client", issuer = %config.issuer_url))]
    pub async fn discover(config: OidcConfig) -> Result<Self, OidcError> {
        let failed = |what: String| OidcError::DiscoveryFailed(what);
        let issuer_url = Url::parse(&config.issuer_url)
            .map_err(|e| failed(format!("Invalid issuer URL: {e}")))?;
        if !matches!(issuer_url.scheme(), "http" | "https") {
            return Err(failed(format!(
                "Invalid issuer URL: {} is not http(s)",
                config.issuer_url
            )));
        }

        info!(issuer = %config.issuer_url, "Discovering OIDC provider metadata");

        // No redirects: the discovery, keys and token endpoints never need
        // one, and following them would let the provider point us elsewhere.
        let http_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.discovery_timeout)
            .build()
            .map_err(|e| failed(format!("Failed to build HTTP client: {e}")))?;

        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            config.issuer_url.trim_end_matches('/')
        );
        let metadata: ProviderMetadata = get_json(&http_client, &discovery_url)
            .await
            .map_err(|e| failed(format!("Discovery request failed: {e}")))?;
        // The provider must name itself exactly as configured (OIDC Discovery
        // 4.3); its ID tokens carry the same string in `iss`.
        if metadata.issuer != config.issuer_url {
            return Err(failed(format!(
                "The provider's issuer is {:?}, not the configured {:?}",
                metadata.issuer, config.issuer_url
            )));
        }
        let algorithms = accepted_algorithms(&metadata.id_token_signing_alg_values_supported);
        if algorithms.is_empty() {
            return Err(failed(format!(
                "The provider signs ID tokens with none of the accepted algorithms: {:?}",
                metadata.id_token_signing_alg_values_supported
            )));
        }
        let set = get_json(&http_client, &metadata.jwks_uri)
            .await
            .map_err(|e| failed(format!("Fetching the signing keys failed: {e}")))?;
        let end_session_endpoint = metadata
            .end_session_endpoint
            .as_deref()
            .map(Url::parse)
            .transpose()
            .map_err(|e| failed(format!("Invalid end_session_endpoint: {e}")))?;

        // client_secret_basic is the default (OIDC Discovery 3); use the
        // request body only for a provider that does not offer it.
        let methods = &metadata.token_endpoint_auth_methods_supported;
        let auth_type = if !methods.is_empty()
            && !methods.iter().any(|m| m == "client_secret_basic")
            && methods.iter().any(|m| m == "client_secret_post")
        {
            AuthType::RequestBody
        } else {
            AuthType::BasicAuth
        };
        let client = oauth2::Client::new(ClientId::new(config.client_id.clone()))
            .set_client_secret(ClientSecret::new(config.client_secret.clone()))
            .set_auth_type(auth_type)
            .set_auth_uri(
                AuthUrl::new(metadata.authorization_endpoint)
                    .map_err(|e| failed(format!("Invalid authorization_endpoint: {e}")))?,
            )
            .set_token_uri(
                TokenUrl::new(metadata.token_endpoint)
                    .map_err(|e| failed(format!("Invalid token_endpoint: {e}")))?,
            );

        debug!(
            client_id = %config.client_id,
            end_session = ?end_session_endpoint,
            algorithms = ?algorithms,
            "OIDC client configured"
        );

        Ok(Self {
            config,
            client,
            issuer: metadata.issuer,
            algorithms,
            jwks_uri: metadata.jwks_uri,
            keys: Arc::new(RwLock::new(Keys {
                set,
                fetched: Instant::now(),
            })),
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
        let nonce = Nonce::new_random();

        let mut request = self
            .client
            .authorize_url(CsrfToken::new_random)
            .set_redirect_uri(std::borrow::Cow::Owned(callback))
            .add_scope(Scope::new("openid".to_string()))
            .add_extra_param("nonce", nonce.secret().clone())
            .set_pkce_challenge(pkce_challenge);
        // `openid` is always requested above, so skip it here to avoid sending
        // it twice.
        for scope in self.config.scopes.iter().filter(|s| *s != "openid") {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        let (url, csrf_token) = request.url();

        debug!(url = %url, "Generated OIDC authorization URL");

        (url, csrf_token, nonce, pkce_verifier)
    }

    /// Exchange an authorization code for tokens and extract claims.
    ///
    /// # Errors
    ///
    /// Returns an error if token exchange fails or the ID token is invalid.
    #[instrument(skip_all, fields(otel.kind = "client"))]
    pub async fn exchange_code(
        &self,
        code: &str,
        callback_uri: &str,
        pkce_verifier: PkceCodeVerifier,
        expected_nonce: &Nonce,
    ) -> Result<OidcClaims, OidcError> {
        let callback = RedirectUrl::new(callback_uri.to_string())
            .map_err(|e| OidcError::TokenExchangeFailed(format!("Invalid redirect URI: {e}")))?;

        // The closure owns its client, so the future oauth2 builds from it
        // borrows nothing and stays `Send` for axum.
        let http = |request| send(self.http_client.clone(), request);
        let token_response = self
            .client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .set_redirect_uri(std::borrow::Cow::Owned(callback))
            .set_pkce_verifier(pkce_verifier)
            .request_async(&http)
            .await
            .map_err(|e| OidcError::TokenExchangeFailed(format!("{e}")))?;

        let id_token = token_response
            .extra_fields()
            .id_token
            .as_deref()
            .ok_or_else(|| OidcError::InvalidToken("No ID token in response".to_string()))?;

        let identity = self.verify(id_token, expected_nonce.secret()).await?;
        let email = identity
            .email
            .ok_or_else(|| OidcError::MissingClaim("email".to_string()))?;

        debug!(
            issuer = %identity.issuer,
            subject = %identity.subject,
            email = %email,
            "Extracted OIDC claims"
        );

        Ok(OidcClaims {
            issuer: identity.issuer,
            subject: identity.subject,
            email,
            name: identity.name,
            picture: identity.picture,
        })
    }

    /// [`verify_id_token`] against the cached keys. A token signed with a key
    /// we do not know yet may follow a key rotation, so the keys are fetched
    /// again (at most every [`JWKS_REFETCH_INTERVAL`]) and the token checked
    /// once more.
    async fn verify(
        &self,
        token: &str,
        nonce: &str,
    ) -> Result<super::verify::VerifiedIdentity, OidcError> {
        let expected = Expected {
            issuer: &self.issuer,
            client_id: &self.config.client_id,
            nonce,
            algorithms: &self.algorithms,
        };
        let invalid =
            |e: Rejection| OidcError::InvalidToken(format!("Token verification failed: {e}"));
        let now = unix_now();
        let first = verify_id_token(token, &self.keys.read().await.set, &expected, now);
        if !matches!(first, Err(Rejection::UnknownKey)) {
            return first.map_err(invalid);
        }
        self.refresh_keys().await?;
        verify_id_token(token, &self.keys.read().await.set, &expected, now).map_err(invalid)
    }

    /// Fetch the JWKS again, unless it was tried within the last
    /// [`JWKS_REFETCH_INTERVAL`]. The write lock is held across the fetch, so
    /// concurrent logins wait for one fetch rather than each starting one, and
    /// a failed fetch counts as a try, so a provider whose keys endpoint is
    /// down is not asked again on every login.
    async fn refresh_keys(&self) -> Result<(), OidcError> {
        let mut keys = self.keys.write().await;
        if keys.fetched.elapsed() < JWKS_REFETCH_INTERVAL {
            return Ok(());
        }
        keys.fetched = Instant::now();
        let set = get_json(&self.http_client, &self.jwks_uri)
            .await
            .map_err(|e| OidcError::InvalidToken(format!("Refetching signing keys failed: {e}")))?;
        keys.set = set;
        drop(keys);
        Ok(())
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

/// Send one of oauth2's requests through `http`.
async fn send(
    http: reqwest::Client,
    request: oauth2::HttpRequest,
) -> Result<oauth2::HttpResponse, reqwest::Error> {
    let response = http.execute(request.try_into()?).await?;
    let status = response.status();
    let headers = response.headers().clone();
    let mut answer = http::Response::new(response.bytes().await?.to_vec());
    *answer.status_mut() = status;
    *answer.headers_mut() = headers;
    Ok(answer)
}

/// GET `url` and parse its JSON answer; anything but a 2xx is an error.
async fn get_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
) -> Result<T, String> {
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    let body = response.bytes().await.map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_slice(&body).map_err(|e| format!("{url}: {e}"))
}
