-- OIDC identity links for SSO authentication
CREATE TABLE IF NOT EXISTS oidc_identities (
    id TEXT PRIMARY KEY,                                       -- ULID
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    issuer TEXT NOT NULL,                                          -- OIDC issuer URL
    subject TEXT NOT NULL,                                          -- OIDC 'sub' claim
    email TEXT,                                                   -- Cached from ID token
    name TEXT,                                                   -- Cached from ID token
    picture TEXT,                                                   -- Cached avatar URL
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (issuer, subject)
);

-- Index for user lookup (list identities for user)
CREATE INDEX IF NOT EXISTS idx_oidc_identities_user ON oidc_identities (user_id);

-- Index for identity lookup during login (issuer + subject)
CREATE INDEX IF NOT EXISTS idx_oidc_identities_lookup ON oidc_identities (issuer, subject);

-- Auto-update updated_at on row changes (reuses existing trigger function)
CREATE TRIGGER trg_oidc_identities_updated_at
BEFORE UPDATE ON oidc_identities
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

-- Make password_hash nullable for OIDC-only users
ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;
