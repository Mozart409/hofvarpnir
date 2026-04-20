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

- [ ] Add `detect_entry_order()` function in `source_indexer.rs`
  - Sample first and last entries from playlist
  - Fetch metadata for both to get `published_at`
  - Compare timestamps to determine order
  - Return `Unordered` if timestamps equal, missing, or entries < 2
- [ ] Handle edge cases:
  - Playlists with < 2 entries → `Unordered`
  - Missing `published_at` on sampled entries → `Unordered`
  - Equal timestamps → `Unordered`

## Phase 4: Integrate Detection into Indexer

- [ ] In `execute_indexing()`, after `index_source()` returns:
  - If `source.entry_order == Unknown`, run detection
  - Persist detected order to database
- [ ] Adjust entry processing based on order:
  - `Descending`: current logic (iterate forward, early-terminate on cutoff)
  - `Ascending`: reverse entry list before processing, same early-terminate logic
  - `Unordered`: no early termination, process all entries
  - `Unknown`: should not reach processing (detection runs first)

## Phase 5: API & Web Exposure

- [ ] Add `entry_order` to source API responses (`SourceResponse`)
- [ ] Display order in web UI (read-only, informational)
- [ ] Optional: Add API endpoint to manually reset order to `Unknown` (trigger re-detection)

## Phase 6: Testing

- [ ] Unit test `detect_entry_order()` with various inputs
- [ ] Unit test processing strategies for each order type
- [ ] Integration test: verify order persists after first index
- [ ] Integration test: verify ascending playlist processes correctly

## Phase 7: Periodic Re-detection

- [ ] Add `entry_order_detected_at` column to `sources` table (nullable timestamp)
- [ ] Update detection logic to set `entry_order_detected_at = now()` when order is detected
- [ ] In `execute_indexing()`, trigger re-detection if:
  - `entry_order != Unknown` AND
  - `entry_order_detected_at` is NULL or older than 30 days
- [ ] On re-detection, reset `entry_order` to `Unknown`, run detection, persist new result
- [ ] Add test: verify re-detection triggers after 30 days

---

## Future Considerations

- **Confidence threshold**: Could sample more than 2 entries for higher confidence
- **Per-platform defaults**: Some platforms may have known default ordering
- **Manual re-detection**: API endpoint to reset order to `Unknown` on user request
