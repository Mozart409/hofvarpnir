-- Video status enum
CREATE TYPE video_status AS ENUM (
    'pending',
    'downloading',
    'completed',
    'failed',
    'skipped',
    'cleaned',
    'permanently_failed'
);

-- Videos table: global, deduplicated by (platform, platform_video_id)
CREATE TABLE IF NOT EXISTS videos (
    id TEXT PRIMARY KEY,  -- ULID
    platform TEXT NOT NULL,     -- yt-dlp extractor name (e.g. "youtube", "vimeo")
    platform_video_id TEXT NOT NULL,     -- e.g. YouTube video ID
    title TEXT NOT NULL,
    description TEXT,
    duration_secs BIGINT,
    published_at TIMESTAMPTZ,
    thumbnail_url TEXT,
    status VIDEO_STATUS NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry TIMESTAMPTZ,
    last_error TEXT,
    file_path TEXT,
    file_size_bytes BIGINT,
    downloaded_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Unique constraint for deduplication
    CONSTRAINT uq_videos_platform_video UNIQUE (platform, platform_video_id)
);

-- Index for finding videos by status (for download queue, cleanup, etc.)
CREATE INDEX IF NOT EXISTS idx_videos_status ON videos (status);

-- Index for retry scheduling
CREATE INDEX IF NOT EXISTS idx_videos_next_retry ON videos (next_retry) WHERE next_retry IS NOT NULL;

-- Index for cleanup by downloaded_at (retention policy)
CREATE INDEX IF NOT EXISTS idx_videos_downloaded_at ON videos (downloaded_at) WHERE downloaded_at IS NOT NULL;

-- Auto-update updated_at on row changes
CREATE TRIGGER trg_videos_updated_at
BEFORE UPDATE ON videos
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

-- Join table: links sources to videos (many-to-many)
CREATE TABLE IF NOT EXISTS source_videos (
    source_id TEXT NOT NULL REFERENCES sources (id) ON DELETE CASCADE,
    video_id TEXT NOT NULL REFERENCES videos (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    PRIMARY KEY (source_id, video_id)
);

-- Index for finding all videos for a source
CREATE INDEX IF NOT EXISTS idx_source_videos_source_id ON source_videos (source_id);

-- Index for finding all sources that reference a video (for retention policy)
CREATE INDEX IF NOT EXISTS idx_source_videos_video_id ON source_videos (video_id);
