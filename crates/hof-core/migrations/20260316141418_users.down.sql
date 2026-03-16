-- Drop trigger first (depends on table and function)
DROP TRIGGER IF EXISTS trg_users_updated_at ON users;

-- Drop table
DROP TABLE IF EXISTS users;

-- Drop the shared trigger function (only if no other tables use it)
-- Note: This will fail if other tables still have triggers using this function.
-- In practice, this function should only be dropped in the final migration teardown.
DROP FUNCTION IF EXISTS update_updated_at_column();
