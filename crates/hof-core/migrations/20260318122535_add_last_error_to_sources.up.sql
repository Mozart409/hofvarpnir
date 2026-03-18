-- Add last_error column to track indexing failures
ALTER TABLE sources ADD COLUMN last_error TEXT;

-- Add index_error_count to track consecutive failures
ALTER TABLE sources ADD COLUMN index_error_count INTEGER NOT NULL DEFAULT 0;
