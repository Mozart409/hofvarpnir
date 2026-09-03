# Hofvarpnir

<p align="center">
  <img src="imgs/logo4.png" alt="Hofvarpnir logo" width="320">
</p>

> In Norse mythology, **Hofvarpnir** ("hoof-thrower") is the horse of the goddess Gná, who rides through sky and sea to fetch things from distant realms for Frigg.

A self-hosted video archival system that downloads videos from YouTube (and other platforms) via yt-dlp.

## Features

- **Multi-platform support**: YouTube and other platforms via yt-dlp auto-detection
- **Web UI**: Modern web interface built with htmx and Tailwind CSS
- **Automatic scheduling**: Per-source indexing frequency
- **Resilient indexing**: Age-restricted, private, and unavailable videos are skipped and the scan continues, so a single problem entry never aborts indexing of the rest of a channel/playlist
- **Health monitoring**: Surface sources that are enabled but persistently failing to index
- **Quality presets**: Configurable download quality
- **Retention policies**: Automatic cleanup with per-source and per-profile settings
- **Deduplication**: Videos downloaded once regardless of multiple source references
- **Real-time progress**: SSE-based live download progress in both web and TUI
- **Observability**: OpenTelemetry traces (Tempo), log shipping (Loki), Prometheus metrics, and Grafana dashboards
- **Dark Mode**: Tailwindcss darkmode
- **OIDC Authentication**: Single sign-on via OpenID Connect (Keycloak, Auth0, Azure AD, Google, Okta, Pocket-ID, etc.)

## Planned

- **TUI**: Terminal-based management interface
- **Keyboard Shortcuts**: Vim motions

## Tech Stack

- **Runtime**: Tokio
- **HTTP**: Axum
- **Actors**: Kameo
- **Templating**: Maud + htmx
- **Styling**: Tailwind CSS 4
- **Database**: PostgreSQL 17 (via SQLx)
- **API docs**: OpenAPI (utoipa) + Scalar
- **TUI**: Ratatui

## Quick Start

```bash
# Build all crates
cargo build --release

# Run database migrations
# (SQLx migrations in hof-core/migrations/)

# Start the server (API + Web UI)
cargo run --bin hof-server

# Run the TUI client (in another terminal)
cargo run --bin hof-tui
```

## Project Structure

```
crates/
├── hof-core/    Domain types, actors, database, yt-dlp process wrapper
├── hof-api/     Axum REST API + OpenAPI + SSE (JSON) + Scalar docs
├── hof-web/     Maud + htmx frontend routes + SSE (HTML partials)
└── hof-tui/     Ratatui TUI client (consumes hof-api over HTTP)
```

## API

The REST API is documented with OpenAPI and available via Scalar at `/docs` when running the server.

Key endpoints:

- `GET /api/v1/profiles` - Manage download profiles
- `GET /api/v1/sources` - Manage video sources (channels, playlists)
- `GET /api/v1/downloads` - List and manage downloads
- `GET /api/v1/downloads/progress` - SSE stream for live progress (JSON)
- `GET /web/v1/downloads/progress` - SSE stream for live progress (HTML)
- `GET /api/v1/activity` - System activity log (indexing, downloads, errors)
- `GET /api/v1/activity/unhealthy-sources` - Sources persistently failing to index (configurable `min_errors` threshold)

### Indexing resilience

When indexing a channel or playlist, a video that cannot be fetched (age-restricted, private, removed, or otherwise unavailable) is **skipped**, and indexing continues with the remaining entries. Age-restriction in particular is treated as a permanent, per-video condition — distinct from rate limiting, which pauses indexing of the source. This prevents a single age-restricted video from silently blocking discovery of every other video in the source.

Use `GET /api/v1/activity/unhealthy-sources` to find sources that are enabled but stuck on errors (e.g. repeated rate limiting). It reports the consecutive-error streak since each source's last successful index.

## Configuration

Configuration is loaded from environment variables:

| Variable                      | Description                            | Default      |
| ----------------------------- | -------------------------------------- | ------------ |
| `DATABASE_URL`                | PostgreSQL connection string           | -            |
| `HOST`                        | Server bind address                    | 127.0.0.1    |
| `PORT`                        | Server port                            | 3000         |
| `YTDLP_PATH`                  | Path to yt-dlp binary                  | yt-dlp       |
| `MAX_CONCURRENT_DOWNLOADS`¹   | Max simultaneous downloads             | 3            |
| `DOWNLOAD_TIMEOUT_HOURS`      | Per-download timeout (hours)           | 4            |
| `MAX_DOWNLOAD_ATTEMPTS`       | Retries before permanently-failed      | 5            |
| `RATE_LIMIT_DELAY_SECS`¹      | Delay between yt-dlp invocations       | 5            |
| `DEFAULT_OUTPUT_DIR`          | Default download output directory      | /var/lib/hofvarpnir/downloads |
| `RETENTION_DAYS`              | Global retention policy, in days       | - (no cleanup unless set) |
| `MAX_INDEXERS_PER_TICK`¹      | Max sources indexed per scheduler tick | 5            |
| `CHECK_INTERVAL_SECS`¹        | Scheduler tick interval, in seconds    | 60           |
| `CLEANUP_INTERVAL_SECS`¹      | Cleanup actor run interval, in seconds | 10800 (3h)   |
| `DRAIN_TIMEOUT_SECS`¹         | Max time to wait for graceful shutdown | 1800 (30m)   |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | OTLP gRPC endpoint for traces          | - (disabled) |
| `OTEL_SERVICE_NAME`           | Service name for traces/logs           | hofvarpnir   |
| `LOKI_URL`                    | Grafana Loki endpoint for log shipping | - (disabled) |
| `METRICS_ENABLED`             | Enable Prometheus metrics endpoint     | false        |
| `LOG_FORMAT`                  | Log output format (`json` or default)  | default      |

¹ **Runtime-tunable.** These six values can also be set from the
`/settings/runtime` control panel (or `PATCH /api/v1/system/settings`). A
value stored in the database **overrides the environment variable**, which
is itself only the fallback used when the database column is `NULL` — the
environment variable does not "win" once a database value exists. The
control panel shows a `default` / `env` / `database` badge next to each
value indicating which layer it actually resolved from. Every other
variable above is read once at startup with no runtime override. See
[Runtime control](#runtime-control) below.

### OIDC Authentication (Optional)

To enable OIDC single sign-on, set these environment variables:

| Variable                  | Description                                              | Default                   |
| ------------------------- | -------------------------------------------------------- | ------------------------- |
| `OIDC_ISSUER`             | OIDC provider issuer URL (required to enable OIDC)       | - (disabled)              |
| `OIDC_CLIENT_ID`          | OAuth2 client ID from your OIDC provider                 | -                         |
| `OIDC_CLIENT_SECRET`      | OAuth2 client secret from your OIDC provider           | -                         |
| `OIDC_SCOPES`             | Comma-separated scopes to request                        | `openid,profile,email`    |
| `OIDC_AUTO_PROVISION`     | Auto-create users on first OIDC login                   | `true`                    |
| `OIDC_REDIRECT_BASE_URL`  | Override base URL for callback (e.g., `https://hof.example.com`) | - (derived from request) |
| `OIDC_LOGOUT_REDIRECT`    | Enable RP-initiated logout                              | `false`                   |
| `OIDC_DISCOVERY_TIMEOUT`  | Discovery HTTP timeout in seconds                       | `30`                      |

## Runtime control

The control panel at `/settings/runtime` lets an operator adjust the six
runtime-tunable settings above without restarting, pause indexing and/or
downloads, and trigger a graceful shutdown. The equivalent endpoints exist
under `/api/v1/system/` (`settings`, `pause`, `shutdown`, `status`) for
scripted use.

- **Pause is per-module**: indexing and downloads are gated independently, so
  you can pause one without stopping the other. Choose a duration — 1h, 6h,
  12h, 24h, 3d, 7d, or indefinite — and it auto-resumes on its own when the
  duration elapses (indefinite pauses require an explicit resume). Pause is
  a database column, so it **survives a restart**.
- **Drain deliberately does not survive a restart.** Draining is process-local
  by design: a restarted container must never come back up silently refusing
  all work with no visible cause. See
  [ADR-0004](docs/adr/0004-drain-state-not-persisted.md).

### The "Shut down" button and `restart: always`

Draining is a *graceful shutdown*, not a crash: the process stops accepting
new work, finishes in-flight downloads and indexing (or gives up once
`DRAIN_TIMEOUT_SECS` elapses), and then **returns from `main` with exit code
0**. Under a container `restart` policy of `always` or `unless-stopped`, the
container runtime treats exit code 0 as a normal stop and **starts the
container straight back up** — so from the operator's point of view, the
"Shut down" button appears to do nothing.

This is the one place the control panel cannot do what its label implies,
and the fix is deployment-side, not application-side: a deployment that
wants the process to actually stay down after using this button must run
with `restart: on-failure` instead.

## Development

See [GOALS.md](./GOALS.md) for detailed architecture and implementation roadmap.

## CI/CD

This project uses GitHub Actions for continuous integration and deployment:

- `.github/workflows/ci.yml` - Runs on pull requests and pushes to main branch
- `.github/workflows/release.yml` - Creates GitHub releases when pushing version tags (v*.*.\*)

## License

GPL-3.0-or-later see [LICENSE](./LICENSE)
