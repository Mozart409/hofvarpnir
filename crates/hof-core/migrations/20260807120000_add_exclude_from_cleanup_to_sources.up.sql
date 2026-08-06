-- Allow a source to be exempted from automatic cleanup.
--
-- Videos belonging to an excluded source are never removed by retention
-- expiry or by profile quota enforcement.
ALTER TABLE sources
ADD COLUMN exclude_from_cleanup BOOLEAN NOT NULL DEFAULT false;

-- Cleanup checks this per candidate video, so keep the excluded set cheap to
-- find. Partial: the overwhelming majority of rows are `false`.
CREATE INDEX IF NOT EXISTS idx_sources_exclude_from_cleanup
ON sources (id)
WHERE exclude_from_cleanup;
