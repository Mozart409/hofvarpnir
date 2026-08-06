DROP INDEX IF EXISTS idx_sources_exclude_from_cleanup;

ALTER TABLE sources
DROP COLUMN IF EXISTS exclude_from_cleanup;
