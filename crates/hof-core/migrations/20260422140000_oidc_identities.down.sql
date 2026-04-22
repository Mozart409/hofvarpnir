-- Revert: make password_hash NOT NULL again (set empty string for any NULLs first)
UPDATE users SET password_hash = '' WHERE password_hash IS NULL;
ALTER TABLE users ALTER COLUMN password_hash SET NOT NULL;

-- Drop OIDC identities table and related objects
DROP TRIGGER IF EXISTS trg_oidc_identities_updated_at ON oidc_identities;
DROP INDEX IF EXISTS idx_oidc_identities_lookup;
DROP INDEX IF EXISTS idx_oidc_identities_user;
DROP TABLE IF EXISTS oidc_identities;
