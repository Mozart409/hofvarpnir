DROP INDEX IF EXISTS idx_sources_enabled;
ALTER TABLE sources DROP COLUMN enabled;
