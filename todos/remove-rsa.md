# Remove `rsa` from the dependency tree

Date: 2026-11-01. Follow-up to [oidc.md](oidc.md) (the OIDC login feature this touches).

## Context

- `rsa 0.9.10` has RUSTSEC-2023-0071 (Marvin timing side channel, no fixed release).
- It pins `crypto-bigint 0.5.5`, which has RUSTSEC-2026-0387.
- Path (`cargo tree -i rsa`): `openidconnect 4.0.1` -> `hof-core` -> `hof-api`, `hof-web`.
- Goal: drop `rsa` by swapping dependencies, not by ignoring the advisories.

### What we use from `openidconnect` (all in `crates/hof-core/src/oidc/client.rs`)

- Discovery: `CoreProviderMetadata::discover_async`
- Authorization code flow with PKCE (S256), CSRF state and nonce
- Code exchange, then ID token verification (`id_token.claims(&verifier, &nonce)`: signature, `iss`, `aud`, `exp`, nonce)
- Claim extraction: `sub`, `email`, `name`, `picture` (localized claims)
- Re-exported types in `oidc/mod.rs`: `Nonce`, `PkceCodeVerifier`
- RP-initiated logout is hand-built from the discovery document, not the crate.

### Findings

- `rsa` is a mandatory, non-optional dependency of `openidconnect` (4.0.1 and the latest 4.1.0, released 2026-10-31). There is no feature flag to turn it off.
- It is used for RS256/384/512 and PS256/384/512 signature verification of the ID token (`core/jwk`), plus a test/mock RSA signer.
- Exposure: Marvin affects RSA private-key operations (decryption / signing) that an attacker can time. We only verify signatures with the provider's public key, so there is no secret to leak. Practical exposure is very low, which makes this a hygiene and audit-noise fix, not an urgent security one. Still worth doing so `cargo audit` and `cargo deny` stay clean without ignores.
- `jsonwebtoken` has an `aws_lc_rs` backend that does not pull `rsa`; its `rust_crypto` backend does. The workspace lock already contains `aws-lc-rs` and `ring`.

## Options

| | Option | Effort | Risk |
| --- | --- | --- | --- |
| A | Wait for upstream `openidconnect` to make `rsa` optional or replace it (4.1.0 did not) | none now | Open-ended; advisories stay in `cargo audit` meanwhile |
| **B** | **Recommended: drop `openidconnect`; use `oauth2` (auth code, PKCE, token exchange) + own discovery (`reqwest` + `serde`) + `jsonwebtoken` with the `aws_lc_rs` backend for ID token and JWKS verification** | Medium: about 1 module rewrite (`oidc/client.rs`, roughly 300 lines) and its tests | Medium: we own ID token validation, so it needs strict checks and tests |
| C | Fork or `[patch]` `openidconnect` with `rsa` removed | Low-medium | High maintenance burden; RS256 is what most providers use, so verification would need a replacement anyway |
| D | Another full OIDC crate (e.g. `openidconnect` alternatives such as `axum-oidc`) | Medium | Most wrap `openidconnect`, so they inherit `rsa`; not a real swap |

Why B: `oauth2` (already in the tree, no `rsa`) covers the flow parts; the only thing `openidconnect` adds that we use is discovery, claims types and JWT verification, which are small. Public-key verification with `aws-lc-rs` is constant-time-safe, maintained, and already in the lock. Keep the public API of `OidcClient` (`discover`, `authorization_url`, `exchange_code`, logout URL, `OidcClaims`) unchanged so `hof-api`/`hof-web` need no edits.

## Plan (Option B)

### Phase 0 - Spike

- [x] Re-check `cargo info openidconnect` and the upstream issue tracker for an `rsa`-free release; if one exists, switch to Option A and stop here. (2026-11-01: 4.1.0 is the latest and still depends on `rsa` unconditionally, no feature turns it off.)
- [x] Confirm `jsonwebtoken` (aws_lc_rs, `default-features = false`) builds with the pinned nightly (`nightly-2026-09-29`) and in the Nix build, and that `cargo tree -i rsa` is empty with it added.
- [x] Confirm `oauth2` features needed (`reqwest`, `rustls-tls`) and that `deny.toml` licenses/bans allow the new crates.

### Phase 1 - Discovery and JWKS

- [x] Add a typed discovery document (`issuer`, `authorization_endpoint`, `token_endpoint`, `jwks_uri`, `end_session_endpoint`, `id_token_signing_alg_values_supported`) fetched from `<issuer>/.well-known/openid-configuration` with the existing timeout and no redirects.
- [x] Verify the returned `issuer` equals the configured issuer URL exactly.
- [x] Fetch the JWKS from `jwks_uri`; cache it and refetch once on unknown `kid` (rate-limited) to survive key rotation.

### Phase 2 - Flow with `oauth2`

- [x] Rebuild `authorization_url` on `oauth2::basic::BasicClient`: S256 PKCE, CSRF state, nonce, scopes (ensure `openid` is always present).
- [x] Rebuild the code exchange with the PKCE verifier; read the raw `id_token` from the token response (extra token field).
- [x] Re-export own `Nonce` / `PkceCodeVerifier` newtypes (or `oauth2` types) so `oidc/mod.rs` consumers keep compiling.

### Phase 3 - ID token verification (security critical)

- [x] Verify with `jsonwebtoken::Validation`: pinned algorithm allowlist (RS256, ES256, EdDSA as the provider advertises; reject `none` and HMAC), `iss`, `aud` == client id, `exp`, `nbf`, small leeway.
- [x] Select the key by `kid` (and `alg`/`kty`) from the JWKS; never trust the header algorithm beyond the allowlist.
- [x] Check `nonce` equals the stored nonce; check `azp` when `aud` has several values.
- [x] Extract `sub`, `email`, `name` / `preferred_username`, `picture` (localized-claim fallback) into the existing `OidcClaims`; keep the "email required" behaviour from [oidc.md](oidc.md). (Done without the `preferred_username` fallback: `name` only, as before.)

### Phase 4 - Tests

- [x] Port `crates/hof-core/tests/oidc_client_tests.rs` (wiremock discovery + JWKS) to the new code. (`oidc_client_tests.rs` passes unchanged; the new `oidc_login_tests.rs` adds whole-login E2E tests, including `client_secret_basic` vs `client_secret_post`.)
- [x] Add negative tests: bad signature, wrong `iss`, wrong `aud`, expired, wrong nonce, `alg=none`, HS256 with the public key as secret, unknown `kid`, issuer mismatch in discovery.
- [ ] Manual login against the real provider used for testing (see Manual Testing in [oidc.md](oidc.md)); also check a provider that signs with ES256/EdDSA if available.

### Phase 5 - Remove the old dependency and verify

- [x] Remove `openidconnect` from `crates/hof-core/Cargo.toml` and update the reqwest/trust-store comment there; refresh `Cargo.lock`.
- [x] `cargo tree -i rsa` prints nothing (and `crypto-bigint 0.5.5` is gone from the lock unless another crate needs it).
- [x] `cargo audit` is clean with no ignore entries for RUSTSEC-2023-0071 / RUSTSEC-2026-0387; `cargo deny check` passes.
- [ ] `just fmt`, `just clippy` (or the project's lint recipe) and the full test suite are green on the pinned nightly; `nix develop .#ci` run passes. (2026-11-01: fmt, workspace clippy and all non-DB tests are green on the pinned nightly, and `nix build .#hofvarpnir` works; the DB suite (`just with-db ci-gate`) and `nix develop .#ci` still need to be run where podman works.)
- [x] Update the Dependencies table and notes in [oidc.md](oidc.md) (library no longer `openidconnect`).

### Follow-ups from the review (2026-11-01, not blocking)

- [ ] `hof-web` logout does not call `OidcClient::end_session_url` yet (`pages/login.rs` placeholder), so the now-discovered `end_session_endpoint` has no effect.
- [ ] The JWKS write lock is held across a refetch for up to `discovery_timeout`; give the refetch a short timeout of its own (same pattern in countdown and lanternd).
- [ ] No periodic JWKS refresh: a key the provider removes stays trusted until an unknown `kid` causes a refetch.
- [ ] No test for the refetch-after-interval path (key rotation); needs an injectable interval.
- [ ] Fail discovery clearly when the provider offers neither `client_secret_basic` nor `client_secret_post`.
