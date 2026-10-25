-- Note: PostgreSQL does not support removing enum values.
-- Videos left in awaiting_quality are returned to pending; the enum value
-- remains but unused.
UPDATE videos SET status = 'pending', next_retry = NULL
WHERE status = 'awaiting_quality';

ALTER TABLE profiles DROP COLUMN quality_wait_hours;

ALTER TABLE videos DROP COLUMN skip_quality_gate;
