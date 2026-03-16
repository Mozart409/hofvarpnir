-- Quality enum for download quality presets
CREATE TYPE quality AS ENUM (
    'best',
    '4320p',
    '2160p',
    '1440p',
    '1080p',
    '720p',
    '480p',
    'audioonly'
);

-- Profiles table: download configuration that applies to sources from any platform
CREATE TABLE IF NOT EXISTS profiles (
    id TEXT PRIMARY KEY,  -- ULID
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    quality QUALITY NOT NULL,
    naming_template TEXT NOT NULL,
    output_dir TEXT NOT NULL,
    include_livestreams BOOLEAN NOT NULL DEFAULT false,
    include_shorts BOOLEAN NOT NULL DEFAULT false,
    storage_quota_bytes BIGINT NOT NULL,
    retention_days INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Index for user lookups
CREATE INDEX IF NOT EXISTS idx_profiles_user_id ON profiles (user_id);

-- Auto-update updated_at on row changes
CREATE TRIGGER trg_profiles_updated_at
BEFORE UPDATE ON profiles
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();
