# Hofvarpnir

> In Norse mythology, **Hofvarpnir** ("hoof-thrower") is the horse of the goddess Gná, who rides through sky and sea to fetch things from distant realms for Frigg.

A self-hosted video archival system that downloads videos from YouTube (and other platforms) via yt-dlp.

## Features

- **Multi-platform support**: YouTube and other platforms via yt-dlp auto-detection
- **Web UI**: Modern web interface built with htmx and Tailwind CSS
- **Automatic scheduling**: Per-source indexing frequency
- **Quality presets**: Configurable download quality
- **Retention policies**: Automatic cleanup with per-source and per-profile settings
- **Deduplication**: Videos downloaded once regardless of multiple source references
- **Real-time progress**: SSE-based live download progress in both web and TUI
- **Observability**: OpenTelemetry traces (Tempo), log shipping (Loki), Prometheus metrics, and Grafana dashboards
- **Dark Mode**: Tailwindcss darkmode

## Planned

- **TUI**: Terminal-based management interface
- **OIDC**: Implement Oauth2 for e.g. [PocketID](https://github.com/pocket-id/pocket-id)
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

## Configuration

Configuration is loaded from environment variables:

| Variable                      | Description                            | Default      |
| ----------------------------- | -------------------------------------- | ------------ |
| `DATABASE_URL`                | PostgreSQL connection string           | -            |
| `PORT`                        | Server port                            | 3000         |
| `YT_DLP_PATH`                 | Path to yt-dlp binary                  | yt-dlp       |
| `DOWNLOAD_CONCURRENCY`        | Max simultaneous downloads             | 3            |
| `DOWNLOAD_TIMEOUT`            | Per-download timeout (hours)           | 4            |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | OTLP gRPC endpoint for traces          | - (disabled) |
| `OTEL_SERVICE_NAME`           | Service name for traces/logs           | hofvarpnir   |
| `LOKI_URL`                    | Grafana Loki endpoint for log shipping | - (disabled) |
| `METRICS_ENABLED`             | Enable Prometheus metrics endpoint     | false        |
| `LOG_FORMAT`                  | Log output format (`json` or default)  | default      |

## Development

See [GOALS.md](./GOALS.md) for detailed architecture and implementation roadmap.

## CI/CD

This project uses GitHub Actions for continuous integration and deployment:

- `.github/workflows/ci.yml` - Runs on pull requests and pushes to main branch
- `.github/workflows/release.yml` - Creates GitHub releases when pushing version tags (v*.*.\*)

## License

GPL-3.0-or-later see [LICENSE](./LICENSE)
