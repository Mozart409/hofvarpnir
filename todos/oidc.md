# OIDC Authentication Feature Plan

Single OIDC provider support for Hofvarpnir, configured via environment variables.

## Goals

- Allow users to authenticate via one OIDC-compliant provider (Keycloak, Auth0, Azure AD, Google, Okta, Pocket-ID, etc.)
- Configure provider via environment variables (no database config)
- Integrate seamlessly with existing session-based auth
- Link OIDC identities to existing local accounts (or auto-provision new users)
- Maintain backward compatibility with password-based login

---

## Phase 1: Database Schema ✅

### New Table

```sql
-- User OIDC identity links
CREATE TABLE oidc_identities (
    id              TEXT PRIMARY KEY,           -- ULID
    user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    issuer          TEXT NOT NULL,              -- OIDC issuer URL (for validation)
    subject         TEXT NOT NULL,              -- OIDC 'sub' claim (unique per issuer)
    email           TEXT,                       -- Cached from ID token
    name            TEXT,                       -- Cached from ID token
    picture         TEXT,                       -- Cached avatar URL from ID token
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (issuer, subject)
);

CREATE INDEX idx_oidc_identities_user ON oidc_identities(user_id);
CREATE INDEX idx_oidc_identities_lookup ON oidc_identities(issuer, subject);

-- Auto-update updated_at on row change
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ language 'plpgsql';

CREATE TRIGGER update_oidc_identities_updated_at
    BEFORE UPDATE ON oidc_identities
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
```

### Modify Users Table

```sql
-- Make password_hash nullable for OIDC-only users
ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;

-- Ensure user has at least one auth method (password OR oidc identity)
-- Note: This is enforced at application level, not DB constraint,
-- because it requires cross-table check (user must have password_hash OR oidc_identity)
```

### Auth Method Tracking (Optional)

Consider adding to users table for clearer auth state:

```sql
-- Track primary auth method (informational, not enforced)
ALTER TABLE users ADD COLUMN auth_method TEXT DEFAULT 'password';
-- Values: 'password', 'oidc', 'both'
```

Decision: Skip for MVP. The presence of `password_hash` and `oidc_identities` rows is sufficient.

---

## Phase 2: Core OIDC Module (`hof-core`) ✅

### New Files

```
crates/hof-core/src/
├── oidc/
│   ├── mod.rs           -- Module exports
│   ├── config.rs        -- OidcConfig from env vars
│   ├── identity.rs      -- OidcIdentity domain type
│   ├── client.rs        -- OIDC client (discovery, token exchange)
│   └── error.rs         -- OidcError enum
```

### Dependencies to Add

```toml
# In crates/hof-core/Cargo.toml
openidconnect = "4"       # OIDC client library (handles discovery, PKCE, token validation)
```

### Configuration (from env vars)

```rust
// config.rs
/// OIDC provider configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub scopes: Vec<String>,            // Default: ["openid", "profile", "email"]
    pub auto_provision: bool,           // Default: true
    pub redirect_base_url: Option<String>,  // Override for redirect URI base
    pub logout_redirect: bool,          // Default: false (RP-initiated logout)
    pub discovery_timeout: Duration,    // Default: 30s
}

impl OidcConfig {
    /// Load from environment variables. Returns None if OIDC is not configured.
    pub fn from_env() -> Option<Self>;
    
    /// Build the redirect URI for callbacks.
    pub fn redirect_uri(&self, request_base: &str) -> String;
}
```

### Domain Type

```rust
// identity.rs
pub struct OidcIdentity {
    pub id: Ulid,
    pub user_id: Ulid,
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub picture: Option<String>,    // Avatar URL from ID token
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

### Error Type

```rust
// error.rs
#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("OIDC not configured")]
    NotConfigured,
    #[error("Discovery failed: {0}")]
    DiscoveryFailed(String),
    #[error("Invalid state parameter")]
    InvalidState,
    #[error("Invalid nonce")]
    InvalidNonce,
    #[error("Token exchange failed: {0}")]
    TokenExchangeFailed(String),
    #[error("Invalid ID token: {0}")]
    InvalidToken(String),
    #[error("Missing required claim: {0}")]
    MissingClaim(String),
    #[error("Account not found and auto-provision disabled")]
    AccountNotFound,
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
}
```

### OIDC Client

```rust
// client.rs
pub struct OidcClient {
    config: OidcConfig,
    inner: openidconnect::CoreClient,
}

impl OidcClient {
    /// Discover provider metadata and build client. Call once at startup.
    pub async fn discover(config: OidcConfig) -> Result<Self, OidcError>;

    /// Generate authorization URL with PKCE
    pub fn authorization_url(&self, state: &str) -> (Url, CsrfToken, Nonce, PkceVerifier);

    /// Exchange authorization code for tokens
    pub async fn exchange_code(
        &self,
        code: &str,
        pkce_verifier: PkceVerifier,
    ) -> Result<TokenResponse, OidcError>;

    /// Extract claims from ID token (already validated during exchange)
    pub fn claims(&self, token: &IdToken) -> Result<Claims, OidcError>;
}
```

### Database Functions

```rust
// db/oidc.rs
pub async fn get_identity_by_subject(pool: &PgPool, issuer: &str, subject: &str) -> Result<Option<OidcIdentity>>;
pub async fn create_identity(pool: &PgPool, identity: &OidcIdentity) -> Result<()>;
pub async fn get_identities_for_user(pool: &PgPool, user_id: Ulid) -> Result<Vec<OidcIdentity>>;
pub async fn delete_identity(pool: &PgPool, id: Ulid, user_id: Ulid) -> Result<bool>;
```

---

## Phase 3: Web Routes (`hof-web`)

### New Routes

| Method | Path                 | Description                                      |
| ------ | -------------------- | ------------------------------------------------ |
| GET    | `/login`             | Login page with OIDC button (if configured)      |
| GET    | `/auth/oidc/login`   | Initiate OIDC flow (redirect to provider)        |
| GET    | `/auth/oidc/callback`| Handle provider callback                         |
| POST   | `/logout`            | Logout (redirects to provider if configured)     |

### Flow: OIDC Login

```
1. User clicks "Login with SSO" on /login
2. GET /auth/oidc/login
   - Generate PKCE verifier + challenge
   - Generate state token (CSRF protection)
   - Store (state, pkce_verifier, nonce) in session
   - Redirect to provider authorization endpoint
3. User authenticates with provider
4. Provider redirects to GET /auth/oidc/callback?code=...&state=...
   - Validate state matches session
   - Exchange code for tokens using PKCE verifier
   - Validate ID token signature and claims (nonce, iss, aud, exp)
   - Extract email claim (required - fail if missing)
   - Look up oidc_identities by (issuer, sub)
   - If found: log in as linked user
   - If not found:
     a. Look up user by email
     b. If user exists: link OIDC identity to existing user, log in
     c. If no user AND auto_provision: create user + identity, log in
     d. If no user AND !auto_provision: show "account not found" error
   - Create session, redirect to /
```

### Session State for OIDC

Uses existing `tower-sessions` with PostgreSQL backend (`tower-sessions-sqlx-store`).

```rust
// Store in session during OIDC flow (key: "oidc_flow")
#[derive(Serialize, Deserialize)]
struct OidcFlowState {
    state: String,              // CSRF token
    nonce: String,              // Replay protection
    pkce_verifier: String,      // Base64-encoded (~64 bytes)
    return_to: Option<String>,  // Post-login redirect
    created_at: DateTime<Utc>,  // For expiration check
}
```

**Session size**: ~200 bytes for OIDC flow state. Well within session limits.

**Cleanup**: 
- Flow state expires after 5 minutes (check `created_at` in callback)
- Remove `oidc_flow` key from session after successful/failed callback
- Abandoned flows cleaned up by session expiration (existing tower-sessions behavior)

---

## Phase 4: Configuration ✅

### Environment Variables

```bash
# Required (OIDC disabled if not set)
OIDC_ISSUER=https://auth.example.com        # Provider issuer URL
OIDC_CLIENT_ID=hofvarpnir                   # Client ID from provider
OIDC_CLIENT_SECRET=secret                   # Client secret from provider

# Optional
OIDC_SCOPES=openid,profile,email            # Default: openid profile email
OIDC_AUTO_PROVISION=true                    # Default: true (create user on first login)
OIDC_REDIRECT_BASE_URL=https://hof.example.com  # Default: derived from request
OIDC_LOGOUT_REDIRECT=true                   # Default: false (redirect to provider on logout)
OIDC_DISCOVERY_TIMEOUT=30                   # Default: 30 seconds
```

### `.env.example` Addition

```bash
# OIDC Authentication (optional - disabled if OIDC_ISSUER not set)
# OIDC_ISSUER=https://auth.example.com
# OIDC_CLIENT_ID=hofvarpnir
# OIDC_CLIENT_SECRET=your-client-secret
# OIDC_SCOPES=openid,profile,email           # Default: openid profile email
# OIDC_AUTO_PROVISION=true                   # Create user on first OIDC login (default: true)
# OIDC_REDIRECT_BASE_URL=https://hof.example.com  # Override callback URL base
# OIDC_LOGOUT_REDIRECT=false                 # Redirect to provider on logout (default: false)
# OIDC_DISCOVERY_TIMEOUT=30                  # Discovery HTTP timeout in seconds (default: 30)
```

### Always Enabled

OIDC module is always compiled in. Runtime behavior controlled by env vars:
- If `OIDC_ISSUER` is set: OIDC login enabled, SSO button shown
- If `OIDC_ISSUER` is not set: OIDC disabled, password-only login

---

## Phase 5: UI Components (`hof-web`)

### Login Page Updates

- Show "Login with SSO" button if OIDC is configured
- Keep password form for users with local credentials
- Conditional rendering based on `OidcConfig::from_env().is_some()`

### Account Settings Page (Future)

- Show linked OIDC identity (issuer + email)
- Allow unlinking if user has password set

---

## Phase 6: Security Considerations

### Token Handling

- Never store raw ID tokens long-term
- Cache only necessary claims (`sub`, `email`, `name`, `picture`) in `oidc_identities`
- Validate `iss`, `aud`, `exp`, `iat`, `nonce` claims
- `openidconnect` crate handles signature verification automatically

### PKCE

- Always use PKCE (S256 challenge method)
- Store verifier in server-side session, never in URL/cookie

### State Parameter

- Generate cryptographically random state
- Bind to session to prevent CSRF
- Short expiration (5 minutes)

### Secret Storage

- `OIDC_CLIENT_SECRET` managed by deployment platform (K8s secrets, Docker secrets, etc.)
- Never log or expose in error messages

### Rate Limiting

- `/auth/oidc/login`: 10 requests/minute per IP (prevent redirect flood)
- `/auth/oidc/callback`: 20 requests/minute per IP (allow retries)
- Use existing rate limiting infrastructure if available, or `tower-governor`

### Audit Logging

Log OIDC events with `tracing` for security monitoring:

```rust
// Successful login
info!(
    user_id = %user.id,
    issuer = %identity.issuer,
    subject = %identity.subject,
    "OIDC login successful"
);

// Failed login
warn!(
    issuer = %config.issuer_url,
    error = %e,
    "OIDC login failed"
);

// New account provisioned
info!(
    user_id = %user.id,
    email = %user.email,
    issuer = %identity.issuer,
    "OIDC user auto-provisioned"
);

// Identity linked to existing account
info!(
    user_id = %user.id,
    email = %user.email,
    issuer = %identity.issuer,
    "OIDC identity linked to existing user"
);
```

---

## Implementation Order

1. ~~**Database migration**~~ - `oidc_identities` table + nullable password_hash ✅
2. ~~**Update `.env.example`**~~ - Add OIDC env vars with documentation ✅
3. ~~**Core OIDC module**~~ - Config, client, discovery, token exchange ✅
4. **Web auth routes** - `/auth/oidc/login`, `/auth/oidc/callback`, updated `/logout`
5. **Login page UI** - SSO button (conditional)
6. **Tests** - Integration tests with mocked provider

---

## Testing Strategy

### Unit Tests

- `OidcConfig::from_env()` parsing
- State/nonce generation
- Flow state expiration check

### Integration Tests

Mock OIDC provider with `wiremock`:

```rust
// Required mock endpoints:
// 1. Discovery: GET /.well-known/openid-configuration
// 2. JWKS: GET /jwks (public keys for token verification)
// 3. Token: POST /token (code exchange)
// 4. (Optional) Userinfo: GET /userinfo
```

**Test cases:**
- Full login flow: initiate -> callback -> session created
- Auto-provision: new user created on first login
- Email match: OIDC identity linked to existing user
- Existing identity: lookup by (issuer, subject) succeeds
- Error cases:
  - Expired token
  - Invalid state
  - Invalid nonce
  - Missing email claim
  - JWKS verification failure (invalid signature)
  - Discovery timeout

### Manual Testing

- Test with real provider (Pocket-ID, Keycloak, or Google)

---

## Dependencies

| Crate           | Purpose             | License |
| --------------- | ------------------- | ------- |
| `openidconnect` | OIDC client library | MIT     |

Note: `openidconnect` is MIT licensed, which is in the allowed list in `deny.toml`.

---

## Design Decisions

1. **Logout**: RP-initiated logout supported via `OIDC_LOGOUT_REDIRECT=true` env var. When enabled, logout redirects to provider's `end_session_endpoint`.

2. **Auto-provision defaults**:
   - `email`: **Required** - fail login if missing (request `email` scope)
   - `name`: Fall back to email username (part before `@`) if `name`/`preferred_username` claims missing

3. **Account linking**: On first OIDC login, if a user with matching email exists, link the OIDC identity to that account (instead of creating a new user). This allows existing password users to transition to SSO.

4. **Password for OIDC-only users**: OIDC-only users (no password_hash) can set a password later via account settings. This gives them a fallback if OIDC provider is unavailable.

5. **Auth method enforcement**: At application level, prevent deleting the last auth method (can't unlink OIDC if no password, can't remove password if no OIDC identity).
