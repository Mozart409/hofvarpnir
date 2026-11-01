//! A whole OIDC login against a wiremock provider that signs real ID tokens.
//!
//! The provider checks what a real one would (the code, the PKCE verifier,
//! the client credentials in the request body as its discovery document
//! asks), and its ID token carries the nonce taken from the authorization URL.
//! The ways it can go wrong are the cases below: a token for another login
//! (nonce), a token without the required email, a token signed by a key the
//! provider has not published, and a provider that names another issuer.

// The helpers below are test code too; clippy.toml's in-test allowances only
// reach #[test] functions.
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::{KeySize, PublicKeyComponents};
use aws_lc_rs::signature::{KeyPair, RSA_PKCS1_SHA256, RsaKeyPair};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hof_core::oidc::{Nonce, OidcClient, OidcConfig, OidcError};
use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "hof-client";
const CLIENT_SECRET: &str = "hof-secret";
const CALLBACK: &str = "https://hof.example.com/auth/oidc/callback";
const CODE: &str = "the-authorization-code";

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// An RS256 signing key of the provider.
struct Key {
    kid: &'static str,
    pair: RsaKeyPair,
}

impl Key {
    fn new(kid: &'static str) -> Self {
        Self {
            kid,
            pair: RsaKeyPair::generate(KeySize::Rsa2048).unwrap(),
        }
    }

    fn jwk(&self) -> Value {
        let public = PublicKeyComponents::<Vec<u8>>::from(self.pair.public_key());
        json!({"kty": "RSA", "use": "sig", "alg": "RS256", "kid": self.kid,
               "n": b64(&public.n), "e": b64(&public.e)})
    }

    fn sign(&self, claims: &Value) -> String {
        let header = json!({"alg": "RS256", "kid": self.kid, "typ": "JWT"});
        let input = format!(
            "{}.{}",
            b64(header.to_string().as_bytes()),
            b64(claims.to_string().as_bytes())
        );
        let mut signature = vec![0; self.pair.public_modulus_len()];
        self.pair
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                input.as_bytes(),
                &mut signature,
            )
            .unwrap();
        format!("{input}.{}", b64(&signature))
    }
}

/// A provider at `server` that publishes `published` and offers only
/// `client_secret_post`, as some providers do.
async fn provider(server: &MockServer, issuer: &str, published: &[&Key]) {
    provider_with_auth_methods(server, issuer, published, &["client_secret_post"]).await;
}

/// A provider that offers `auth_methods` at its token endpoint.
async fn provider_with_auth_methods(
    server: &MockServer,
    issuer: &str,
    published: &[&Key],
    auth_methods: &[&str],
) {
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
            "jwks_uri": format!("{}/jwks", server.uri()),
            "end_session_endpoint": format!("{}/logout", server.uri()),
            "id_token_signing_alg_values_supported": ["RS256"],
            "token_endpoint_auth_methods_supported": auth_methods
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/jwks"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"keys": published.iter().map(|k| k.jwk()).collect::<Vec<_>>()}),
            ),
        )
        .mount(server)
        .await;
}

/// The token endpoint answers with `id_token` only for our code, the PKCE
/// verifier of this login and the client credentials in the body.
async fn token_endpoint(server: &MockServer, verifier: &str, id_token: &str) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains(format!("code={CODE}")))
        .and(body_string_contains(format!("code_verifier={verifier}")))
        .and(body_string_contains(format!(
            "client_secret={CLIENT_SECRET}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at",
            "token_type": "Bearer",
            "expires_in": 3600,
            "id_token": id_token
        })))
        .expect(1)
        .mount(server)
        .await;
}

fn config(issuer: &str) -> OidcConfig {
    OidcConfig {
        issuer_url: issuer.to_string(),
        client_id: CLIENT_ID.to_string(),
        client_secret: CLIENT_SECRET.to_string(),
        scopes: vec!["openid".into(), "profile".into(), "email".into()],
        auto_provision: true,
        redirect_base_url: None,
        logout_redirect: true,
        discovery_timeout: Duration::from_secs(5),
    }
}

fn claims(issuer: &str, nonce: &str) -> Value {
    json!({"iss": issuer, "sub": "user-42", "aud": CLIENT_ID, "exp": now() + 300,
           "iat": now(), "nonce": nonce, "email": "ada@example.com", "name": "Ada",
           "picture": "https://idp.example.com/ada.png"})
}

/// The value of query parameter `name` in `url`.
fn param(url: &Url, name: &str) -> String {
    url.query_pairs().find(|(k, _)| k == name).map_or_else(
        || panic!("{name} missing from {url}"),
        |(_, v)| v.into_owned(),
    )
}

#[tokio::test]
async fn a_login_yields_the_claims_of_the_signed_id_token() {
    let server = MockServer::start().await;
    let key = Key::new("k1");
    provider(&server, &server.uri(), &[&key]).await;
    let client = OidcClient::discover(config(&server.uri())).await.unwrap();

    let (url, csrf, nonce, verifier) = client.authorization_url(CALLBACK);
    assert_eq!(url.path(), "/authorize");
    assert_eq!(param(&url, "state"), *csrf.secret());
    assert_eq!(param(&url, "nonce"), *nonce.secret());
    assert_eq!(param(&url, "redirect_uri"), CALLBACK);
    assert_eq!(param(&url, "code_challenge_method"), "S256");
    assert_eq!(param(&url, "scope"), "openid profile email");

    let id_token = key.sign(&claims(&server.uri(), &param(&url, "nonce")));
    token_endpoint(&server, verifier.secret(), &id_token).await;
    let claims = client
        .exchange_code(CODE, CALLBACK, verifier, &nonce)
        .await
        .unwrap();

    assert_eq!(
        (
            claims.issuer.as_str(),
            claims.subject.as_str(),
            claims.email.as_str(),
            claims.name.as_deref(),
            claims.picture.as_deref()
        ),
        (
            server.uri().as_str(),
            "user-42",
            "ada@example.com",
            Some("Ada"),
            Some("https://idp.example.com/ada.png")
        )
    );
    // RP-initiated logout goes to the discovered end_session_endpoint.
    let logout = client
        .end_session_url(Some(&id_token), "https://hof.example.com/")
        .unwrap();
    assert_eq!(logout.path(), "/logout");
    assert_eq!(param(&logout, "id_token_hint"), id_token);
}

#[tokio::test]
async fn a_provider_offering_basic_auth_gets_the_secret_in_the_header() {
    let server = MockServer::start().await;
    let key = Key::new("k1");
    provider_with_auth_methods(
        &server,
        &server.uri(),
        &[&key],
        &["client_secret_basic", "client_secret_post"],
    )
    .await;
    let client = OidcClient::discover(config(&server.uri())).await.unwrap();

    let (_, _, nonce, verifier) = client.authorization_url(CALLBACK);
    let basic = STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"));
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(header("authorization", format!("Basic {basic}").as_str()))
        .and(body_string_contains(format!("code={CODE}")))
        .and(body_string_contains(format!(
            "code_verifier={}",
            verifier.secret()
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at",
            "token_type": "Bearer",
            "id_token": key.sign(&claims(&server.uri(), nonce.secret()))
        })))
        .expect(1)
        .mount(&server)
        .await;
    let claims = client
        .exchange_code(CODE, CALLBACK, verifier, &nonce)
        .await
        .unwrap();
    assert_eq!(claims.subject, "user-42");

    let requests = server.received_requests().await.unwrap();
    let token_request = requests.iter().find(|r| r.url.path() == "/token").unwrap();
    let body = String::from_utf8_lossy(&token_request.body);
    assert!(!body.contains("client_secret"), "{body}");
}

#[tokio::test]
async fn a_token_for_another_login_is_refused() {
    let server = MockServer::start().await;
    let key = Key::new("k1");
    provider(&server, &server.uri(), &[&key]).await;
    let client = OidcClient::discover(config(&server.uri())).await.unwrap();

    let (_, _, nonce, verifier) = client.authorization_url(CALLBACK);
    let other_login = Nonce::new_random();
    token_endpoint(
        &server,
        verifier.secret(),
        &key.sign(&claims(&server.uri(), other_login.secret())),
    )
    .await;
    let err = client
        .exchange_code(CODE, CALLBACK, verifier, &nonce)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, OidcError::InvalidToken(why) if why.contains("nonce")),
        "{err}"
    );
}

#[tokio::test]
async fn a_token_without_email_is_refused() {
    let server = MockServer::start().await;
    let key = Key::new("k1");
    provider(&server, &server.uri(), &[&key]).await;
    let client = OidcClient::discover(config(&server.uri())).await.unwrap();

    let (_, _, nonce, verifier) = client.authorization_url(CALLBACK);
    let mut claims = claims(&server.uri(), nonce.secret());
    claims.as_object_mut().unwrap().remove("email");
    token_endpoint(&server, verifier.secret(), &key.sign(&claims)).await;
    let err = client
        .exchange_code(CODE, CALLBACK, verifier, &nonce)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, OidcError::MissingClaim(claim) if claim == "email"),
        "{err}"
    );
}

#[tokio::test]
async fn a_token_signed_by_an_unpublished_key_is_refused() {
    let server = MockServer::start().await;
    let (published, unpublished) = (Key::new("k1"), Key::new("k2"));
    provider(&server, &server.uri(), &[&published]).await;
    let client = OidcClient::discover(config(&server.uri())).await.unwrap();

    let (_, _, nonce, verifier) = client.authorization_url(CALLBACK);
    token_endpoint(
        &server,
        verifier.secret(),
        &unpublished.sign(&claims(&server.uri(), nonce.secret())),
    )
    .await;
    let err = client
        .exchange_code(CODE, CALLBACK, verifier, &nonce)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, OidcError::InvalidToken(why) if why.contains("does not publish")),
        "{err}"
    );
    // The keys were fetched once at discovery; within the refetch interval an
    // unknown kid does not make us ask the provider again.
    let jwks_fetches = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/jwks")
        .count();
    assert_eq!(jwks_fetches, 1);
}

#[tokio::test]
async fn a_provider_naming_another_issuer_is_refused_at_discovery() {
    let server = MockServer::start().await;
    provider(&server, "https://evil.example.com", &[&Key::new("k1")]).await;
    let err = OidcClient::discover(config(&server.uri()))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&err, OidcError::DiscoveryFailed(why) if why.contains("evil.example.com")),
        "{err}"
    );
}
