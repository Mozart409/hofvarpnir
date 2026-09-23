-- Clear the `unordered` entry-order verdicts that detection latched by mistake.
--
-- `detect_entry_order` used to answer `Unordered` whenever its two-point date
-- comparison could not discriminate -- fewer than two entries, a metadata
-- fetch that errored, an entry with no publish date, or two entries sharing a
-- timestamp -- and the indexer persisted that as a verdict.
-- `update_source_entry_order` stamps `entry_order_detected_at = NOW()` for any
-- value other than `unknown`, so a failed lookup became indistinguishable from
-- a real detection: it suppressed re-detection for 30 days and disabled early
-- termination, forcing a full scan of every entry on every index.
--
-- Detection no longer produces `unordered` at all -- an inconclusive run now
-- persists nothing -- so every row still carrying it holds a stale verdict
-- that can never be refreshed on its own. Resetting to `unknown` with a null
-- timestamp is what the indexer keys re-detection off, so the next index
-- works the order out for real.
UPDATE sources
SET
    entry_order = 'unknown',
    entry_order_detected_at = NULL
WHERE entry_order = 'unordered';
