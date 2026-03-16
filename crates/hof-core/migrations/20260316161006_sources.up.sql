-- Source type enum
CREATE TYPE source_type AS ENUM ('channel', 'playlist');

-- Sources table: channels or playlists to monitor
CREATE TABLE IF NOT EXISTS sources (
    id TEXT PRIMARY KEY,  -- ULID
    profile_id TEXT NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    source_type SOURCE_TYPE NOT NULL,
    custom_name TEXT,
    index_frequency_secs BIGINT NOT NULL,
    cutoff_date DATE NOT NULL,
    retention_days INTEGER,
    last_indexed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Index for profile lookups
CREATE INDEX IF NOT EXISTS idx_sources_profile_id ON sources (profile_id);

-- Index for scheduler to find sources due for indexing
CREATE INDEX IF NOT EXISTS idx_sources_last_indexed_at ON sources (last_indexed_at);

-- Auto-update updated_at on row changes
CREATE TRIGGER trg_sources_updated_at
BEFORE UPDATE ON sources
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();
