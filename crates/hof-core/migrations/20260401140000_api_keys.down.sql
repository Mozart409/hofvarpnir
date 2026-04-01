DROP TRIGGER IF EXISTS trg_api_keys_updated_at ON api_keys;
DROP TABLE IF EXISTS api_key_events;
DROP TABLE IF EXISTS api_keys;
DROP TYPE IF EXISTS api_key_event_type;
DROP TYPE IF EXISTS api_key_scope;
