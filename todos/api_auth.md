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
    key_hash    TEXT NOT NULL,                  -- SHA-256 hash of the full token
    scopes      api_key_scope[] NOT NULL,       -- e.g. {read, write}
    expires_at  TIMESTAMPTZ,                    -- NULL = never expires
    last_used_at TIMESTAMPTZ,                   -- updated on each authenticated request
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_api_keys_user_id ON api_keys (user_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_prefix  ON api_keys (prefix);
CREATE INDEX IF NOT EXISTS idx_api_keys_key_hash ON api_keys (key_hash);  -- hot path for auth lookups
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

### New dependency

Add `sha2` to `hof-core/Cargo.toml` (already has `rand` via argon2).

---

## Phase 4: Database Operations (`hof-core`)

### File: `crates/hof-core/src/db/api_key.rs`

Functions:

| Function | Description |
|---|---|
| `create_api_key(pool, user_id, name, prefix, key_hash, scopes, expires_at) -> ApiKey` | Insert key + log `created` event |
| `list_api_keys(pool, user_id) -> Vec<ApiKey>` | All keys for a user (never returns hash) |
| `get_api_key_by_hash(pool, key_hash) -> Option<ApiKey>` | Lookup for auth middleware |
| `touch_api_key_last_used(pool, key_id)` | Update `last_used_at` (fire-and-forget, don't block request) |
| `roll_api_key(pool, key_id, new_prefix, new_key_hash, new_expires_at) -> ApiKey` | Replace hash + prefix, log `rolled` event |
| `delete_api_key(pool, key_id, user_id)` | Delete key, log `deleted` event |
| `list_api_key_events(pool, api_key_id) -> Vec<ApiKeyEvent>` | Lifecycle events for a key |

Register module in `crates/hof-core/src/db/mod.rs`.

---

## Phase 5: API Auth Middleware (`hof-api`)

### File: `crates/hof-api/src/auth.rs` (new)

#### Extractor: `ApiAuth`

```rust
pub struct ApiAuth {
    pub user_id: Ulid,
    pub scopes: Vec<ApiKeyScope>,
}
```

Implements `FromRequestParts<AppState>`:

1. Check `Authorization: Bearer hof_sk_...` header.
2. SHA-256 hash the token, look up via `get_api_key_by_hash`.
3. Check expiration (`expires_at` is `NULL` or in the future).
4. Spawn background task to `touch_api_key_last_used`.
5. Return `ApiAuth { user_id, scopes }` or `401 Unauthorized`.

#### Scope guard helper

```rust
impl ApiAuth {
    pub fn require_scope(&self, scope: ApiKeyScope) -> Result<(), ApiError> { ... }
}
```

Returns `403 Forbidden` with a message like `"API key missing required scope: write"`.

### Middleware layer for `/api` and `/docs`

In `crates/hof-api/src/lib.rs`:

- Create an Axum middleware (or use `from_fn`) that runs before route handlers.
- **Skip check** if the request already has a valid session (web UI bypass). Do this by attempting session extraction first — if `AuthUser` succeeds, pass through without requiring an API key.
- **Exempt** `/api/health/*` endpoints from auth (needed for monitoring/probes).
- For all other `/api/*` and `/docs/*` routes, require a valid API key.

### Integrate into route handlers

Each handler calls `auth.require_scope(ApiKeyScope::Read)` (or Write/Delete) at the top. Mapping:

| HTTP method / action | Required scope |
|---|---|
| GET (list, get, SSE) | `Read` |
| POST (create), PUT/PATCH (update), triggers (index, metadata, cleanup, retry) | `Write` |
| DELETE | `Delete` |

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

// 403 Forbidden - missing scope
{"error": "insufficient_scope", "message": "API key missing required scope: write"}
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

## Phase 7: Scalar / Docs Auth

In `main.rs`, the `/docs` nest currently uses `hof_api::scalar_router()` which has no state or middleware.

**Change:** Wrap the `/docs` routes with the same API auth middleware from Phase 5. Since the middleware passes through session-authenticated requests, logged-in web users can still browse docs without a key.

Update `scalar_router()` to accept `AppState` so the middleware can access the DB pool for key lookups.

---

## Phase 8: SQLx Offline Data

After all migrations and queries are written:

```bash
just prepare
# or: cargo sqlx prepare --workspace --check -- --all-targets --all-features
```

This regenerates the `.sqlx/` offline query data so CI can build without a live database.

---

## Phase 9: Test Coverage

### Unit tests (`hof-core`)

| Test | Location | Description |
|---|---|---|
| `test_generate_api_key_format` | `auth.rs` | Verify token format: `hof_sk_` prefix + 32 alphanumeric chars |
| `test_generate_api_key_uniqueness` | `auth.rs` | Generate 1000 keys, assert all unique |
| `test_hash_api_key_consistency` | `auth.rs` | Same input → same hash |
| `test_prefix_extraction` | `auth.rs` | Verify prefix is first 12 chars of token |

### Integration tests (`hof-api`)

| Test | Description |
|---|---|
| `test_api_auth_valid_key` | Valid Bearer token returns `ApiAuth` with correct scopes |
| `test_api_auth_invalid_token` | Wrong token returns 401 |
| `test_api_auth_expired_key` | Expired key returns 401 |
| `test_api_auth_missing_header` | No Authorization header returns 401 |
| `test_api_auth_malformed_header` | Bad format (e.g., `Basic ...`) returns 401 |
| `test_api_scope_guard_read` | Key with `read` scope passes `require_scope(Read)` |
| `test_api_scope_guard_missing` | Key without `write` fails `require_scope(Write)` with 403 |
| `test_api_auth_session_bypass` | Session-authenticated request bypasses API key check |
| `test_api_health_no_auth` | `/api/health` works without any auth |

### Handler tests (`hof-web`)

| Test | Description |
|---|---|---|
| `test_create_api_key_success` | Form submit creates key, returns full token once |
| `test_create_api_key_duplicate_name` | Duplicate name returns error |
| `test_create_api_key_no_scopes` | At least one scope required |
| `test_roll_api_key` | Roll replaces hash, old token no longer works |
| `test_delete_api_key` | Delete removes key, auth fails |
| `test_list_api_keys` | List shows all keys for user (never shows hash) |

---

## File Change Summary

| File | Action |
|---|---|
| `crates/hof-core/migrations/YYYYMMDD_api_keys.up.sql` | New — schema |
| `crates/hof-core/migrations/YYYYMMDD_api_keys.down.sql` | New — rollback |
| `crates/hof-core/src/domain/api_key.rs` | New — domain types |
| `crates/hof-core/src/domain/mod.rs` | Edit — add `pub mod api_key` |
| `crates/hof-core/src/auth.rs` | Edit — add key generation + SHA-256 hashing |
| `crates/hof-core/src/db/api_key.rs` | New — CRUD queries |
| `crates/hof-core/src/db/mod.rs` | Edit — add `mod api_key` + re-export |
| `crates/hof-core/Cargo.toml` | Edit — add `sha2` dependency |
| `crates/hof-api/src/auth.rs` | New — `ApiAuth` extractor + scope guard + middleware |
| `crates/hof-api/src/lib.rs` | Edit — wire middleware, update OpenAPI security scheme, update `scalar_router` |
| `crates/hof-api/src/routes/*.rs` | Edit — add scope checks to each handler |
| `crates/hof-web/src/pages.rs` | Edit — add NavItem::ApiKeys, key management page + htmx routes |
| `crates/hof-web/src/main.rs` | Edit — pass state to scalar_router, adjust router nesting |
| `crates/hof-core/src/auth.rs` | Edit — add unit tests for key generation |
| `crates/hof-api/src/auth.rs` | Edit — add integration tests for auth middleware |
| `crates/hof-web/src/pages.rs` | Edit — add handler tests for key management |

---

## Implementation Order

1. **Phase 1** — Migration (schema must exist before anything else)
2. **Phase 2** — Domain types (needed by db layer)
3. **Phase 3** — Key generation (needed by db + API layers)
4. **Phase 4** — DB operations (needed by middleware + web page)
5. **Phase 5** — API middleware + scope enforcement (core auth feature)
6. **Phase 6** — Web UI page (depends on db ops being ready)
7. **Phase 7** — Scalar auth (small wiring change, can be done with Phase 5)
8. **Phase 8** — SQLx prepare (final step after all queries compile)
9. **Phase 9** — Tests (run alongside each phase, final verification)

---

## Open Decisions (non-blocking, decide during implementation)

- **Rate limiting on auth failures**: Not in scope for v1, but worth adding later to prevent brute-force.
- **Key count limit per user**: Consider capping at ~20 keys per user to prevent abuse.
- **Expired key cleanup**: Let expired keys accumulate (audit trail) or add periodic cleanup job.
