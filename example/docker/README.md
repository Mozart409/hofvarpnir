# Hofvarpnir Docker Compose Deployment

Docker Compose setup for deploying Hofvarpnir video archival service.

## Quick Start

```bash
cd example/docker

# Copy and configure environment
cp .env.example .env

# Start services
docker compose up -d

# View logs
docker compose logs -f hofvarpnir
```

## Components

| Service | Image | Description |
|---------|-------|-------------|
| `hofvarpnir` | `ghcr.io/mozart409/hofvarpnir:latest` | Main application |
| `postgres` | `postgres:17` | PostgreSQL database |

## Configuration

### Environment Variables

Copy `.env.example` to `.env` and adjust as needed:

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | `postgresql://postgres:postgres@postgres:5432/hofvarpnir` | Database connection string |
| `HOST` | `0.0.0.0` | Server bind address |
| `PORT` | `8080` | Server port |
| `MAX_CONCURRENT_DOWNLOADS` | `3` | Parallel download limit |
| `DOWNLOAD_TIMEOUT_HOURS` | `4` | Timeout for long downloads |
| `MAX_DOWNLOAD_ATTEMPTS` | `5` | Retry attempts |
| `RATE_LIMIT_DELAY_SECS` | `60` | Delay between rate-limited requests |
| `DEFAULT_OUTPUT_DIR` | `/data/downloads` | Download directory inside container |
| `RUST_LOG` | `info,hofvarpnir=debug,sqlx=warn` | Logging configuration |

#### OIDC Authentication (Optional)

| Variable | Default | Description |
|----------|---------|-------------|
| `OIDC_ISSUER` | - | OIDC provider URL (e.g., `https://auth.example.com`) - enables OIDC when set |
| `OIDC_CLIENT_ID` | - | OAuth2 client ID |
| `OIDC_CLIENT_SECRET` | - | OAuth2 client secret |
| `OIDC_SCOPES` | `openid,profile,email` | Requested scopes |
| `OIDC_AUTO_PROVISION` | `true` | Auto-create users on first login |
| `OIDC_REDIRECT_BASE_URL` | - | Callback base URL (e.g., `https://hof.example.com`) |
| `OIDC_LOGOUT_REDIRECT` | `false` | Enable provider logout redirect |
| `OIDC_DISCOVERY_TIMEOUT` | `30` | Discovery timeout in seconds |

### Volumes

| Mount | Description |
|-------|-------------|
| `./hofvarpnir` | Downloaded videos (maps to `/data/downloads`) |
| `postgres_data` | PostgreSQL data (named volume) |

### User Permissions

The container runs as `UID:GID 1000:1000` for compatibility with media servers like Jellyfin and Plex. Adjust the `user:` directive in `compose.yml` if needed.

## Production Considerations

- Set up proper backup for the `postgres_data` volume
- Configure a reverse proxy (Caddy, Traefik, nginx) for HTTPS
- Change default PostgreSQL credentials
- Consider using Docker secrets for sensitive values

## Commands

```bash
# Start in background
docker compose up -d

# View logs
docker compose logs -f

# Stop services
docker compose down

# Stop and remove volumes (WARNING: deletes data)
docker compose down -v

# Update to latest image
docker compose pull
docker compose up -d

# Check service health
docker compose ps
```

## Troubleshooting

### Database connection issues

```bash
# Check if postgres is healthy
docker compose exec postgres pg_isready -U postgres

# View postgres logs
docker compose logs postgres
```

### Permission issues with downloads

Ensure the host directory has correct ownership:

```bash
sudo chown -R 1000:1000 ./hofvarpnir
```
