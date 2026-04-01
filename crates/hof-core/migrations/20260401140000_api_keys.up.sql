-- API key scope enum
CREATE TYPE api_key_scope AS ENUM ('read', 'write', 'delete');

-- API key event type enum
CREATE TYPE api_key_event_type AS ENUM ('created', 'rolled', 'deleted');

-- API keys table
CREATE TABLE IF NOT EXISTS api_keys (
    id TEXT PRIMARY KEY,               -- ULID
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,                  -- user-chosen label ("CI bot", "backup script")
    prefix TEXT NOT NULL,                  -- first 12 chars of token (for display: hof_sk_Ab3xY…)
    key_hash TEXT NOT NULL UNIQUE,           -- SHA-256 hash of the full token
    scopes API_KEY_SCOPE [] NOT NULL,       -- e.g. {read, write}
    expires_at TIMESTAMPTZ,                    -- NULL = never expires
    last_used_at TIMESTAMPTZ,                    -- updated on each authenticated request
    last_used_ip TEXT,                           -- IP of last successful use
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_api_keys_user_id ON api_keys (user_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_prefix ON api_keys (prefix);
CREATE UNIQUE INDEX IF NOT EXISTS idx_api_keys_user_id_name ON api_keys (user_id, name);

CREATE TRIGGER trg_api_keys_updated_at
BEFORE UPDATE ON api_keys
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

-- API key events table (audit log)
CREATE TABLE IF NOT EXISTS api_key_events (
    id TEXT PRIMARY KEY,               -- ULID
    api_key_id TEXT NOT NULL,                  -- no FK — key may be deleted
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    event_type API_KEY_EVENT_TYPE NOT NULL,
    ip_address TEXT,                           -- optional, for audit
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_api_key_events_api_key_id ON api_key_events (api_key_id);
CREATE INDEX IF NOT EXISTS idx_api_key_events_user_id ON api_key_events (user_id);
