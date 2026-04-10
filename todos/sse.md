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

All three new SSE endpoints use **adaptive debouncing** with exponential backoff for optimal UX and performance.

**Strategy**: Adaptive window with event coalescing
- **Initial window**: 200ms for responsive feel
- **Rapid events**: Double to 400ms, max 800ms based on event frequency
- **Smart grouping**: Similar events (e.g., multiple video status changes) coalesce into single updates
- **Event prioritization**: User-visible updates (status changes) take precedence over background events

Implementation uses `tokio::time::sleep` + drain loop with adaptive timing based on event arrival patterns.

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

### Phase 1: Broadcast Infrastructure

**1.1 — Add `activity_tx` and `invalidate_tx` to `AppState`**
- File: `crates/hof-api/src/lib.rs`
- Add two new `broadcast::Sender` fields and constructor params.

**1.2 — Create channels in `main.rs`**
- File: `crates/hof-web/src/main.rs`
- Create `broadcast::channel` for activity and invalidate alongside the existing progress channel.
- Pass both to `AppState::new`.

**1.3 — Wrap `log_activity` to broadcast**
- File: `crates/hof-core/src/db/activity.rs`
- Add an optional `broadcast::Sender<ActivityEvent>` parameter to `log_activity` (or create a new `log_activity_with_broadcast` and rename the old one to keep backward compat during migration).
- After successful DB insert, send the returned `ActivityEvent` on `activity_tx`.
- Preferred approach: add a new `ActivityBroadcaster` struct (holds both `activity_tx` and `invalidate_tx`) that gets passed through `AppState` and into actors. `log_activity` stays as-is (pure DB), and a new `log_and_broadcast_activity` function calls `log_activity` + sends on both channels.

**1.4 — Publish invalidation signals**
- Anywhere a profile, source, or video is created/updated/deleted, send `()` on `invalidate_tx`.
- Call sites in `crates/hof-web/src/pages.rs`: profile CRUD handlers, source CRUD handlers, download retry/cancel/delete handlers.
- Call sites in actors: `download_supervisor.rs` (status transitions), `scheduler.rs` (new videos discovered), `cleanup.rs` (videos cleaned).
- The `log_and_broadcast_activity` function also sends on `invalidate_tx` automatically, so any activity event implicitly invalidates the dashboard.

### Phase 2: SSE Endpoints

**2.1 — Dashboard SSE (`/web/dashboard/events`)**
- File: `crates/hof-web/src/pages.rs`
- New handler `dashboard_events_sse` subscribes to `invalidate_tx` with adaptive debouncing.
- On each (debounced) signal: query profiles count, sources count, video status counts, recent 8 videos.
- **Error handling**: If database query fails, send error indicator event and retry with exponential backoff.
- Render the metric cards grid + recent downloads table into an HTML fragment.
- Send as `Event::default().event("dashboard-update").data(fragment)`.
- Extract the dashboard metrics + recent table rendering into a reusable function (shared with `dashboard_page`).
- **Fallback**: On template rendering error, send placeholder HTML with error message.

**2.2 — Downloads SSE (`/web/downloads/events`)**
- File: `crates/hof-web/src/pages.rs`
- New handler `downloads_events_sse` accepts `DownloadsQuery` and subscribes to `invalidate_tx` with adaptive debouncing.
- **Error handling**: Implement retry logic for database failures, send error events on persistent failures.
- On each (debounced) signal: re-run the same query as `downloads_list_partial` with the query params from the SSE connection URL.
- **Connection validation**: Validate query params and send error event if invalid parameters detected.
- Send as `Event::default().event("downloads-update").data(fragment)`.

**2.3 — Activity SSE (`/web/activity/events`)**
- File: `crates/hof-web/src/pages.rs`
- New handler `activity_events_sse` accepts `ActivityQuery` and subscribes to `activity_tx` with adaptive debouncing.
- **Error handling**: Handle database query failures gracefully with retry logic.
- On each (debounced) signal: re-run the same query as `activity_list_partial` with the query params.
- **Event filtering**: Coalesce similar activity events to prevent spam (e.g., batch multiple video status changes).
- Send as `Event::default().event("activity-update").data(fragment)`.

**2.4 — Register routes**
- File: `crates/hof-web/src/pages.rs` (`router` fn)
- Add:
  - `.route("/web/dashboard/events", get(dashboard_events_sse))`
  - `.route("/web/downloads/events", get(downloads_events_sse))`
  - `.route("/web/activity/events", get(activity_events_sse))`

### Phase 3: htmx Page Updates + Client-Side SSE Management

**3.1 — Dashboard page markup**
- Wrap metric cards + recent downloads in an SSE-connected container.
- Extract the inner content into a function so both the initial render and the SSE handler share it.
- Use named event `dashboard-update` with `sse-swap`.
- **Client-side reconnection**: Add JavaScript to monitor htmx navigation and reconnect SSE with updated parameters.

**3.4 — Client-side SSE parameter synchronization**
- Add JavaScript that listens for htmx filter/search/pagination events
- On parameter changes, disconnect and reconnect SSE with updated query params
- Implement reconnection backoff to prevent thundering herd
- Store current SSE connection state to prevent duplicate connections

**3.2 — Downloads page markup**
- Add SSE connection to the downloads list container.
- The SSE connect URL includes current filter/search/page query params.
- Named event `downloads-update` replaces the list content.
- Existing progress SSE stays untouched.

**3.3 — Activity page markup**
- Add SSE connection to the activity content container.
- The SSE connect URL includes current severity/page query params.
- Named event `activity-update` replaces the content.

### Phase 4: Debounce Utility

**4.1 — Implement adaptive debounced broadcast stream**
- Create a helper function that takes a `broadcast::Receiver<T>` and returns a `Stream` with adaptive timing (200ms → 400ms → 800ms based on event frequency).
- Implementation: Track event arrival patterns and adjust window dynamically. Group similar events by type/content.
- Include error recovery: If template rendering fails, send error indicator and continue stream.
- All three SSE handlers use this wrapper for consistent behavior.

---

## Error Handling & Resilience

**Database Failures**:
- Implement retry logic with exponential backoff for all database queries
- Send error indicator events to clients when persistent failures occur
- Fallback to cached data or placeholder content when available
- Log errors with tracing for monitoring

**Template Rendering Errors**:
- Wrap all template rendering in error handling
- Send fallback HTML with user-friendly error messages
- Never crash SSE stream due to rendering failures

**Connection Management**:
- Handle broadcast channel lag gracefully - disconnect receivers that fall too far behind
- Implement proper Drop handlers for SSE connections to prevent memory leaks
- Add connection health checks and automatic cleanup

## Edge Cases & Notes

- **SSE reconnection**: htmx SSE extension auto-reconnects on disconnect. On reconnect, the client gets a fresh render immediately (first event after subscribe + debounce window).
- **No subscribers**: `broadcast::send` returns `Err` when there are no receivers — this is fine, we already ignore it for `progress_tx`.
- **Page navigation**: Client-side JavaScript automatically reconnects SSE with updated query parameters when users change filters via htmx.
- **Auth**: SSE endpoints require `AuthUser` extraction like all other protected routes.
- **KeepAlive**: All SSE endpoints use 15-second keepalive (matching existing progress endpoint).
- **Memory & Scaling**: 
  - Broadcast channels use capacity 256 (optimized for burst scenarios)
  - Implement per-user SSE connection limits (max 5 concurrent connections)
  - Timeout-based cleanup: Connections auto-close after 5 minutes of inactivity
  - Backpressure handling: Lagged receivers get disconnected gracefully with proper cleanup
  - Connection pooling: Reuse SSE handlers to prevent memory leaks from abandoned connections

**Server-Side Session Tracking Alternative**:
Instead of client-side parameter sync, we could track user session state server-side:
- **Pros**: No client-side JavaScript needed, simpler htmx integration, centralized state management
- **Cons**: Requires session storage, more complex server implementation, potential scalability issues with many concurrent users
- **Recommendation**: Start with client-side approach for simplicity, consider server-side if client-side proves unreliable
