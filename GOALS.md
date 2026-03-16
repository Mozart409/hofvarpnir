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
- **Database**: PostgreSQL 17 (via SQLx) + PgBouncer
- **IDs**: ULID (lexicographically sortable, stored as `TEXT` in Postgres)
- **API docs**: OpenAPI (utoipa) + Scalar
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
- `hof-tui` depends only on `reqwest` — it talks to the API, not to core directly.
- `hof-api` and `hof-web` compile into a single server binary.
- `hof-tui` compiles into a separate standalone binary.

## Domain Model

### User

Authentication and ownership boundary.

### Profile

Belongs to a user. Represents a download configuration for a specific platform.

| Field               | Type              | Description                              |
|---------------------|-------------------|------------------------------------------|
| platform            | youtube, vimeo, … | Target platform                          |
| quality             | best, 1080p, …    | Download quality preset                  |
| naming_template     | String             | e.g. `"{title}-{id}.{ext}"`             |
| output_dir          | PathBuf            | Where files land on disk                 |
| include_livestreams | bool               | Whether to download livestream VODs      |
| include_shorts      | bool               | Whether to download Shorts               |
| storage_quota_bytes | i64                | Max disk usage for this profile          |
| retention_days      | Option\<i32\>     | Auto-cleanup after N days (profile-wide) |

### Source

Belongs to a profile. Represents a channel, playlist, or other feed to monitor.

| Field           | Type             | Description                                |
|-----------------|------------------|--------------------------------------------|
| url             | String           | Channel or playlist URL                    |
| source_type     | channel, playlist| Type of source                             |
| custom_name     | Option\<String\> | User-defined label                         |
| index_frequency | Duration         | How often to check for new videos          |
| cutoff_date     | NaiveDate        | Ignore videos published before this date   |
| retention_days  | Option\<i32\>    | Per-source retention override              |

### Video (global, deduplicated)

Keyed by `(platform, platform_video_id)`. A single video is downloaded once
regardless of how many sources reference it.

| Field             | Type                | Description                        |
|-------------------|---------------------|------------------------------------|
| platform_video_id | String (unique)     | e.g. YouTube video ID              |
| platform          | youtube, vimeo, …   | Origin platform                    |
| title             | String              | Video title                        |
| description       | Option\<String\>    | Video description                  |
| duration          | Option\<Duration\>  | Video length                       |
| published_at      | Option\<DateTime\>  | Publication date                   |
| thumbnail_url     | Option\<String\>    | Thumbnail                          |
| status            | pending, downloading, completed, failed, skipped, cleaned | Lifecycle state |
| attempts          | i32                 | Number of download attempts        |
| next_retry        | Option\<DateTime\>  | When to retry after failure        |
| last_error        | Option\<String\>    | Last failure reason                |
| file_path         | Option\<String\>    | Path to downloaded file            |
| file_size_bytes   | Option\<i64\>       | Size on disk                       |
| downloaded_at     | Option\<DateTime\>  | When download completed            |

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

| Actor                | Lifecycle         | Purpose                                       |
|----------------------|-------------------|-----------------------------------------------|
| SchedulerActor       | singleton         | Fires indexing on schedule via tokio::time     |
| SourceIndexerActor   | per source        | Calls yt-dlp --flat-playlist, filters by date  |
| DownloadSupervisor   | singleton         | Manages concurrency (3 max), retry, backpressure |
| DownloadWorker       | short-lived       | Shells out to yt-dlp, streams progress         |
| CleanupActor         | singleton         | Enforces retention + storage quotas            |

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
- `GET /api/downloads/progress` — JSON events (consumed by TUI and API clients)
- `GET /web/downloads/progress` — HTML partial events (consumed by htmx)

Both read from the same actor state. The web endpoint wraps JSON in maud templates.

The TUI consumes the API SSE endpoint via reqwest's streaming response,
forwarding parsed events to the render loop over `tokio::sync::mpsc`.

## Persistence Strategy

- Postgres is the single source of truth.
- Actors are the runtime cache.
- On state change: write to Postgres first, then update actor state.
- On startup: load from Postgres, hydrate actors.

## What We Explicitly Do NOT Need

| Thing           | Why                                                     |
|-----------------|---------------------------------------------------------|
| pgmq            | Actors + Postgres rows as pending work is sufficient    |
| Redis           | No caching layer needed, actor state is the cache       |
| External cron   | SchedulerActor with tokio::time::interval handles it    |
| Message broker  | Single binary, actors communicate in-process            |
