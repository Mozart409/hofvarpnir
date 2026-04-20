CREATE TYPE entry_order AS ENUM (
    'unknown',
    'ascending',
    'descending',
    'unordered'
);

ALTER TABLE sources
ADD COLUMN entry_order entry_order NOT NULL DEFAULT 'unknown';
