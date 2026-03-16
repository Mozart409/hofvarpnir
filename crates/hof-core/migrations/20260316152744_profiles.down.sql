-- Drop trigger first
DROP TRIGGER IF EXISTS trg_profiles_updated_at ON profiles;

-- Drop table
DROP TABLE IF EXISTS profiles;

-- Drop enum type
DROP TYPE IF EXISTS quality;
