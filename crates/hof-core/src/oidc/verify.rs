//! ID-token validation (OIDC Core 3.1.3.7), as a pure function over the token,
//! the provider's keys and what the token has to match, so it can be tested
//! without a provider. `jsonwebtoken` checks the signature; it runs on the
//! `aws_lc_rs` backend, as its `rust_crypto` backend would pull in `rsa`.

use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::jwk::{Jwk, JwkSet, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

/// Clock skew allowed between us and the provider for `exp`, `iat` and
/// `nbf`, in seconds.
const LEEWAY_SECS: u64 = 60;

/// The ID-token signature algorithms we accept at all: asymmetric only, so
/// never `none` or an HMAC keyed with the (shared) client secret.
const ASYMMETRIC_ALGORITHMS: [Algorithm; 9] = [
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::PS256,
    Algorithm::PS384,
    Algorithm::PS512,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];

/// The claims of a verified ID token that the login uses.
#[derive(Debug)]
pub(super) struct VerifiedIdentity {
    pub(super) issuer: String,
    pub(super) subject: String,
    pub(super) email: Option<String>,
    pub(super) name: Option<String>,
    pub(super) picture: Option<String>,
}

/// The advertised ID-token algorithms that are also in
/// [`ASYMMETRIC_ALGORITHMS`]; RS256, the OIDC default, when the provider
/// advertises none.
pub(super) fn accepted_algorithms(advertised: &[String]) -> Vec<Algorithm> {
    if advertised.is_empty() {
        return vec![Algorithm::RS256];
    }
    advertised
        .iter()
        .filter_map(|alg| alg.parse::<Algorithm>().ok())
        .filter(|alg| ASYMMETRIC_ALGORITHMS.contains(alg))
        .collect()
}

/// What an ID token has to match besides its signature.
pub(super) struct Expected<'a> {
    pub(super) issuer: &'a str,
    pub(super) client_id: &'a str,
    /// The nonce stored with the login flow.
    pub(super) nonce: &'a str,
    pub(super) algorithms: &'a [Algorithm],
}

/// The ID-token claims we check or keep.
#[derive(Deserialize)]
struct IdTokenClaims {
    iss: String,
    sub: String,
    /// A string or an array of strings; only counted here, as `decode`
    /// already checked that we are in it.
    aud: serde_json::Value,
    exp: u64,
    iat: u64,
    nbf: Option<u64>,
    nonce: Option<String>,
    azp: Option<String>,
    email: Option<String>,
    name: Option<String>,
    picture: Option<String>,
}

/// Why an ID token was refused.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Rejection {
    /// It names a key the JWKS does not have (yet).
    UnknownKey,
    Invalid(String),
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKey => f.write_str("signed with a key the provider does not publish"),
            Self::Invalid(why) => f.write_str(why),
        }
    }
}

/// Validate an ID token from the token endpoint as OIDC Core 3.1.3.7 asks:
/// an accepted asymmetric algorithm, a signature by one of the provider's
/// `keys`, our issuer and client in `iss` and `aud` (and `azp`), not expired
/// and not issued in the future at `now` (Unix seconds, give or take
/// [`LEEWAY_SECS`]), and the nonce of this login.
pub(super) fn verify_id_token(
    token: &str,
    keys: &JwkSet,
    expected: &Expected<'_>,
    now: u64,
) -> Result<VerifiedIdentity, Rejection> {
    let invalid = |why: String| Rejection::Invalid(why);
    let header =
        jsonwebtoken::decode_header(token).map_err(|e| invalid(format!("malformed: {e}")))?;
    if !expected.algorithms.contains(&header.alg) {
        return Err(invalid(format!(
            "algorithm {:?} is not accepted",
            header.alg
        )));
    }
    let jwk = signing_key(keys, header.kid.as_deref())?;
    if let Some(key_alg) = &jwk.common.key_algorithm
        && key_alg.to_string() != format!("{:?}", header.alg)
    {
        return Err(invalid(format!(
            "the key is for {key_alg}, the token says {:?}",
            header.alg
        )));
    }
    let key = DecodingKey::from_jwk(jwk).map_err(|e| invalid(format!("unusable key: {e}")))?;

    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[expected.issuer]);
    validation.set_audience(&[expected.client_id]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    // `exp`, `iat` and `nbf` are checked below against `now` rather than the
    // clock.
    validation.validate_exp = false;
    let claims = jsonwebtoken::decode::<IdTokenClaims>(token, &key, &validation)
        .map_err(|e| invalid(e.to_string()))?
        .claims;

    if claims.exp.saturating_add(LEEWAY_SECS) < now {
        return Err(invalid("expired".to_string()));
    }
    if claims.iat > now.saturating_add(LEEWAY_SECS) {
        return Err(invalid("issued in the future".to_string()));
    }
    if claims
        .nbf
        .is_some_and(|nbf| nbf > now.saturating_add(LEEWAY_SECS))
    {
        return Err(invalid("not valid yet".to_string()));
    }
    let several_audiences = claims.aud.as_array().is_some_and(|aud| aud.len() > 1);
    if (several_audiences || claims.azp.is_some())
        && claims.azp.as_deref() != Some(expected.client_id)
    {
        return Err(invalid(
            "authorized party (azp) is not this client".to_string(),
        ));
    }
    let nonce_matches = claims
        .nonce
        .as_deref()
        .is_some_and(|nonce| constant_time_eq(nonce.as_bytes(), expected.nonce.as_bytes()));
    if !nonce_matches {
        return Err(invalid("nonce does not match this login".to_string()));
    }

    Ok(VerifiedIdentity {
        issuer: claims.iss,
        subject: claims.sub,
        email: claims.email,
        name: claims.name,
        picture: claims.picture,
    })
}

/// The signing key `kid` names, or the only one when the token names none.
/// Keys published for encryption only are passed over.
fn signing_key<'k>(keys: &'k JwkSet, kid: Option<&str>) -> Result<&'k Jwk, Rejection> {
    let mut signing = keys.keys.iter().filter(|jwk| {
        jwk.common
            .public_key_use
            .as_ref()
            .is_none_or(|key_use| *key_use == PublicKeyUse::Signature)
    });
    match kid {
        Some(kid) => signing
            .find(|jwk| jwk.common.key_id.as_deref() == Some(kid))
            .ok_or(Rejection::UnknownKey),
        None => match (signing.next(), signing.next()) {
            (Some(only), None) => Ok(only),
            (None, _) => Err(Rejection::UnknownKey),
            (Some(_), Some(_)) => Err(Rejection::Invalid(
                "the token names no key and the provider has several".to_string(),
            )),
        },
    }
}

/// Whether `a == b`, in time that depends on the lengths only.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// The current time in Unix seconds.
pub(super) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod id_token_tests {
    use aws_lc_rs::rand::SystemRandom;
    use aws_lc_rs::rsa::{KeySize, PublicKeyComponents};
    use aws_lc_rs::signature::{
        ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair, RSA_PKCS1_SHA256, RsaKeyPair,
    };
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use jsonwebtoken::Algorithm;
    use jsonwebtoken::jwk::JwkSet;
    use serde_json::{Value, json};

    use super::{Expected, Rejection, VerifiedIdentity, accepted_algorithms, verify_id_token};

    const ISSUER: &str = "https://idp.example.com";
    const CLIENT: &str = "hofvarpnir";
    const NONCE: &str = "n-0S6_WzA2Mj";
    const NOW: u64 = 1_800_000_000;
    const ALGORITHMS: [Algorithm; 2] = [Algorithm::RS256, Algorithm::ES256];

    fn b64(bytes: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    enum Signer {
        Rsa(RsaKeyPair),
        Ec(EcdsaKeyPair),
    }

    /// A provider signing key, generated per test.
    struct Key {
        kid: &'static str,
        signer: Signer,
    }

    impl Key {
        fn rsa(kid: &'static str) -> Self {
            Self {
                kid,
                signer: Signer::Rsa(RsaKeyPair::generate(KeySize::Rsa2048).unwrap()),
            }
        }

        fn ec(kid: &'static str) -> Self {
            Self {
                kid,
                signer: Signer::Ec(
                    EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING).unwrap(),
                ),
            }
        }

        const fn alg(&self) -> &'static str {
            match self.signer {
                Signer::Rsa(_) => "RS256",
                Signer::Ec(_) => "ES256",
            }
        }

        /// The public half, as the provider's JWKS lists it.
        fn jwk(&self) -> Value {
            match &self.signer {
                Signer::Rsa(pair) => {
                    let public = PublicKeyComponents::<Vec<u8>>::from(pair.public_key());
                    json!({"kty": "RSA", "use": "sig", "alg": "RS256", "kid": self.kid,
                           "n": b64(&public.n), "e": b64(&public.e)})
                }
                Signer::Ec(pair) => {
                    // The uncompressed point: 0x04, then x and y.
                    let point = pair.public_key().as_ref();
                    json!({"kty": "EC", "crv": "P-256", "use": "sig", "alg": "ES256",
                           "kid": self.kid, "x": b64(&point[1..33]), "y": b64(&point[33..65])})
                }
            }
        }

        /// A JWT of `header` and `claims`, signed with this key.
        fn sign(&self, header: &Value, claims: &Value) -> String {
            let input = format!(
                "{}.{}",
                b64(header.to_string().as_bytes()),
                b64(claims.to_string().as_bytes())
            );
            let rng = SystemRandom::new();
            let signature = match &self.signer {
                Signer::Rsa(pair) => {
                    let mut signature = vec![0; pair.public_modulus_len()];
                    pair.sign(&RSA_PKCS1_SHA256, &rng, input.as_bytes(), &mut signature)
                        .unwrap();
                    signature
                }
                Signer::Ec(pair) => pair.sign(&rng, input.as_bytes()).unwrap().as_ref().to_vec(),
            };
            format!("{input}.{}", b64(&signature))
        }

        /// A token with `claims`, as the provider would issue it.
        fn token(&self, claims: &Value) -> String {
            self.sign(
                &json!({"alg": self.alg(), "kid": self.kid, "typ": "JWT"}),
                claims,
            )
        }
    }

    fn jwks(keys: &[&Key]) -> JwkSet {
        serde_json::from_value(json!({"keys": keys.iter().map(|k| k.jwk()).collect::<Vec<_>>()}))
            .unwrap()
    }

    /// The claims of a good token.
    fn claims() -> Value {
        json!({"iss": ISSUER, "sub": "user-1", "aud": CLIENT, "exp": NOW + 300,
               "iat": NOW - 10, "nonce": NONCE, "email": "ada@example.com", "name": "Ada",
               "picture": "https://idp.example.com/ada.png"})
    }

    /// [`claims`] with some replaced; a `null` removes the claim.
    fn claims_with(changes: &Value) -> Value {
        let mut claims = claims();
        let map = claims.as_object_mut().unwrap();
        for (name, value) in changes.as_object().unwrap() {
            if value.is_null() {
                map.remove(name);
            } else {
                map.insert(name.clone(), value.clone());
            }
        }
        claims
    }

    fn verify(token: &str, keys: &JwkSet) -> Result<VerifiedIdentity, Rejection> {
        let expected = Expected {
            issuer: ISSUER,
            client_id: CLIENT,
            nonce: NONCE,
            algorithms: &ALGORITHMS,
        };
        verify_id_token(token, keys, &expected, NOW)
    }

    /// Why `token` is refused; fails the test if it is accepted.
    fn refusal(token: &str, keys: &JwkSet) -> String {
        match verify(token, keys) {
            Err(Rejection::Invalid(why)) => why,
            other => panic!("expected the token to be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_good_rs256_token_gives_the_identity() {
        let key = Key::rsa("k1");
        let identity = verify(&key.token(&claims()), &jwks(&[&key])).unwrap();
        assert_eq!(
            (
                identity.issuer.as_str(),
                identity.subject.as_str(),
                identity.email.as_deref(),
                identity.name.as_deref(),
                identity.picture.as_deref()
            ),
            (
                ISSUER,
                "user-1",
                Some("ada@example.com"),
                Some("Ada"),
                Some("https://idp.example.com/ada.png")
            )
        );
    }

    #[test]
    fn a_good_es256_token_passes_and_the_key_is_picked_by_kid() {
        let (rsa, ec) = (Key::rsa("k1"), Key::ec("k2"));
        assert!(verify(&ec.token(&claims()), &jwks(&[&rsa, &ec])).is_ok());
    }

    #[test]
    fn a_token_signed_by_another_key_is_refused() {
        let (key, impostor) = (Key::rsa("k1"), Key::rsa("k1"));
        let why = refusal(&impostor.token(&claims()), &jwks(&[&key]));
        assert!(why.contains("Signature"), "{why}");
    }

    #[test]
    fn a_tampered_token_is_refused() {
        let key = Key::rsa("k1");
        let token = key.token(&claims());
        let forged = claims_with(&json!({"sub": "admin"}));
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
        parts[1] = b64(forged.to_string().as_bytes());
        assert!(verify(&parts.join("."), &jwks(&[&key])).is_err());
    }

    #[test]
    fn an_unknown_kid_asks_for_fresh_keys() {
        let (known, rotated) = (Key::rsa("k1"), Key::rsa("k2"));
        assert!(matches!(
            verify(&rotated.token(&claims()), &jwks(&[&known])),
            Err(Rejection::UnknownKey)
        ));
    }

    #[test]
    fn without_a_kid_only_a_lone_key_is_used() {
        let (one, two) = (Key::rsa("k1"), Key::ec("k2"));
        let token = one.sign(&json!({"alg": "RS256"}), &claims());
        assert!(verify(&token, &jwks(&[&one])).is_ok());
        assert!(refusal(&token, &jwks(&[&one, &two])).contains("several"));
    }

    #[test]
    fn alg_none_and_hmac_are_refused() {
        let key = Key::rsa("k1");
        let body = b64(claims().to_string().as_bytes());
        let none = format!("{}.{body}.", b64(br#"{"alg":"none","kid":"k1"}"#));
        assert!(verify(&none, &jwks(&[&key])).is_err());

        // HS256 keyed with something the attacker knows, such as the public key.
        let hs256 = format!(
            "{}.{body}.{}",
            b64(br#"{"alg":"HS256","kid":"k1"}"#),
            b64(&[0; 32])
        );
        assert!(refusal(&hs256, &jwks(&[&key])).contains("not accepted"));
    }

    #[test]
    fn a_token_must_use_the_algorithm_of_its_key() {
        let (rsa, ec) = (Key::rsa("k1"), Key::ec("k1"));
        // Signed with the EC key but naming the RSA key's id.
        let why = refusal(&ec.token(&claims()), &jwks(&[&rsa]));
        assert!(why.contains("RS256"), "{why}");
    }

    #[test]
    fn only_asymmetric_advertised_algorithms_are_accepted() {
        let advertised = |algs: &[&str]| {
            accepted_algorithms(&algs.iter().map(ToString::to_string).collect::<Vec<_>>())
        };
        assert_eq!(advertised(&[]), [Algorithm::RS256], "the OIDC default");
        assert_eq!(
            advertised(&["none", "HS256", "RS256", "ES256", "XY999"]),
            [Algorithm::RS256, Algorithm::ES256]
        );
        assert_eq!(advertised(&["HS256"]), Vec::<Algorithm>::new());
    }

    #[test]
    fn issuer_and_audience_must_be_ours() {
        let key = Key::rsa("k1");
        let keys = jwks(&[&key]);
        for changes in [
            json!({"iss": "https://evil.example.com"}),
            json!({"aud": "another-client"}),
            json!({"aud": ["another-client"]}),
            json!({"iss": null}),
            json!({"aud": null}),
            json!({"sub": null}),
        ] {
            assert!(
                verify(&key.token(&claims_with(&changes)), &keys).is_err(),
                "{changes}"
            );
        }
    }

    #[test]
    fn several_audiences_need_azp_naming_us() {
        let key = Key::rsa("k1");
        let keys = jwks(&[&key]);
        let token = |changes: Value| key.token(&claims_with(&changes));
        assert!(refusal(&token(json!({"aud": [CLIENT, "other"]})), &keys).contains("azp"));
        assert!(
            verify(
                &token(json!({"aud": [CLIENT, "other"], "azp": CLIENT})),
                &keys
            )
            .is_ok()
        );
        assert!(refusal(&token(json!({"azp": "other"})), &keys).contains("azp"));
        assert!(verify(&token(json!({"aud": [CLIENT]})), &keys).is_ok());
    }

    #[test]
    fn the_nonce_must_be_this_logins() {
        let key = Key::rsa("k1");
        let keys = jwks(&[&key]);
        for changes in [json!({"nonce": "another-login"}), json!({"nonce": null})] {
            let why = refusal(&key.token(&claims_with(&changes)), &keys);
            assert!(why.contains("nonce"), "{changes}: {why}");
        }
    }

    #[test]
    fn expiry_issue_and_not_before_times_allow_a_minute_of_skew() {
        let key = Key::rsa("k1");
        let keys = jwks(&[&key]);
        let token = |changes: Value| key.token(&claims_with(&changes));
        assert!(refusal(&token(json!({"exp": NOW - 61})), &keys).contains("expired"));
        assert!(verify(&token(json!({"exp": NOW - 30})), &keys).is_ok());
        assert!(refusal(&token(json!({"iat": NOW + 120})), &keys).contains("future"));
        assert!(verify(&token(json!({"iat": NOW + 30})), &keys).is_ok());
        assert!(refusal(&token(json!({"nbf": NOW + 120})), &keys).contains("not valid yet"));
        assert!(verify(&token(json!({"nbf": NOW + 30})), &keys).is_ok());
        assert!(verify(&token(json!({"exp": null})), &keys).is_err());
        assert!(verify(&token(json!({"iat": null})), &keys).is_err());
    }
}
