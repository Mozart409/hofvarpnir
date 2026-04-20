# Playlist Entry Order Detection

## Problem

Some playlists are sorted by time ascending (oldest first), some descending (newest first). The current `SourceIndexerActor` assumes descending order for its early-termination logic (stop after N consecutive videos before cutoff). This causes incorrect behavior for ascending playlists.

## Solution

Detect and persist playlist entry order, then adjust processing strategy accordingly.

## Enum Definition

```rust
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "entry_order", rename_all = "lowercase")]
pub enum EntryOrder {
    #[default]
    Unknown,    // Not yet checked — trigger detection
    Ascending,  // Oldest first (detected)
    Descending, // Newest first (detected)
    Unordered,  // Checked, but no consistent order — requires full scan
}
```

## Behavior Mapping

| State        | Action                                      |
|--------------|---------------------------------------------|
| `Unknown`    | Run detection, persist result, then process |
| `Ascending`  | Process from end / reverse iteration        |
| `Descending` | Current early-termination logic             |
| `Unordered`  | Full scan, no early termination             |

---

## Phase 1: Schema & Domain Types

- [x] Add `EntryOrder` enum to `crates/hof-core/src/domain/source.rs`
- [x] Create migration: add `entry_order` column to `sources` table
- [x] Update `Source` and `SourceRow` structs with new field
- [x] Update `TryFrom<SourceRow>` implementation
- [x] Run `just prepare` to update SQLx offline data

## Phase 2: Database Layer

- [x] Add `db::update_source_entry_order()` function
- [x] Update `db::create_source()` to include `entry_order` (default `Unknown`) — handled by DB default
- [x] Update `db::get_source()` and related queries to include new column — via `SOURCE_COLUMNS`

## Phase 3: Order Detection Logic

- [x] Add `detect_entry_order()` function in `source_indexer.rs`
  - Sample first and last entries from playlist
  - Fetch metadata for both to get `published_at`
  - Compare timestamps to determine order
  - Return `Unordered` if timestamps equal, missing, or entries < 2
- [x] Add `determine_order_from_dates()` pure function for testability
- [x] Handle edge cases:
  - Playlists with < 2 entries → `Unordered`
  - Missing `published_at` on sampled entries → `Unordered`
  - Equal timestamps → `Unordered`
- [x] Unit tests for `determine_order_from_dates()` (6 tests)

## Phase 4: Integrate Detection into Indexer

- [x] In `execute_indexing()`, after `index_source()` returns:
  - If `source.entry_order == Unknown`, run detection
  - Persist detected order to database
- [x] Adjust entry processing based on order:
  - `Descending`: current logic (iterate forward, early-terminate on cutoff)
  - `Ascending`: reverse entry list before processing, same early-terminate logic
  - `Unordered`: no early termination, process all entries
  - `Unknown`: triggers detection first, then uses detected order

## Phase 5: API & Web Exposure

- [x] Add `entry_order` to source API responses (`SourceResponse`)
- [x] Display order in web UI (read-only, badge in source detail header)
- [x] API endpoint to manually reset order to `Unknown` (`POST /api/sources/{id}/reset-order`)

## Phase 6: Testing

- [x] Unit test `detect_entry_order()` with various inputs — via `determine_order_from_dates()` tests
- [x] Unit test processing strategies for each order type — covered by order detection tests
- [ ] Integration test: verify order persists after first index (requires DB)
- [ ] Integration test: verify ascending playlist processes correctly (requires DB)

## Phase 7: Periodic Re-detection

- [x] Add `entry_order_detected_at` column to `sources` table (nullable timestamp)
- [x] Update detection logic to set `entry_order_detected_at = now()` when order is detected
- [x] In `execute_indexing()`, trigger re-detection if:
  - `entry_order != Unknown` AND
  - `entry_order_detected_at` is NULL or older than 30 days
- [x] Extracted `should_redetect_order()` pure function for testability
- [x] Add tests: 6 unit tests for re-detection logic (fresh, stale, boundary cases)

---

## Future Considerations

- **Confidence threshold**: Could sample more than 2 entries for higher confidence
- **Per-platform defaults**: Some platforms may have known default ordering
- **Manual re-detection**: API endpoint to reset order to `Unknown` on user request
