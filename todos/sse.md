# SSE Real-Time Updates Plan

## Overview

Add per-page SSE endpoints for **Dashboard**, **Downloads**, and **Activity** pages. Each endpoint streams HTML fragments that htmx swaps into the DOM when server-side state changes.

## Architecture

### Broadcast Channels (added to `AppState`)

| Channel | Type | Purpose | Capacity |
|---------|------|---------|----------|
| `progress_tx` | `broadcast::Sender<DownloadProgress>` | **Existing** — live download progress bars | 1000 |
| `activity_tx` | `broadcast::Sender<ActivityEvent>` | New activity events (wraps every `log_activity` call) | 256 |
| `invalidate_tx` | `broadcast::Sender<()>` | Lightweight "something changed" signal for dashboard | 256 |

- `activity_tx` is published from inside `log_activity` itself, so all 15+ call sites get SSE for free.
- `invalidate_tx` is published from: profile CRUD, source CRUD, download status changes, and alongside every `activity_tx` send. This avoids coupling dashboard rendering to the activity schema.

### SSE Endpoints

| Page | Endpoint | Trigger | Response |
|------|----------|---------|----------|
| Dashboard | `/web/dashboard/events` | `invalidate_tx` | Re-queries DB, sends full metric cards grid + recent downloads table as named SSE events |
| Downloads | `/web/downloads/events` | `invalidate_tx` | Re-renders the current downloads list page (respects active filters/page via query params from client) |
| Activity | `/web/activity/events` | `activity_tx` | Re-renders the current activity list page (respects active filters/page via query params from client) |

> Download progress (`/web/downloads/progress`) stays as-is — it already works.

### Batching / Debouncing

All three new SSE endpoints debounce rapid events with a fixed **500ms coalesce window**.

Strategy: drain-and-coalesce. On each event, start a 500ms timer. Consume any additional events that arrive during the window, then do a single DB query + HTML render. This prevents the scheduler indexing 50 videos from firing 50 separate SSE pushes.

### htmx Wiring

Each page uses `hx-ext="sse"` with **named events** so htmx knows which DOM target to swap:

**Dashboard** (`/dashboard`):
```html
<div hx-ext="sse" sse-connect="/web/dashboard/events">
  <div id="dashboard-metrics" sse-swap="dashboard-update" hx-swap="innerHTML">
    <!-- metric cards + recent downloads rendered here -->
  </div>
</div>
```
SSE sends: `event: dashboard-update\ndata: <full metrics + recent table HTML>\n\n`

**Downloads** (`/downloads`):
```html
<div hx-ext="sse" sse-connect="/web/downloads/events?status=...&search=...&page=...&per_page=...">
  <div id="downloads-list" sse-swap="downloads-update" hx-swap="innerHTML">
    <!-- downloads list rendered here -->
  </div>
</div>
```
The SSE endpoint reads the query params from the connection URL, so it re-renders the same filtered/paginated view the user is looking at. When the user navigates (filter, search, paginate), the htmx partial still works as today — the SSE connection just provides live refresh on top.

**Activity** (`/activity`):
```html
<div hx-ext="sse" sse-connect="/web/activity/events?severity=...&page=...&per_page=...">
  <div id="activity-content" sse-swap="activity-update" hx-swap="innerHTML">
    <!-- activity list rendered here -->
  </div>
</div>
```
Same pattern: query params from the connection URL drive the re-render.

---

## Implementation Steps

### Phase 1: Broadcast Infrastructure ✅

**1.1 — Add `activity_tx` and `invalidate_tx` to `AppState`** ✅
- File: `crates/hof-api/src/lib.rs`
- Added `ActivityBroadcaster` struct and `broadcaster` field to `AppState`.

**1.2 — Create channels in `main.rs`** ✅
- File: `crates/hof-web/src/main.rs`
- `ActivityBroadcaster` is created in `startup::initialize` and exposed on `ActorSystem`; `main.rs` clones it into `AppState`.

**1.3 — Wrap `log_activity` to broadcast** ✅
- File: `crates/hof-core/src/db/activity.rs`
- `ActivityBroadcaster` struct with `activity_tx: broadcast::Sender<()>` and `invalidate_tx: broadcast::Sender<()>` (capacity 256 each).
- `log_and_broadcast()` method: calls `log_activity` then sends on both channels (errors ignored).
- All 8 actor-side `db::log_activity` calls replaced with `self.broadcaster.log_and_broadcast()`.

**1.4 — Publish invalidation signals** ✅
- Web handlers in `pages.rs`: all 6 `db::log_activity` calls replaced with `state.broadcaster.log_and_broadcast()`; `invalidate()` added to `update_profile`, `update_source`, `retry_download`, `cancel_download` success paths.

### Phase 2: SSE Endpoints ✅

**2.1 — Dashboard SSE (`/web/dashboard/events`)** ✅
- File: `crates/hof-web/src/pages.rs`
- New handler `dashboard_events_sse` subscribes to `invalidate_tx`.
- On each (debounced) signal: query profiles count, sources count, video status counts, recent 8 videos.
- Render the metric cards grid + recent downloads table into an HTML fragment.
- Send as `Event::default().event("dashboard-update").data(fragment)`.
- Extracted `dashboard_metrics_markup()` shared with `dashboard_page`.

**2.2 — Downloads SSE (`/web/downloads/events`)** ✅
- File: `crates/hof-web/src/pages.rs`
- New handler `downloads_events_sse` accepts `DownloadsQuery` and subscribes to `invalidate_tx`.
- On each (debounced) signal: re-run the same query as `downloads_list_partial` with the query params from the SSE connection URL.
- Send as `Event::default().event("downloads-update").data(fragment)`.

**2.3 — Activity SSE (`/web/activity/events`)** ✅
- File: `crates/hof-web/src/pages.rs`
- New handler `activity_events_sse` accepts `ActivityQuery` and subscribes to `activity_tx`.
- On each (debounced) signal: re-run the same query as `activity_list_partial` with the query params.
- Send as `Event::default().event("activity-update").data(fragment)`.

**2.4 — Register routes** ✅
- File: `crates/hof-web/src/pages.rs` (`router` fn)
- Added:
  - `.route("/web/dashboard/events", get(dashboard_events_sse))`
  - `.route("/web/downloads/events", get(downloads_events_sse))`
  - `.route("/web/activity/events", get(activity_events_sse))`

### Phase 3: htmx Page Updates ✅

**3.1 — Dashboard page markup** ✅
- Wrapped metric cards + recent downloads in an SSE-connected container.
- Extracted content into `dashboard_metrics_markup()` shared with the SSE handler.
- Named event `dashboard-update` with `sse-swap` on `#dashboard-metrics`.

**3.2 — Downloads page markup** ✅
- Added SSE connection wrapping the `#downloads-list` container.
- SSE connect URL (`/web/downloads/events?...`) includes current filter/search/page query params via `downloads_events_url()`.
- Named event `downloads-update` replaces the list content.
- Existing progress SSE (`/web/downloads/progress`) stays untouched.

**3.3 — Activity page markup** ✅
- Added SSE connection wrapping the `#activity-content` container.
- SSE connect URL (`/web/activity/events?...`) includes current severity/page query params via `activity_events_url()`.
- Named event `activity-update` replaces the content.

### Phase 4: Debounce Utility ✅

**4.1 — Implement debounced broadcast stream** ✅
- `debounced_broadcast(rx: broadcast::Receiver<()>) -> impl Stream<Item = ()>` in `pages.rs`.
- Uses `futures::stream::unfold` — no new dependencies.
- On first signal, starts 500ms coalesce window (`SSE_COALESCE_WINDOW`). Drains any additional signals then yields one item.
- Returns `None` (ends stream) only when the channel is closed.
- All three SSE handlers use this wrapper.

---

## Edge Cases & Notes

- **SSE reconnection**: htmx SSE extension auto-reconnects on disconnect. On reconnect, the client gets a fresh render immediately (first event after subscribe + debounce window).
- **No subscribers**: `broadcast::send` returns `Err` when there are no receivers — this is fine, we already ignore it for `progress_tx`.
- **Page navigation**: When the user changes filters or pages via htmx partials, the SSE connection URL doesn't change (it was set on page load). Acceptable for v1 — the htmx partial updates are the primary interaction; SSE is for background refresh.
- **Auth**: SSE endpoints require `AuthUser` extraction like all other protected routes.
- **KeepAlive**: All SSE endpoints use 15-second keepalive (matching existing progress endpoint).
- **Memory**: Broadcast channels use capacity 256. Lagged receivers skip events gracefully.
