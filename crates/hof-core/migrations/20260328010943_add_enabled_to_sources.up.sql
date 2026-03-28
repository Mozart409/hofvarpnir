-- Add enabled column to allow pausing indexing/downloading for specific sources
ALTER TABLE sources ADD COLUMN enabled BOOLEAN NOT NULL DEFAULT true;

-- Index for efficient filtering of enabled sources during scheduler queries
CREATE INDEX IF NOT EXISTS idx_sources_enabled ON sources (enabled);
