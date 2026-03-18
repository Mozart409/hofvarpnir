-- Add channel metadata columns for Jellyfin integration
ALTER TABLE sources ADD COLUMN channel_id TEXT;
ALTER TABLE sources ADD COLUMN channel_title TEXT;
ALTER TABLE sources ADD COLUMN channel_description TEXT;
ALTER TABLE sources ADD COLUMN channel_thumbnail_url TEXT;

-- Track when Jellyfin metadata was last generated (NULL = never)
ALTER TABLE sources ADD COLUMN jellyfin_metadata_at TIMESTAMPTZ;
