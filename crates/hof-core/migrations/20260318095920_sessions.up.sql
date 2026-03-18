-- Sessions table for tower-sessions-sqlx-store
-- Note: The actual table creation is handled by PostgresStore::migrate()
-- This migration is a no-op placeholder to maintain migration history
-- The table schema is:
--   id TEXT PRIMARY KEY NOT NULL
--   data BYTEA NOT NULL
--   expiry_date TIMESTAMPTZ NOT NULL
SELECT 1;
