# OIDC Authentication Feature Plan

Single OIDC provider support for Hofvarpnir, configured via environment variables.

## Goals

- Allow users to authenticate via one OIDC-compliant provider (Keycloak, Auth0, Azure AD, Google, Okta, Pocket-ID, etc.)
- Configure provider via environment variables (no database config)
- Integrate seamlessly with existing session-based auth
- Link OIDC identities to existing local accounts (or auto-provision new users)
- Maintain backward compatibility with password-based login

---

## Phase 1: Database Schema

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
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (issuer, subject)
);

CREATE INDEX idx_oidc_identities_user ON oidc_identities(user_id);
CREATE INDEX idx_oidc_identities_lookup ON oidc_identities(issuer, subject);
```

### Modify Users Table

```sql
-- Make password_hash nullable for OIDC-only users
ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;
```

---

## Phase 2: Core OIDC Module (`hof-core`)

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
    pub scopes: Vec<String>,        // Default: ["openid", "profile", "email"]
    pub auto_provision: bool,       // Default: true
    pub redirect_base_url: Option<String>,  // Override for redirect URI base
    pub logout_redirect: bool,      // Default: false (RP-initiated logout)
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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

```rust
// Store in session during OIDC flow
struct OidcFlowState {
    state: String,              // CSRF token
    nonce: String,              // Replay protection
    pkce_verifier: String,      // Base64-encoded
    return_to: Option<String>,  // Post-login redirect
}
```

---

## Phase 4: Configuration

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
```

### Feature Flag

```toml
# In Cargo.toml
[features]
default = []
oidc = ["openidconnect"]
```

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
- Cache only necessary claims (`sub`, `email`, `name`) in `oidc_identities`
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

---

## Implementation Order

1. **Database migration** - `oidc_identities` table + nullable password_hash
2. **Update `.env.example`** - Add OIDC env vars with documentation
3. **Core OIDC module** - Config, client, discovery, token exchange
4. **Web auth routes** - `/auth/oidc/login`, `/auth/oidc/callback`, updated `/logout`
5. **Login page UI** - SSO button (conditional)
6. **Tests** - Integration tests with mocked provider

---

## Testing Strategy

### Unit Tests

- `OidcConfig::from_env()` parsing
- State/nonce generation

### Integration Tests

- Mock OIDC provider with `wiremock`
- Full login flow: initiate -> callback -> session created
- Auto-provision: new user created on first login
- Existing user: lookup by (issuer, subject) succeeds
- Error cases: expired token, invalid state, invalid nonce

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
