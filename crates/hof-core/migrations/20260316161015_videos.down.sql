-- Drop join table first (depends on both sources and videos)
DROP TABLE IF EXISTS source_videos;

-- Drop trigger
DROP TRIGGER IF EXISTS trg_videos_updated_at ON videos;

-- Drop table
DROP TABLE IF EXISTS videos;

-- Drop enum type
DROP TYPE IF EXISTS video_status;
