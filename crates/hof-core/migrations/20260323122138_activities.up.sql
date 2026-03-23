-- Activity event severity
CREATE TYPE activity_severity AS ENUM ('info', 'success', 'warning', 'error');

-- Activity event types
CREATE TYPE activity_event_type AS ENUM (
    'source_indexed',
    'source_error',
    'download_started',
    'download_completed',
    'download_failed',
    'retry_scheduled',
    'metadata_generated',
    'video_cleaned',
    'profile_created',
    'profile_updated',
    'profile_deleted',
    'source_created',
    'source_deleted'
);

-- Activity events table
CREATE TABLE IF NOT EXISTS activity_events (
    id TEXT PRIMARY KEY,  -- ULID
    event_type ACTIVITY_EVENT_TYPE NOT NULL,
    severity ACTIVITY_SEVERITY NOT NULL,
    message TEXT NOT NULL,
    source_id TEXT REFERENCES sources (id) ON DELETE SET NULL,
    video_id TEXT REFERENCES videos (id) ON DELETE SET NULL,
    profile_id TEXT REFERENCES profiles (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Index for reverse-chronological listing
CREATE INDEX IF NOT EXISTS idx_activity_events_created_at ON activity_events (created_at DESC);

-- Index for filtering by severity
CREATE INDEX IF NOT EXISTS idx_activity_events_severity ON activity_events (severity);

-- Index for filtering by event type
CREATE INDEX IF NOT EXISTS idx_activity_events_event_type ON activity_events (event_type);

-- Index for source-specific activity
CREATE INDEX IF NOT EXISTS idx_activity_events_source_id ON activity_events (source_id);
