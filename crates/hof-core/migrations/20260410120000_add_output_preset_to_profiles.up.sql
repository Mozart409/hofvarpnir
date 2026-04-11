CREATE TYPE output_preset AS ENUM (
    'auto',
    'browser',
    'tv'
);

ALTER TABLE profiles
ADD COLUMN output_preset output_preset NOT NULL DEFAULT 'browser';

UPDATE profiles
SET output_preset = 'browser'
WHERE output_preset IS DISTINCT FROM 'browser';
