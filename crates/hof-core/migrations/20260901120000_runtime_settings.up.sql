CREATE TABLE IF NOT EXISTS runtime_settings (
    id BOOLEAN PRIMARY KEY DEFAULT true CHECK (id),
    indexing_paused_until TIMESTAMPTZ,
    downloads_paused_until TIMESTAMPTZ,
    max_concurrent_downloads INTEGER CHECK (max_concurrent_downloads >= 1),
    max_indexers_per_tick INTEGER CHECK (max_indexers_per_tick >= 1),
    rate_limit_delay_secs INTEGER CHECK (rate_limit_delay_secs >= 0),
    check_interval_secs INTEGER CHECK (check_interval_secs >= 1),
    cleanup_interval_secs INTEGER CHECK (cleanup_interval_secs >= 1),
    drain_timeout_secs INTEGER CHECK (drain_timeout_secs >= 1),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by TEXT REFERENCES users (id)
);

INSERT INTO runtime_settings (id) VALUES (true) ON CONFLICT (id) DO NOTHING;

CREATE OR REPLACE FUNCTION notify_runtime_settings_changed()
RETURNS TRIGGER AS $$
BEGIN
    PERFORM pg_notify('runtime_settings_changed', '');
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_runtime_settings_notify
AFTER INSERT OR UPDATE ON runtime_settings
FOR EACH ROW EXECUTE FUNCTION notify_runtime_settings_changed();
