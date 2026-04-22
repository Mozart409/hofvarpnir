//! OIDC client integration tests with mocked provider.
//!
//! Uses wiremock to simulate an OIDC provider for testing discovery and basic flow.

use std::time::Duration;

use hof_core::oidc::{OidcClient, OidcConfig};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// Creates a mock OIDC provider discovery endpoint.
async fn setup_mock_discovery(mock_server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": mock_server.uri(),
            "authorization_endpoint": format!("{}/auth", mock_server.uri()),
            "token_endpoint": format!("{}/token", mock_server.uri()),
            "jwks_uri": format!("{}/jwks", mock_server.uri()),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
            "scopes_supported": ["openid", "profile", "email"],
            "token_endpoint_auth_methods_supported": ["client_secret_post"]
        })))
        .mount(mock_server)
        .await;
}

/// Creates a mock JWKS endpoint with a test key.
async fn setup_mock_jwks(mock_server: &MockServer) {
    // Note: This is a mock key - it won't pass real signature validation,
    // but it allows the discovery to complete
    Mock::given(method("GET"))
        .and(path("/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "keys": [{
                "kty": "RSA",
                "use": "sig",
                "kid": "test-key-1",
                "n": "xGOr-H7G2AosXkA5J9C6kT1F3y8PqR9XxV1z2y3a4b5c6d7e8f9g0h1i2j3k4l5m6n7o8p9q0r1s2t3u4v5w6x7y8z9A0B1C2D3E4F5G6H7I8J9K0L1M2N3O4P5Q6R7S8T9U0V1W2X3Y4Z5a6b7c8d9e0f1g2h3i4j5k6l7m8n9o0p1q2r3s4t5u6v7w8x9y0z1A2B3C4D5E6F7G8H9I0J1K2L3M4N5O6P7Q8R9S0T1U2V3W4X5Y6Z7",
                "e": "AQAB",
                "alg": "RS256"
            }]
        })))
        .mount(mock_server)
        .await;
}

#[tokio::test]
async fn test_oidc_discovery_success() {
    let mock_server = MockServer::start().await;
    setup_mock_discovery(&mock_server).await;
    setup_mock_jwks(&mock_server).await;

    let config = OidcConfig {
        issuer_url: mock_server.uri(),
        client_id: "test-client-id".to_string(),
        client_secret: "test-client-secret".to_string(),
        scopes: vec![
            "openid".to_string(),
            "profile".to_string(),
            "email".to_string(),
        ],
        auto_provision: true,
        redirect_base_url: None,
        logout_redirect: false,
        discovery_timeout: Duration::from_secs(30),
    };

    let result = OidcClient::discover(config).await;
    assert!(
        result.is_ok(),
        "Discovery should succeed: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_oidc_discovery_timeout() {
    let mock_server = MockServer::start().await;
    // Don't set up any mocks - this will cause the request to hang/timeout

    let config = OidcConfig {
        issuer_url: mock_server.uri(),
        client_id: "test-client-id".to_string(),
        client_secret: "test-client-secret".to_string(),
        scopes: vec!["openid".to_string()],
        auto_provision: true,
        redirect_base_url: None,
        logout_redirect: false,
        discovery_timeout: Duration::from_millis(100), // Very short timeout
    };

    let result = OidcClient::discover(config).await;
    // Should timeout or fail since no mock is set up
    assert!(result.is_err(), "Discovery should fail with short timeout");
}

#[tokio::test]
async fn test_oidc_discovery_invalid_issuer() {
    let config = OidcConfig {
        issuer_url: "not-a-valid-url".to_string(),
        client_id: "test-client-id".to_string(),
        client_secret: "test-client-secret".to_string(),
        scopes: vec!["openid".to_string()],
        auto_provision: true,
        redirect_base_url: None,
        logout_redirect: false,
        discovery_timeout: Duration::from_secs(5),
    };

    let result = OidcClient::discover(config).await;
    assert!(
        result.is_err(),
        "Discovery should fail with invalid issuer URL"
    );
}

#[tokio::test]
async fn test_oidc_authorization_url_generation() {
    let mock_server = MockServer::start().await;
    setup_mock_discovery(&mock_server).await;
    setup_mock_jwks(&mock_server).await;

    let config = OidcConfig {
        issuer_url: mock_server.uri(),
        client_id: "test-client-id".to_string(),
        client_secret: "test-client-secret".to_string(),
        scopes: vec![
            "openid".to_string(),
            "profile".to_string(),
            "email".to_string(),
        ],
        auto_provision: true,
        redirect_base_url: None,
        logout_redirect: false,
        discovery_timeout: Duration::from_secs(30),
    };

    let client = OidcClient::discover(config)
        .await
        .expect("Discovery should succeed");

    let (auth_url, _csrf_token, _nonce, pkce_verifier) =
        client.authorization_url("https://example.com/auth/oidc/callback");

    let url_str = auth_url.to_string();

    // Verify URL contains expected parameters
    assert!(
        url_str.contains(&mock_server.uri()),
        "URL should contain mock server"
    );
    assert!(
        url_str.contains("client_id=test-client-id"),
        "URL should contain client_id"
    );
    assert!(
        url_str.contains("response_type=code"),
        "URL should request code"
    );
    assert!(
        url_str.contains("code_challenge="),
        "URL should include PKCE challenge"
    );
    assert!(
        url_str.contains("code_challenge_method=S256"),
        "URL should use S256"
    );
    assert!(url_str.contains("scope="), "URL should include scopes");
    assert!(url_str.contains("state="), "URL should include state");
    assert!(url_str.contains("nonce="), "URL should include nonce");

    // Verify PKCE verifier was generated
    assert!(
        !pkce_verifier.secret().is_empty(),
        "PKCE verifier should be generated"
    );
}

#[tokio::test]
async fn test_oidc_authorization_url_with_custom_scopes() {
    let mock_server = MockServer::start().await;
    setup_mock_discovery(&mock_server).await;
    setup_mock_jwks(&mock_server).await;

    let config = OidcConfig {
        issuer_url: mock_server.uri(),
        client_id: "test-client-id".to_string(),
        client_secret: "test-client-secret".to_string(),
        scopes: vec!["openid".to_string(), "custom_scope".to_string()],
        auto_provision: true,
        redirect_base_url: None,
        logout_redirect: false,
        discovery_timeout: Duration::from_secs(30),
    };

    let client = OidcClient::discover(config)
        .await
        .expect("Discovery should succeed");

    let (auth_url, _csrf_token, _nonce, _pkce_verifier) =
        client.authorization_url("https://example.com/auth/oidc/callback");

    let url_str = auth_url.to_string();
    // The openidconnect crate may add openid automatically, so we just check for custom_scope
    assert!(
        url_str.contains("custom_scope"),
        "URL should contain custom_scope: {url_str}"
    );
    assert!(
        url_str.contains("scope="),
        "URL should contain scope parameter: {url_str}"
    );
}

#[tokio::test]
async fn test_oidc_client_issuer_url_accessor() {
    let mock_server = MockServer::start().await;
    setup_mock_discovery(&mock_server).await;
    setup_mock_jwks(&mock_server).await;

    let config = OidcConfig {
        issuer_url: mock_server.uri(),
        client_id: "test-client-id".to_string(),
        client_secret: "test-client-secret".to_string(),
        scopes: vec!["openid".to_string()],
        auto_provision: true,
        redirect_base_url: None,
        logout_redirect: false,
        discovery_timeout: Duration::from_secs(30),
    };

    let client = OidcClient::discover(config)
        .await
        .expect("Discovery should succeed");
    assert_eq!(client.issuer_url(), mock_server.uri());
}

#[tokio::test]
async fn test_oidc_client_auto_provision_accessor() {
    let mock_server = MockServer::start().await;
    setup_mock_discovery(&mock_server).await;
    setup_mock_jwks(&mock_server).await;

    // Test with auto_provision = true
    let config = OidcConfig {
        issuer_url: mock_server.uri(),
        client_id: "test-client-id".to_string(),
        client_secret: "test-client-secret".to_string(),
        scopes: vec!["openid".to_string()],
        auto_provision: true,
        redirect_base_url: None,
        logout_redirect: false,
        discovery_timeout: Duration::from_secs(30),
    };

    let client = OidcClient::discover(config)
        .await
        .expect("Discovery should succeed");
    assert!(client.auto_provision());

    // Test with auto_provision = false
    let config = OidcConfig {
        issuer_url: mock_server.uri(),
        client_id: "test-client-id".to_string(),
        client_secret: "test-client-secret".to_string(),
        scopes: vec!["openid".to_string()],
        auto_provision: false,
        redirect_base_url: None,
        logout_redirect: false,
        discovery_timeout: Duration::from_secs(30),
    };

    let client = OidcClient::discover(config)
        .await
        .expect("Discovery should succeed");
    assert!(!client.auto_provision());
}
