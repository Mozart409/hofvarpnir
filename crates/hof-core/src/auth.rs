//! Authentication utilities for password hashing, verification, and API key management.

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use rand::distr::{Alphanumeric, SampleString};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Authentication errors.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("Password hashing failed: {0}")]
    HashingFailed(String),
}

/// Hash a password using Argon2id.
///
/// # Errors
///
/// Returns an error if password hashing fails.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    // argon2 0.6 generates the salt internally (recommended length, via
    // `getrandom`), replacing the explicit `SaltString::generate(&mut OsRng)`
    // of 0.5. The PHC output format is unchanged, so hashes written by either
    // version verify against the other.
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|e| AuthError::HashingFailed(e.to_string()))
}

/// Verify a password against a stored hash.
///
/// # Errors
///
/// Returns `AuthError::InvalidCredentials` if the password doesn't match.
pub fn verify_password(password: &str, password_hash: &str) -> Result<(), AuthError> {
    let parsed_hash =
        PasswordHash::new(password_hash).map_err(|_| AuthError::InvalidCredentials)?;

    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .map_err(|_| AuthError::InvalidCredentials)
}

/// API key prefix used for all generated keys.
const API_KEY_PREFIX: &str = "hof_sk_";

/// Length of the random portion of the API key.
const API_KEY_RANDOM_LENGTH: usize = 32;

/// Length of the display prefix (includes `hof_sk_` + first 5 random chars).
const API_KEY_DISPLAY_PREFIX_LENGTH: usize = 12;

/// Generated API key with its components.
pub struct GeneratedApiKey {
    /// The full token to return to the user (only shown once).
    pub token: String,
    /// Display prefix for identifying the key (e.g., `hof_sk_Ab3xY`).
    pub prefix: String,
    /// SHA-256 hash of the full token for storage.
    pub hash: String,
}

/// Generate a new API key.
///
/// Returns the full token (to show user once), a display prefix, and the SHA-256 hash for storage.
/// Format: `hof_sk_<32 random alphanumeric chars>` (total ~39 chars).
#[must_use]
pub fn generate_api_key() -> GeneratedApiKey {
    let random_part = Alphanumeric.sample_string(&mut rand::rng(), API_KEY_RANDOM_LENGTH);
    let token = format!("{API_KEY_PREFIX}{random_part}");
    let prefix = token[..API_KEY_DISPLAY_PREFIX_LENGTH].to_string();
    let hash = hash_api_key(&token);

    GeneratedApiKey {
        token,
        prefix,
        hash,
    }
}

/// Hash an API key token using SHA-256.
///
/// Used for both storing new keys and looking up keys on incoming requests.
#[must_use]
pub fn hash_api_key(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn test_hash_and_verify_password() {
        let password = "secure_password_123";
        let hash = hash_password(password).unwrap();

        // Should verify correctly
        assert!(verify_password(password, &hash).is_ok());

        // Wrong password should fail
        assert!(verify_password("wrong_password", &hash).is_err());
    }

    /// Hashes written before the argon2 0.5 -> 0.6 upgrade must still verify.
    ///
    /// 0.6 moved salt generation inside `hash_password`, so this is the only
    /// check that the stored PHC strings in `users.password_hash` are still
    /// readable. The literal below was produced by argon2 0.5 with
    /// `SaltString::generate(&mut OsRng)`; do not regenerate it, that would
    /// defeat the test.
    #[test]
    fn argon2_0_5_hashes_still_verify() {
        let legacy = "$argon2id$v=19$m=19456,t=2,p=1$slKZiGr85w6za9MBkarRrg\
                      $9pHj7dJOn8ZF3IAj7dz25w8NmZCRxKxK9Z3aWH35Crg"
            .replace(char::is_whitespace, "");

        assert!(verify_password("correct horse battery staple", &legacy).is_ok());
        assert!(verify_password("wrong password", &legacy).is_err());
    }

    #[test]
    fn test_different_hashes_for_same_password() {
        let password = "test_password";
        let hash1 = hash_password(password).unwrap();
        let hash2 = hash_password(password).unwrap();

        // Hashes should be different (different salts)
        assert_ne!(hash1, hash2);

        // But both should verify
        assert!(verify_password(password, &hash1).is_ok());
        assert!(verify_password(password, &hash2).is_ok());
    }

    #[test]
    fn test_generate_api_key_format() {
        let key = generate_api_key();

        // Token should start with prefix
        assert!(key.token.starts_with("hof_sk_"));

        // Token should be prefix (7) + random (32) = 39 chars
        assert_eq!(key.token.len(), 39);

        // Display prefix should be first 12 chars
        assert_eq!(key.prefix.len(), 12);
        assert!(key.token.starts_with(&key.prefix));

        // Hash should be 64 hex chars (SHA-256 = 256 bits = 32 bytes = 64 hex)
        assert_eq!(key.hash.len(), 64);
        assert!(key.hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_api_key_uniqueness() {
        let keys: HashSet<String> = (0..1000).map(|_| generate_api_key().token).collect();

        // All 1000 keys should be unique
        assert_eq!(keys.len(), 1000);
    }

    #[test]
    fn test_hash_api_key_consistency() {
        let token = "hof_sk_TestToken12345678901234567890";
        let hash1 = hash_api_key(token);
        let hash2 = hash_api_key(token);

        // Same input should produce same hash
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_prefix_extraction() {
        let key = generate_api_key();

        // Prefix should be exactly the first 12 chars of the token
        assert_eq!(key.prefix, &key.token[..12]);
        assert!(key.prefix.starts_with("hof_sk_"));
    }

    #[test]
    fn test_hash_api_key_different_inputs() {
        let hash1 = hash_api_key("hof_sk_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let hash2 = hash_api_key("hof_sk_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");

        // Different inputs should produce different hashes
        assert_ne!(hash1, hash2);
    }
}
