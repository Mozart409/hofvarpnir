-- Remove error tracking columns from sources
ALTER TABLE sources DROP COLUMN IF EXISTS last_error;
ALTER TABLE sources DROP COLUMN IF EXISTS index_error_count;
