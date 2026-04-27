//! OIDC flow state tests.
//!
//! Tests for `OidcFlowState` expiration and validation.

use chrono::{DateTime, Utc};

/// OIDC flow state (matching the structure in hof-web/src/oidc.rs)
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields are used in tests but not all read directly
struct OidcFlowState {
    state: String,
    nonce: String,
    pkce_verifier: String,
    return_to: Option<String>,
    created_at: DateTime<Utc>,
}

impl OidcFlowState {
    const FLOW_EXPIRATION_SECS: i64 = 300; // 5 minutes

    fn is_expired(&self) -> bool {
        let elapsed = Utc::now().signed_duration_since(self.created_at);
        elapsed.num_seconds() > Self::FLOW_EXPIRATION_SECS
    }

    fn validate_state(&self, returned_state: &str) -> bool {
        self.state == returned_state
    }
}

#[test]
fn test_flow_state_not_expired() {
    let flow_state = OidcFlowState {
        state: "test-state".to_string(),
        nonce: "test-nonce".to_string(),
        pkce_verifier: "test-verifier".to_string(),
        return_to: None,
        created_at: Utc::now(),
    };

    assert!(
        !flow_state.is_expired(),
        "Flow state should not be expired immediately after creation"
    );
}

#[test]
fn test_flow_state_expired() {
    let flow_state = OidcFlowState {
        state: "test-state".to_string(),
        nonce: "test-nonce".to_string(),
        pkce_verifier: "test-verifier".to_string(),
        return_to: None,
        created_at: Utc::now() - chrono::Duration::seconds(400), // 400 seconds ago (> 300 limit)
    };

    assert!(
        flow_state.is_expired(),
        "Flow state should be expired after 400 seconds"
    );
}

#[test]
fn test_flow_state_almost_expired() {
    let flow_state = OidcFlowState {
        state: "test-state".to_string(),
        nonce: "test-nonce".to_string(),
        pkce_verifier: "test-verifier".to_string(),
        return_to: None,
        created_at: Utc::now() - chrono::Duration::seconds(299), // Just under 300 seconds
    };

    assert!(
        !flow_state.is_expired(),
        "Flow state should not be expired at 299 seconds"
    );
}

#[test]
fn test_flow_state_validate_success() {
    let flow_state = OidcFlowState {
        state: "correct-state".to_string(),
        nonce: "test-nonce".to_string(),
        pkce_verifier: "test-verifier".to_string(),
        return_to: None,
        created_at: Utc::now(),
    };

    assert!(
        flow_state.validate_state("correct-state"),
        "Should validate correct state"
    );
}

#[test]
fn test_flow_state_validate_failure() {
    let flow_state = OidcFlowState {
        state: "correct-state".to_string(),
        nonce: "test-nonce".to_string(),
        pkce_verifier: "test-verifier".to_string(),
        return_to: None,
        created_at: Utc::now(),
    };

    assert!(
        !flow_state.validate_state("wrong-state"),
        "Should reject incorrect state"
    );
}

#[test]
fn test_flow_state_with_return_to() {
    let flow_state = OidcFlowState {
        state: "test-state".to_string(),
        nonce: "test-nonce".to_string(),
        pkce_verifier: "test-verifier".to_string(),
        return_to: Some("/dashboard".to_string()),
        created_at: Utc::now(),
    };

    assert_eq!(flow_state.return_to, Some("/dashboard".to_string()));
    assert!(!flow_state.is_expired());
}

#[test]
fn test_flow_state_csrf_protection() {
    // Simulate a CSRF attack by providing a different state
    let legitimate_flow = OidcFlowState {
        state: "legitimate-state-123".to_string(),
        nonce: "legitimate-nonce-456".to_string(),
        pkce_verifier: "legitimate-verifier-789".to_string(),
        return_to: None,
        created_at: Utc::now(),
    };

    // Attacker tries to use a different state
    let attacker_state = "attacker-state-999";
    assert!(
        !legitimate_flow.validate_state(attacker_state),
        "Should reject state mismatch (CSRF protection)"
    );
}

#[test]
fn test_flow_state_expiration_boundary() {
    // Test at exactly 301 seconds (> 300 limit)
    let flow_state = OidcFlowState {
        state: "test-state".to_string(),
        nonce: "test-nonce".to_string(),
        pkce_verifier: "test-verifier".to_string(),
        return_to: None,
        created_at: Utc::now() - chrono::Duration::seconds(301),
    };

    assert!(
        flow_state.is_expired(),
        "Flow state should be expired at 301 seconds (> 300 limit)"
    );
}
