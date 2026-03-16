# Hofvarpnir — Project Goals

## Overview

A self-hosted video archival system that downloads videos from YouTube (and other
platforms) via yt-dlp. Provides both a web UI and a TUI for management.

## Tech Stack

- **Runtime**: Tokio
- **HTTP**: Axum
- **Actors**: Kameo
- **Templating**: Maud + htmx
- **Styling**: Tailwind CSS 4
- **Database**: PostgreSQL 17 (via SQLx)
- **IDs**: ULID (lexicographically sortable, stored as `TEXT` in Postgres)
- **api/v1 docs**: OpenAPI (utoipa) + Scalar
- **TUI**: Ratatui

## Crate Structure

```
crates/
  hof-core/    Domain types, actors, database, yt-dlp process wrapper
  hof-api/     Axum REST API + OpenAPI + SSE (JSON) + Scalar docs
  hof-web/     Maud + htmx frontend routes + SSE (HTML partials)
  hof-tui/     Ratatui TUI client (consumes hof-api over HTTP)
```

- `hof-core` has no dependency on any HTTP crate.
- `hof-api` depends on `hof-core`.
- `hof-web` depends on `hof-core` and `hof-api`.
- `hof-tui` depends only on `reqwest` — it talks to the api/v1, not to core directly.
- `hof-api` and `hof-web` compile into a single server binary.
- `hof-tui` compiles into a separate standalone binary.

## Domain Model

### User

Authentication and ownership boundary.

### Profile

Belongs to a user. Represents a download configuration that can apply to sources
from any platform (yt-dlp auto-detects the platform from each source URL).

| Field               | Type           | Description                              |
| ------------------- | -------------- | ---------------------------------------- |
| quality             | best, 1080p, … | Download quality preset                  |
| naming_template     | String         | e.g. `"{title}-{id}.{ext}"`              |
| output_dir          | PathBuf        | Where files land on disk                 |
| include_livestreams | bool           | Whether to download livestream VODs      |
| include_shorts      | bool           | Whether to download Shorts               |
| storage_quota_bytes | i64            | Max disk usage for this profile          |
| retention_days      | Option\<i32\>  | Auto-cleanup after N days (profile-wide) |

### Source

Belongs to a profile. Represents a channel, playlist, or other feed to monitor.

| Field           | Type              | Description                              |
| --------------- | ----------------- | ---------------------------------------- |
| url             | String            | Channel or playlist URL                  |
| source_type     | channel, playlist | Type of source                           |
| custom_name     | Option\<String\>  | User-defined label                       |
| index_frequency | Duration          | How often to check for new videos        |
| cutoff_date     | NaiveDate         | Ignore videos published before this date |
| retention_days  | Option\<i32\>     | Per-source retention override            |

### Video (global, deduplicated)

Keyed by `(platform, platform_video_id)`. A single video is downloaded once
regardless of how many sources reference it.

| Field             | Type                                                      | Description                                     |
| ----------------- | --------------------------------------------------------- | ----------------------------------------------- |
| platform_video_id | String (unique)                                           | e.g. YouTube video ID                           |
| platform          | String (TEXT)                                             | yt-dlp extractor name (e.g. "youtube", "vimeo") |
| title             | String                                                    | Video title                                     |
| description       | Option\<String\>                                          | Video description                               |
| duration          | Option\<Duration\>                                        | Video length                                    |
| published_at      | Option\<DateTime\>                                        | Publication date                                |
| thumbnail_url     | Option\<String\>                                          | Thumbnail                                       |
| status            | pending, downloading, completed, failed, skipped, cleaned | Lifecycle state                                 |
| attempts          | i32                                                       | Number of download attempts                     |
| next_retry        | Option\<DateTime\>                                        | When to retry after failure                     |
| last_error        | Option\<String\>                                          | Last failure reason                             |
| file_path         | Option\<String\>                                          | Path to downloaded file                         |
| file_size_bytes   | Option\<i64\>                                             | Size on disk                                    |
| downloaded_at     | Option\<DateTime\>                                        | When download completed                         |

### source_videos (join table)

Links sources to videos (many-to-many).

## Retention Policy

Precedence (first non-null wins):

```
source.retention_days → profile.retention_days → global_config.retention_days
```

If a video is referenced by multiple sources with different retention values,
it is kept until the **longest** retention period expires (all referencing
sources must agree the video is expired before deletion).

## Actor Architecture

```
┌──────────────────────────────────────────────────┐
│                  App (startup)                    │
│  Hydrates all actors from Postgres on boot        │
└──┬───────────┬──────────────┬────────────────────┘
   │           │              │
   ▼           ▼              ▼
Scheduler    Download       Cleanup
Actor        Supervisor     Actor
(singleton)  (singleton)    (singleton)
   │              │
   │ ticks per    │ bounded(3) via tokio::sync::Semaphore
   │ source freq  │
   ▼              ▼
Source        Download Worker
Indexer       (short-lived, per video)
(per source)  streams progress via actor messages
```

### Actor Responsibilities

| Actor              | Lifecycle   | Purpose                                          |
| ------------------ | ----------- | ------------------------------------------------ |
| SchedulerActor     | singleton   | Fires indexing on schedule via tokio::time       |
| SourceIndexerActor | per source  | Calls yt-dlp --flat-playlist, filters by date    |
| DownloadSupervisor | singleton   | Manages concurrency (3 max), retry, backpressure |
| DownloadWorker     | short-lived | Shells out to yt-dlp, streams progress           |
| CleanupActor       | singleton   | Enforces retention + storage quotas              |

### Concurrency

- Max 3 simultaneous downloads (configurable).
- `DownloadSupervisor` holds a `tokio::sync::Semaphore` with 3 permits.
- A global rate limiter spaces yt-dlp invocations by 3-5 seconds to avoid
  YouTube throttling.

### Timeouts and Backoff

- Per-download timeout: 4 hours (configurable, for long videos).
- `kill_on_drop(true)` on `tokio::process::Command` to prevent orphaned yt-dlp.
- Exponential backoff on failure: 2, 4, 8, 16, 32, 64 minutes (capped).
- Max 5 attempts before marking `permanently_failed`.
- On YouTube 429 (rate limit), exponential backoff on the rate limiter itself.

### Crash Recovery

On startup:

1. Reset any videos stuck in `downloading` → `pending` (don't increment attempts).
2. Clean up orphaned `.part` files from yt-dlp.
3. Hydrate all actors from Postgres state.
4. Supervisor picks up pending downloads naturally.

## Progress Reporting

yt-dlp supports `--progress-template` for structured JSON progress on stdout.

Two SSE endpoints:

- `GET /api/v1/downloads/progress` — JSON events (consumed by TUI and API clients)
- `GET /web/downloads/progress` — HTML partial events (consumed by htmx)

Both read from the same actor state. The web endpoint wraps JSON in maud templates.

The TUI consumes the api/v1 SSE endpoint via reqwest's streaming response,
forwarding parsed events to the render loop over `tokio::sync::mpsc`.

## Persistence Strategy

- Postgres is the single source of truth.
- Actors are the runtime cache.
- On state change: write to Postgres first, then update actor state.
- On startup: load from Postgres, hydrate actors.

## What We Explicitly Do NOT Need

| Thing          | Why                                                  |
| -------------- | ---------------------------------------------------- |
| pgmq           | Actors + Postgres rows as pending work is sufficient |
| Redis          | No caching layer needed, actor state is the cache    |
| External cron  | SchedulerActor with tokio::time::interval handles it |
| Message broker | Single binary, actors communicate in-process         |

## Implementation Roadmap

### Phase 1: Foundation

- [x] **Database Migrations**
  - [x] Create `users` table
  - [x] Create `profiles` table with `quality` enum
  - [x] Create `sources` table with `source_type` enum
  - [x] Create `videos` table with `video_status` enum (platform as TEXT)
  - [x] Create `source_videos` join table

- [x] **Configuration** (`hof-core/src/config.rs`)
  - [x] Load from environment variables
  - [x] Database URL, server port, yt-dlp path
  - [x] Download concurrency, timeouts, retry settings

- [x] **Database Layer** (`hof-core/src/db.rs`)
  - [x] Connection pool setup with SQLx
  - [x] CRUD operations for User, Profile, Source, Video
  - [x] Join table operations for source_videos

### Phase 2: yt-dlp Integration

- [x] **yt-dlp Wrapper** (`hof-core/src/ytdlp.rs`)
  - [x] Video metadata fetching via yt-dlp crate
  - [x] Playlist/channel indexing for source discovery
  - [x] Video downloading with progress callbacks
  - [x] Quality selection based on profile settings
  - [x] Platform detection from URLs

### Phase 3: Actor System

- [x] **DownloadWorker** (`hof-core/src/actors/download_worker.rs`)
  - [x] Kameo actor implementation
  - [x] Spawn yt-dlp process, stream progress
  - [x] Report completion/failure to supervisor

- [x] **DownloadSupervisor** (`hof-core/src/actors/download_supervisor.rs`)
  - [x] Semaphore-based concurrency (max 3)
  - [x] Rate limiter (3-5 second spacing)
  - [x] Exponential backoff on failure
  - [x] 429 detection and global backoff

- [x] **SourceIndexerActor** (`hof-core/src/actors/source_indexer.rs`)
  - [x] Per-source actor
  - [x] Call yt-dlp `--flat-playlist`
  - [x] Filter by cutoff_date only index after cutoff date and not more, livestreams, shorts
  - [x] Upsert videos to database

- [x] **SchedulerActor** (`hof-core/src/actors/scheduler.rs`)
  - [x] Singleton with `tokio::time::interval`
  - [x] Track per-source index frequency
  - [x] Spawn/message SourceIndexerActors

- [x] **CleanupActor** (`hof-core/src/actors/cleanup.rs`)
  - [x] Retention policy enforcement
  - [x] Storage quota enforcement
  - [x] Delete files and update database

- [x] **Crash Recovery** (`hof-core/src/startup.rs`)
  - [x] Reset `downloading` → `pending`
  - [x] Clean up `.part` files
  - [x] Hydrate actors from Postgres

### Phase 4: REST api/v1

- [ ] **Profile Endpoints** (`hof-api/src/routes/profiles.rs`)
  - [ ] `GET /api/v1/profiles` — list all
  - [ ] `POST /api/v1/profiles` — create
  - [ ] `GET /api/v1/profiles/:id` — get one
  - [ ] `PUT /api/v1/profiles/:id` — update
  - [ ] `DELETE /api/v1/profiles/:id` — delete

- [ ] **Source Endpoints** (`hof-api/src/routes/sources.rs`)
  - [ ] `GET /api/v1/sources` — list all (filterable by profile)
  - [ ] `POST /api/v1/sources` — create
  - [ ] `GET /api/v1/sources/:id` — get one
  - [ ] `PUT /api/v1/sources/:id` — update
  - [ ] `DELETE /api/v1/sources/:id` — delete
  - [ ] `POST /api/v1/sources/:id/index` — trigger manual index

- [ ] **Download Endpoints** (`hof-api/src/routes/downloads.rs`)
  - [ ] `GET /api/v1/downloads` — list videos with status
  - [ ] `GET /api/v1/downloads/progress` — SSE stream (JSON)
  - [ ] `POST /api/v1/downloads/:id/retry` — manual retry

- [ ] **Openapi/v1 + Scalar**
  - [ ] Add utoipa annotations to all endpoints
  - [ ] Mount Scalar UI at `/docs`

### Phase 5: Web UI

- [ ] **Layout & Components** (`hof-web/src/pages.rs`)
  - [ ] Base layout with Tailwind CSS 4
  - [ ] Navigation component
  - [ ] Form components

- [ ] **Pages**
  - [ ] Dashboard (overview of downloads)
  - [ ] Profiles list and edit form
  - [ ] Sources list and edit form
  - [ ] Downloads list with live progress

- [ ] **SSE for htmx** (`GET /web/downloads/progress`)
  - [ ] HTML partial events for live updates

### Phase 6: TUI

- [ ] **Ratatui App** (`hof-tui/src/main.rs`)
  - [ ] App structure with event loop
  - [ ] reqwest client for api/v1
  - [ ] SSE stream consumption via `tokio::sync::mpsc`

- [ ] **TUI Views**
  - [ ] Downloads list with progress bars
  - [ ] Source management
  - [ ] Profile management
  - [ ] Keyboard navigation
