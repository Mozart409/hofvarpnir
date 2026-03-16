-- Drop trigger first
DROP TRIGGER IF EXISTS trg_sources_updated_at ON sources;

-- Drop table
DROP TABLE IF EXISTS sources;

-- Drop enum type
DROP TYPE IF EXISTS source_type;
