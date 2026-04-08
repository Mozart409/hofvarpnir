# OpenTelemetry Observability Plan

**Document version:** 1.0.0
**Created:** 2026-04-08
**Status:** Draft / Brainstorm

## Goal

Full observability for Hofvarpnir: **traces + metrics + logs**, all visible in Grafana via Tempo, Prometheus, and Loki. Configurable via environment variables, off by default.

## Architecture

```
┌─────────────────┐
│   Hofvarpnir    │
│                 │──── OTLP/gRPC (traces) ────► Grafana Tempo
│  tracing +      │──── OTLP/gRPC (logs)   ────► Grafana Loki
│  opentelemetry  │
│                 │──── /metrics (scrape)   ────► Prometheus
└─────────────────┘                                   │
                                                      ▼
                                                  Grafana
                                            (traces ↔ logs ↔ metrics
                                             via trace_id correlation)
```

Direct export (no OTel Collector) — single service, keeps it simple. Collector can be added later if needed.

## Crate Stack

| Crate | Version | Purpose |
|---|---|---|
| `tracing` | 0.1 | Already in use |
| `tracing-subscriber` | 0.3 | Already in use (env-filter, json features) |
| `opentelemetry` | 0.31.0 | Core OTel API |
| `opentelemetry_sdk` | 0.31.0 | OTel SDK (trace provider, span processor) |
| `opentelemetry-otlp` | 0.31.1 | OTLP gRPC/HTTP exporter |
| `tracing-opentelemetry` | 0.32.1 | Bridge: tracing spans → OTel spans |
| `tracing-loki` | 0.2.6 | Ship logs to Loki with trace_id |
| `metrics` | 0.24.3 | Metrics facade |
| `metrics-exporter-prometheus` | 0.18.1 | `/metrics` endpoint for Prometheus scrape |

## Environment Variables

Add to `.env.example` and `Config`:

```bash
# OpenTelemetry — set OTEL_EXPORTER_OTLP_ENDPOINT to enable
# OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
# OTEL_SERVICE_NAME=hofvarpnir

# Loki — set LOKI_URL to enable log shipping
# LOKI_URL=http://localhost:3100

# Metrics — set to expose /metrics endpoint (default: true when server runs)
# METRICS_ENABLED=true
```

OTel follows its own env var conventions (`OTEL_*`), so we respect those. If `OTEL_EXPORTER_OTLP_ENDPOINT` is unset, tracing export is disabled and the app behaves exactly as today.

---

## Phase 1: Improved Logging Foundation ✅

**Status:** Complete
**Goal:** Fix existing gaps, prepare the subscriber pipeline for layered composition.

### 1.1 Refactor tracing-subscriber initialization

- Move from simple `fmt().init()` to a layered `Registry` approach in a shared init function (likely in `hof-core` or a new `hof-telemetry` module)
- This is required because OTel, Loki, and fmt layers all stack on the same registry

```rust
// Pseudocode — final shape after all phases
tracing_subscriber::registry()
    .with(env_filter_layer)
    .with(fmt_layer)           // always: console output
    .with(otel_trace_layer)    // phase 3: if OTEL endpoint set
    .with(loki_layer)          // phase 4: if LOKI_URL set
    .init();
```

### 1.2 Conditional JSON logging

- If `LOG_FORMAT=json` env var is set, use `fmt::layer().json()` instead of default text
- Useful for production log aggregation

### 1.3 Apply tower-http TraceLayer

- Wire up `TraceLayer::new_for_http()` on the Axum router
- Gives automatic spans for every HTTP request (method, path, status, latency)
- The dependency is already there, just unused

### 1.4 Add logging to auth failures

- Log `warn!` on failed auth attempts in `auth.rs` (currently silent redirects)

### 1.5 Request ID middleware

- Generate a UUID/ULID per request, attach to the span as `request_id`
- Return it as `x-request-id` response header
- All log lines within the request will carry this ID

---

## Phase 2: Metrics (Prometheus) ✅

**Status:** Complete
**Goal:** Expose application metrics via `/metrics` for Prometheus to scrape.

### 2.1 Add metrics dependencies

- Add `metrics` and `metrics-exporter-prometheus` to workspace `Cargo.toml`

### 2.2 Set up Prometheus exporter

- Initialize `PrometheusBuilder` at startup
- Mount the `/metrics` GET endpoint on the Axum router (separate from API routes)
- Gate behind `METRICS_ENABLED` env var (default: true)

### 2.3 HTTP request metrics

- Add middleware that records per-request metrics:
  - `http_requests_total` (counter, labels: method, path, status)
  - `http_request_duration_seconds` (histogram, labels: method, path)

### 2.4 Business metrics

- `downloads_active` (gauge) — currently running downloads
- `downloads_total` (counter, labels: status=completed|failed)
- `download_duration_seconds` (histogram) — time from start to completion
- `source_index_total` (counter, labels: status=success|error)
- `source_index_duration_seconds` (histogram)
- `videos_cleaned_total` (counter)

### 2.5 Infrastructure metrics

- `db_query_duration_seconds` (histogram, label: query) — wrap SQLx queries or use SQLx's built-in tracing
- `actor_messages_processed_total` (counter, label: actor) — if kameo exposes this

---

## Phase 3: Distributed Tracing (OpenTelemetry + Tempo) ✅

**Status:** Complete
**Goal:** Export spans to Grafana Tempo. Existing `#[instrument]` annotations become real distributed traces.

### 3.1 Add OpenTelemetry dependencies

- Add `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `tracing-opentelemetry` to workspace

### 3.2 Initialize OTel trace pipeline

- Conditional on `OTEL_EXPORTER_OTLP_ENDPOINT` being set
- Configure `BatchSpanProcessor` for async export
- Use `opentelemetry-otlp` gRPC exporter (tonic)
- Set service name from `OTEL_SERVICE_NAME` (default: `hofvarpnir`)
- 100% sampling (no sampling configured = sample everything)

### 3.3 Add OpenTelemetry tracing layer

- Create `tracing_opentelemetry::layer()` and add to the subscriber registry
- All existing `#[instrument]` spans are now exported — zero code changes needed

### 3.4 Propagate trace context in HTTP responses

- Add `traceparent` header to responses for downstream correlation
- Useful if a frontend or other service calls the API

### 3.5 Enrich existing spans

- Review `#[instrument]` annotations — ensure key spans have meaningful names and fields
- Add `#[instrument]` to HTTP handlers that lack it
- Ensure DB queries are captured as child spans (SQLx emits tracing events by default)

### 3.6 Graceful shutdown

- Flush the span pipeline on shutdown (`opentelemetry::global::shutdown_tracer_provider()`)
- Ensure in-flight spans are exported before the process exits

---

## Phase 4: Log Shipping (Loki)

**Goal:** Ship structured logs to Grafana Loki with `trace_id` for correlation.

### 4.1 Add tracing-loki dependency

- Add `tracing-loki` to workspace

### 4.2 Initialize Loki layer

- Conditional on `LOKI_URL` being set
- Configure with labels: `service=hofvarpnir`, `env` from env var
- Add as a layer to the tracing subscriber registry

### 4.3 Inject trace_id into log events

- When OTel is enabled, `tracing-opentelemetry` automatically adds trace/span IDs to the current span context
- Ensure the Loki layer picks these up and includes `trace_id` as a label or structured field
- This enables Grafana's "Trace to Logs" and "Logs to Trace" navigation

### 4.4 Verify correlation in Grafana

- Configure Tempo datasource → "Trace to Logs" pointing at Loki
- Configure Loki datasource → "Derived fields" to link `trace_id` to Tempo
- Verify: click a trace → see logs; click a log line → jump to trace

---

## Phase 5: Development Environment

**Goal:** Provide a local Grafana+Tempo+Loki+Prometheus stack for development via Podman.

### 5.1 Create `docker-compose.dev-observability.yml`

```yaml
# Services:
# - Grafana (port 3001 to avoid conflict with app)
# - Prometheus (port 9090, scrapes app at host:8080/metrics)
# - Tempo (port 4317 for OTLP gRPC, port 3200 for query)
# - Loki (port 3100)
```

### 5.2 Grafana provisioning

- Auto-provision datasources (Prometheus, Tempo, Loki) via config files
- Pre-configure Trace-to-Logs and Trace-to-Metrics correlations
- Optionally include a starter dashboard JSON

### 5.3 Development .env additions

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
OTEL_SERVICE_NAME=hofvarpnir
LOKI_URL=http://localhost:3100
LOG_FORMAT=text
METRICS_ENABLED=true
```

### 5.4 Document in README / justfile

- Add `just observability-up` / `just observability-down` commands
- Document how to access Grafana, explore traces, view metrics

---

## Phase 6: Production Hardening (Future)

**Goal:** Items to consider once the basics are stable. Not required for initial rollout.

### 6.1 Health/readiness endpoints

- `GET /health` — basic liveness (always 200)
- `GET /ready` — checks DB connectivity, actor system health

### 6.2 Grafana dashboards

- Pre-built dashboard: HTTP overview (request rate, latency p50/p95/p99, error rate)
- Pre-built dashboard: Downloads (active, completed, failed, duration)
- Pre-built dashboard: System (DB pool, actor mailboxes)

### 6.3 Alerting rules

- Prometheus alerting rules for: high error rate, download failures spike, DB connection pool exhaustion

### 6.4 Span sampling (if needed)

- If trace volume becomes a concern, configure head-based or tail-based sampling
- For current scale (video downloads), 100% sampling should be fine

### 6.5 OTel Collector (if needed)

- If more services are added or you want trace enrichment/filtering, add the collector as an intermediary
- App → Collector → Tempo/Loki

---

## Implementation Order

Phases are designed to be merged independently. Suggested order:

1. **Phase 1** (logging foundation) — prerequisite for everything else
2. **Phase 2** (metrics) — standalone, immediate value with existing Prometheus+Grafana
3. **Phase 5** (dev environment) — set up before Phase 3/4 so you can test locally
4. **Phase 3** (traces) — biggest observability upgrade
5. **Phase 4** (log shipping) — completes the correlation triangle
6. **Phase 6** (hardening) — ongoing, as needed

---

## Notes

- NixOS production deployment config for Tempo/Loki is out of scope for this document (handled separately in NixOS config)
- The app should work identically with all OTel features disabled (no env vars set)
- All new dependencies should be workspace-level in root `Cargo.toml`
- Check `tracing-loki` compatibility with the `tracing-opentelemetry` version before Phase 4 — these crate ecosystems sometimes have version coupling
