-- Deliberately a no-op.
--
-- The `unordered` values the up migration cleared were wrong, and which rows
-- carried them is recorded nowhere, so there is nothing to put back. The
-- column itself is untouched: the next index re-detects `entry_order` for any
-- source sitting at `unknown`.
SELECT 1;
