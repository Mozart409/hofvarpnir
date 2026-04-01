# API Key Authentication — Implementation Plan

## Overview

Add API key authentication with independent scopes (`read`, `write`, `delete`) to all `/api/*` and `/docs/*` routes. Web UI (session-based auth) bypasses API key checks. Users manage keys from a new web page with create, roll, delete, and activity viewing.

---

## Phase 1: Database Schema

### Migration: `api_keys` table

```sql
-- Scope enum
CREATE TYPE api_key_scope AS ENUM ('read', 'write', 'delete');

CREATE TABLE IF NOT EXISTS api_keys (
    id          TEXT PRIMARY KEY,               -- ULID
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,                  -- user-chosen label ("CI bot", "backup script")
    prefix      TEXT NOT NULL,                  -- first 12 chars of token (for display: hof_sk_Ab3xY…)
    key_hash    TEXT NOT NULL UNIQUE,           -- SHA-256 hash of the full token (unique prevents duplicates)
    scopes      api_key_scope[] NOT NULL,       -- e.g. {read, write}
    expires_at  TIMESTAMPTZ,                    -- NULL = never expires
    last_used_at TIMESTAMPTZ,                   -- updated on each authenticated request
    last_used_ip TEXT,                          -- IP of last successful use (anomaly detection)
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_api_keys_user_id ON api_keys (user_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_prefix  ON api_keys (prefix);
-- idx_api_keys_key_hash removed — UNIQUE constraint provides an index automatically
CREATE UNIQUE INDEX IF NOT EXISTS idx_api_keys_user_id_name ON api_keys (user_id, name);  -- enforce unique names per user

CREATE TRIGGER trg_api_keys_updated_at
BEFORE UPDATE ON api_keys
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();
```

### Migration: `api_key_events` table

```sql
CREATE TYPE api_key_event_type AS ENUM ('created', 'rolled', 'deleted');

CREATE TABLE IF NOT EXISTS api_key_events (
    id          TEXT PRIMARY KEY,               -- ULID
    api_key_id  TEXT NOT NULL,                  -- don't FK — key may be deleted
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    event_type  api_key_event_type NOT NULL,
    ip_address  TEXT,                           -- optional, for audit
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_api_key_events_api_key_id ON api_key_events (api_key_id);
CREATE INDEX IF NOT EXISTS idx_api_key_events_user_id    ON api_key_events (user_id);
```

### Cleanup strategy for orphaned events

Events reference deleted keys by `api_key_id` (no FK). To prevent unbounded growth:

- **No periodic job in v1** — events are small and infrequent.
- **On key deletion**, delete all associated events in the same transaction as the key delete (`DELETE FROM api_key_events WHERE api_key_id = $1`). This keeps the table bounded to active keys only.
- **Future**: Add a `DELETE CASCADE`-style periodic cleanup if events accumulate (e.g., delete events older than 180 days whose key no longer exists).

### Down migration

```sql
DROP TABLE IF EXISTS api_key_events;
DROP TABLE IF EXISTS api_keys;
DROP TYPE IF EXISTS api_key_event_type;
DROP TYPE IF EXISTS api_key_scope;
```

---

## Phase 2: Domain Types (`hof-core`)

### File: `crates/hof-core/src/domain/api_key.rs`

```rust
pub enum ApiKeyScope { Read, Write, Delete }      // sqlx::Type, Serialize, Deserialize, ToSchema
pub enum ApiKeyEventType { Created, Rolled, Deleted }

pub struct ApiKey {
    pub id: Ulid,
    pub user_id: Ulid,
    pub name: String,
    pub prefix: String,             // "hof_sk_Ab3x" (display only, never the full key)
    pub scopes: Vec<ApiKeyScope>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct ApiKeyEvent {
    pub id: Ulid,
    pub api_key_id: Ulid,
    pub user_id: Ulid,
    pub event_type: ApiKeyEventType,
    pub ip_address: Option<String>,
    pub created_at: DateTime<Utc>,
}
```

Plus `ApiKeyRow` / `ApiKeyEventRow` with `TryFrom` conversions (same pattern as `ActivityEventRow`).

Register module in `crates/hof-core/src/domain/mod.rs`.

---

## Phase 3: Key Generation & Hashing (`hof-core`)

### File: `crates/hof-core/src/auth.rs` (extend existing)

Add functions:

- **`generate_api_key() -> (String, String, String)`** — returns `(full_token, prefix, sha256_hash)`.
  - Format: `hof_sk_<32 random alphanumeric chars>` (total ~39 chars).
  - Use `rand::thread_rng` + `rand::distributions::Alphanumeric`.
  - Prefix: first 12 chars of the token (`hof_sk_XXXXX`).
  - Hash: `sha2::Sha256` hex digest of the full token. (SHA-256 is fine for high-entropy random tokens; no need for Argon2.)
- **`hash_api_key(token: &str) -> String`** — SHA-256 hex digest. Used for lookup on incoming requests.

### Timing-safe comparison

Use `hmac::Mac::verify_slice` or `subtle::ConstantTimeEq` when comparing the incoming token hash against the stored hash to prevent timing side-channel attacks. Since we hash the token first and compare hex digests, add a constant-time equality check:

```rust
use subtle::ConstantTimeEq;

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}
```

Apply this when comparing the computed hash against the database value before accepting a key.

### New dependencies

Add `sha2` and `subtle` to `hof-core/Cargo.toml` (already has `rand` via argon2).

---

## Phase 4: Database Operations (`hof-core`)

### File: `crates/hof-core/src/db/api_key.rs`

Functions:

| Function | Description |
|---|---|
| `create_api_key(pool, user_id, name, prefix, key_hash, scopes, expires_at) -> ApiKey` | Insert key + log `created` event |
| `list_api_keys(pool, user_id) -> Vec<ApiKey>` | All keys for a user (never returns hash) |
| `get_api_key_by_hash(pool, key_hash) -> Option<ApiKey>` | Lookup for auth middleware (use constant-time comparison) |
| `touch_api_key_last_used(pool, key_id, ip)` | Update `last_used_at` + `last_used_ip` (best-effort spawn) |
| `roll_api_key(pool, key_id, new_prefix, new_key_hash, new_expires_at) -> ApiKey` | Replace hash + prefix, log `rolled` event |
| `delete_api_key(pool, key_id, user_id)` | Delete key + associated events in same transaction, log `deleted` event |
| `list_api_key_events(pool, api_key_id) -> Vec<ApiKeyEvent>` | Lifecycle events for a key |

Register module in `crates/hof-core/src/db/mod.rs`.

---

## Phase 5: Unified Auth Extractor (`hof-api`)

### File: `crates/hof-api/src/auth.rs` (new)

#### Unified `Auth` enum extractor

```rust
pub enum Auth {
    /// Session-based authentication (web UI users)
    Session { user_id: Ulid },
    /// API key authentication
    ApiKey { user_id: Ulid, scopes: Vec<ApiKeyScope> },
}

impl Auth {
    /// Returns the authenticated user ID regardless of auth method.
    pub fn user_id(&self) -> Ulid {
        match self {
            Auth::Session { user_id } | Auth::ApiKey { user_id, .. } => *user_id,
        }
    }

    /// Returns scopes if authenticated via API key, None for session auth.
    /// Session-authenticated users have full access (no scope restrictions).
    pub fn scopes(&self) -> Option<&[ApiKeyScope]> {
        match self {
            Auth::ApiKey { scopes, .. } => Some(scopes),
            Auth::Session { .. } => None,
        }
    }

    /// Require a specific scope. Returns `Ok(())` for session auth or
    /// if the API key has the required scope.
    pub fn require_scope(&self, scope: ApiKeyScope) -> Result<(), ApiError> {
        match self.scopes() {
            None => Ok(()), // session auth: full access
            Some(scopes) if scopes.contains(&scope) => Ok(()),
            Some(_) => Err(ApiError::InsufficientScope(scope)),
        }
    }
}
```

Implements `FromRequestParts<AppState>`:

1. **Try session auth first** — attempt to extract `AuthUser` from cookies/session store.
2. **If session valid** → return `Auth::Session { user_id }`.
3. **If no session** → check `Authorization: Bearer hof_sk_...` header.
4. **If Bearer token present** → SHA-256 hash, look up via `get_api_key_by_hash` (constant-time comparison), check expiration.
5. **If key valid** → spawn best-effort background task to `touch_api_key_last_used` (logs errors, doesn't block request).
6. **If neither session nor valid key** → return `401 Unauthorized`.

This eliminates double-auth overhead: session check is a fast cookie lookup; API key lookup only runs when no session exists.

### Rate limiting middleware

Add a simple rate limiter on auth failures to prevent brute-force:

- Track failed API key lookups by IP using `tower_governor` or `governor` crate.
- Limit: 10 failed attempts per minute per IP.
- Return `429 Too Many Requests` when exceeded.
- Apply only to the auth extraction path, not general API routes.

### Scope guard in handlers

Each handler calls `auth.require_scope(ApiKeyScope::Read)` (or Write/Delete) at the top. Mapping:

| HTTP method / action | Required scope |
|---|---|
| GET (list, get, SSE) | `Read` |
| POST (create), PUT/PATCH (update), triggers (index, metadata, cleanup, retry) | `Write` |
| DELETE | `Delete` |

### Middleware layer for `/api`

In `crates/hof-api/src/lib.rs`:

- The `Auth` extractor handles authentication per-request — no separate middleware layer needed.
- **Exempt** `/api/health/*` endpoints from auth by not using the `Auth` extractor in those handlers.
- All other `/api/*` handlers take `Auth` as an extractor parameter.

### OpenAPI security scheme

Add to `ApiDoc`:
```rust
#[openapi(
    security(("api_key" = [])),
    components(
        // existing schemas...
    ),
    modifiers(&SecurityAddon),
)]
```

Define `SecurityAddon` to add `ApiKeyAuth` (HTTP Bearer) scheme so Scalar shows the auth input.

### Error response format

All auth errors return JSON with consistent structure:

```json
// 401 Unauthorized - invalid or missing token
{"error": "invalid_api_key", "message": "API key not found or invalid"}

// 401 Unauthorized - token expired
{"error": "api_key_expired", "message": "API key has expired"}

// 401 Unauthorized - no auth provided
{"error": "unauthorized", "message": "Authentication required"}

// 403 Forbidden - missing scope
{"error": "insufficient_scope", "message": "API key missing required scope: write"}

// 429 Too Many Requests - rate limited
{"error": "rate_limited", "message": "Too many failed authentication attempts"}
```

---

## Phase 6: Web UI — API Keys Page (`hof-web`)

### NavItem

Add `ApiKeys` variant to the `NavItem` enum in `pages.rs`.

### Routes (in `pages::router`)

| Route | Method | Description |
|---|---|---|
| `/settings/api-keys` | GET | Full page: list all keys for current user |
| `/settings/api-keys` | POST | Create new key (form submit) |
| `/settings/api-keys/:id/roll` | POST | Roll key (htmx) |
| `/settings/api-keys/:id` | DELETE | Delete key (htmx) |
| `/settings/api-keys/:id/events` | GET | htmx partial: lifecycle events for a key |

All routes require `AuthUser` (session-based).

### Page layout

**Create form** (top of page):
- Name: text input (required)
- Scopes: checkboxes for `read`, `write`, `delete` (at least one required)
- Expiration: dropdown — 90 days (default), 180 days, 365 days, forever
- Submit button: "Generate Key"

**After creation** — flash/modal showing the full token once:
- Warning: "Copy this key now. You won't be able to see it again."
- Copyable code block with the full `hof_sk_...` token.

**Key list table** (below form):
- Columns: Name | Prefix | Scopes (badges) | Expires | Last used (relative time) | Created | Actions
- "Last used" shows relative time: "3m ago", "2h ago", "5d ago", or "Never"
- Actions: Roll (confirm dialog) | Delete (confirm dialog) | Expand events (htmx swap)

**Events panel** (expandable per key, loaded via htmx):
- Simple list: `created — 2026-04-01 12:00` / `rolled — 2026-04-01 14:30`

### Maud templates

Follow existing patterns in `pages.rs` — use the `layout()` / `shell()` helpers, htmx attributes for partials, Tailwind classes for styling.

---

## Phase 7: Scalar / Docs (no auth)

The `/docs` routes serve the Scalar OpenAPI UI and its static assets (JS, CSS). These do **not** require authentication — they are a documentation browser, not an API.

**No changes needed** to `scalar_router()` — it remains a stateless router serving static content. The auth is enforced on `/api/*` endpoints via the `Auth` extractor (Phase 5). Users browsing docs can see the API spec; actual API calls require authentication via the Bearer token input in Scalar.

---

## Phase 8: Per-Endpoint OpenAPI Scope Documentation

Update each `#[utoipa::path(...)]` macro to document the required security scope per endpoint. This ensures Scalar/OpenAPI consumers know which scope a key needs.

```rust
#[utoipa::path(
    get,
    path = "/api/videos",
    security(
        ("api_key" = ["read"]),
    ),
    responses(
        (status = 200, description = "List of videos", body = Vec<Video>),
    ),
)]
```

For session-only endpoints (web UI), no `security()` attribute is needed. For API endpoints, add the appropriate scope: `read`, `write`, or `delete`.

---

## Phase 9: SQLx Offline Data

After all migrations and queries are written:

```bash
just prepare
# or: cargo sqlx prepare --workspace --check -- --all-targets --all-features
```

This regenerates the `.sqlx/` offline query data so CI can build without a live database.

---

## Phase 10: Test Coverage

### Unit tests (`hof-core`)

| Test | Location | Description |
|---|---|---|
| `test_generate_api_key_format` | `auth.rs` | Verify token format: `hof_sk_` prefix + 32 alphanumeric chars |
| `test_generate_api_key_uniqueness` | `auth.rs` | Generate 1000 keys, assert all unique |
| `test_hash_api_key_consistency` | `auth.rs` | Same input → same hash |
| `test_prefix_extraction` | `auth.rs` | Verify prefix is first 12 chars of token |
| `test_constant_time_eq` | `auth.rs` | Verify constant-time comparison produces correct results |

### Integration tests (`hof-api`)

| Test | Description |
|---|---|
| `test_auth_session_returns_session_variant` | Valid session returns `Auth::Session` |
| `test_auth_api_key_returns_api_key_variant` | Valid Bearer token returns `Auth::ApiKey` with correct scopes |
| `test_auth_no_credentials_returns_401` | No session, no header returns 401 |
| `test_auth_invalid_token_returns_401` | Wrong token returns 401 |
| `test_auth_expired_key_returns_401` | Expired key returns 401 |
| `test_auth_malformed_header_returns_401` | Bad format (e.g., `Basic ...`) returns 401 |
| `test_scope_guard_session_bypass` | `Auth::Session.require_scope(Write)` returns `Ok(())` |
| `test_scope_guard_api_key_has_scope` | Key with `read` scope passes `require_scope(Read)` |
| `test_scope_guard_api_key_missing_scope` | Key without `write` fails with 403 |
| `test_api_health_no_auth` | `/api/health` works without any auth |
| `test_auth_rate_limit` | 10+ failed attempts from same IP returns 429 |

### Concurrent and edge case tests

| Test | Description |
|---|---|
| `test_concurrent_requests_same_key` | Multiple concurrent requests with same key don't cause race conditions on `touch` |
| `test_auth_key_exactly_at_expiration` | Key at exact expiration boundary is rejected |
| `test_touch_best_effort_no_block` | Request completes even if `touch` spawn fails |

### Handler tests (`hof-web`)

| Test | Description |
|---|---|
| `test_create_api_key_success` | Form submit creates key, returns full token once |
| `test_create_api_key_duplicate_name` | Duplicate name returns error |
| `test_create_api_key_no_scopes` | At least one scope required |
| `test_roll_api_key` | Roll replaces hash, old token no longer works |
| `test_delete_api_key` | Delete removes key and events, auth fails |
| `test_list_api_keys` | List shows all keys for user (never shows hash) |

---

## File Change Summary

| File | Action |
|---|---|
| `crates/hof-core/migrations/YYYYMMDD_api_keys.up.sql` | New — schema |
| `crates/hof-core/migrations/YYYYMMDD_api_keys.down.sql` | New — rollback |
| `crates/hof-core/src/domain/api_key.rs` | New — domain types |
| `crates/hof-core/src/domain/mod.rs` | Edit — add `pub mod api_key` |
| `crates/hof-core/src/auth.rs` | Edit — add key generation, SHA-256 hashing, constant-time comparison |
| `crates/hof-core/src/db/api_key.rs` | New — CRUD queries |
| `crates/hof-core/src/db/mod.rs` | Edit — add `mod api_key` + re-export |
| `crates/hof-core/Cargo.toml` | Edit — add `sha2`, `subtle` dependencies |
| `crates/hof-api/src/auth.rs` | New — unified `Auth` enum extractor, scope guard, rate limiting |
| `crates/hof-api/src/lib.rs` | Edit — wire rate limiter, update OpenAPI security scheme |
| `crates/hof-api/src/routes/*.rs` | Edit — add scope checks to each handler, update per-endpoint OpenAPI docs |
| `crates/hof-api/Cargo.toml` | Edit — add `governor` or `tower_governor` dependency |
| `crates/hof-web/src/pages.rs` | Edit — add NavItem::ApiKeys, key management page + htmx routes |
| `crates/hof-core/src/auth.rs` | Edit — add unit tests for key generation |
| `crates/hof-api/src/auth.rs` | Edit — add integration tests for auth extractor |
| `crates/hof-web/src/pages.rs` | Edit — add handler tests for key management |

---

## Implementation Order

1. **Phase 1** — Migration (schema must exist before anything else)
2. **Phase 2** — Domain types (needed by db layer)
3. **Phase 3** — Key generation + constant-time comparison (needed by db + API layers)
4. **Phase 4** — DB operations (needed by extractor + web page)
5. **Phase 5** — Unified `Auth` extractor + rate limiting + scope enforcement (core auth feature)
6. **Phase 6** — Web UI page (depends on db ops being ready)
7. **Phase 7** — Scalar/docs (no changes, no auth needed)
8. **Phase 8** — Per-endpoint OpenAPI scope documentation
9. **Phase 9** — SQLx prepare (final step after all queries compile)
10. **Phase 10** — Tests (run alongside each phase, final verification)

---

## Open Decisions (non-blocking, decide during implementation)

- **Rate limiting on auth failures**: Not in scope for v1, but worth adding later to prevent brute-force.
- **Key count limit per user**: Consider capping at ~20 keys per user to prevent abuse.
- **Expired key cleanup**: Let expired keys accumulate (audit trail) or add periodic cleanup job.
