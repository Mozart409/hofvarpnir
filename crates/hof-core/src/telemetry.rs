//! Telemetry initialization: tracing subscriber with layered architecture.
//!
//! Supports:
//! - Console output with optional JSON formatting (`LOG_FORMAT=json`)
//! - Environment-based log filtering via `RUST_LOG`
//! - Per-request ID generation and propagation via `x-request-id` header
//! - OpenTelemetry trace export via OTLP (when `OTEL_EXPORTER_OTLP_ENDPOINT` is set)
//!   - Protocol configurable via `OTEL_EXPORTER_OTLP_PROTOCOL` (`grpc` or `http/protobuf`)
//!   - Auth headers from `OTEL_EXPORTER_OTLP_HEADERS`, sampling from
//!     `OTEL_TRACES_SAMPLER`/`_ARG` (both read by the SDK itself)
//! - Log shipping to Grafana Loki (when `LOKI_URL` is set); lines carry `trace_id`
//!   wherever an enclosing span recorded one via [`record_trace_id`]
//! - HTTP semantic convention attributes for service graph generation
//!
//! Both the OpenTelemetry and Loki pipelines are opt-in: if the respective env
//! vars are unset the app behaves exactly as before.

use std::time::Duration;

use http::{Request, Response};
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::Resource;
use tower_http::request_id::{MakeRequestId, RequestId};
use tower_http::trace::{MakeSpan, OnResponse};
use tracing::{Level, Span};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};
use ulid::Ulid;

/// Guard returned by [`init_tracing`].
///
/// When dropped, the OpenTelemetry tracer provider is shut down and in-flight
/// spans are flushed. Hold this in `main` until the process is ready to exit.
pub struct TelemetryGuard {
    provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    /// Handle to the Loki background sender task.
    _loki_task: Option<tokio::task::JoinHandle<()>>,
}

impl TelemetryGuard {
    /// Explicitly shut down the OpenTelemetry pipeline, flushing all pending spans.
    ///
    /// Idempotent: `Drop` calls this again after an explicit call.
    pub fn shutdown(&self) {
        let Some(provider) = &self.provider else {
            return;
        };
        match provider.shutdown() {
            Ok(()) | Err(opentelemetry_sdk::error::OTelSdkError::AlreadyShutdown) => {}
            Err(e) => {
                tracing::warn!(error = %e, "Failed to shut down OpenTelemetry tracer provider");
            }
        }
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Initialize the global tracing subscriber.
///
/// Reads configuration from environment variables:
/// - `RUST_LOG` — filter directives (default: `info`)
/// - `LOG_FORMAT` — `json` for structured JSON output, anything else for human-readable (default)
/// - `OTEL_EXPORTER_OTLP_ENDPOINT` — if set, enables OTLP trace export
/// - `OTEL_EXPORTER_OTLP_PROTOCOL` — `grpc` (default) or `http/protobuf`
/// - `OTEL_SERVICE_NAME` — service name reported to the collector (default: `hofvarpnir`)
/// - `LOKI_URL` — if set, ships logs to Grafana Loki (e.g. `http://localhost:3100`)
/// - `LOKI_HEADERS` — extra Loki push headers, same format as `OTEL_EXPORTER_OTLP_HEADERS`
///
/// Returns a [`TelemetryGuard`] that must be held until shutdown. Dropping it
/// flushes the OpenTelemetry exporter.
///
/// # Panics
///
/// Panics if a global subscriber has already been set.
pub fn init_tracing() -> TelemetryGuard {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let log_format = std::env::var("LOG_FORMAT").unwrap_or_default();
    let use_json = log_format.eq_ignore_ascii_case("json");

    // Build the optional OpenTelemetry layer + provider
    let (otel_layer, provider) = init_otel_layer();

    // Build the optional Loki layer + background task
    let (loki_layer, loki_task) = init_loki_layer();

    // Use boxed layers to avoid monomorphization headaches with Option<OpenTelemetryLayer<S>>
    let fmt_layer: Box<dyn Layer<_> + Send + Sync> = if use_json {
        Box::new(fmt::layer().json())
    } else {
        Box::new(fmt::layer())
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(otel_layer)
        .with(loki_layer)
        .with(fmt_layer)
        .init();

    if provider.is_some() {
        tracing::info!("OpenTelemetry trace export enabled");
    }

    // Spawn the Loki background task after the subscriber is initialized
    let loki_handle = loki_task.map(|task| {
        tracing::info!("Loki log shipping enabled");
        tokio::spawn(task)
    });

    TelemetryGuard {
        provider,
        _loki_task: loki_handle,
    }
}

/// Try to build an OpenTelemetry tracing layer from env vars.
///
/// Returns `(Some(layer), Some(provider))` when `OTEL_EXPORTER_OTLP_ENDPOINT`
/// is set, or `(None, None)` otherwise. The `None` layer is a no-op thanks to
/// `Option<L>: Layer`.
fn init_otel_layer<S>() -> (
    Option<tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::SdkTracer>>,
    Option<opentelemetry_sdk::trace::SdkTracerProvider>,
)
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    // Only enable OpenTelemetry when the endpoint is explicitly configured
    if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_err() {
        return (None, None);
    }

    let service_name = std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "hofvarpnir".into());
    let protocol = std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL").unwrap_or_else(|_| "grpc".into());

    let exporter = match protocol.as_str() {
        "grpc" => opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .build(),
        "http/protobuf" => opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .build(),
        other => {
            eprintln!(
                "Unsupported OTEL_EXPORTER_OTLP_PROTOCOL: {other}. Use 'grpc' or 'http/protobuf'"
            );
            return (None, None);
        }
    };

    let exporter = match exporter {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Failed to create OTLP span exporter: {e}");
            return (None, None);
        }
    };

    let resource = Resource::builder().with_service_name(service_name).build();

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    let tracer = provider.tracer("hofvarpnir");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    (Some(layer), Some(provider))
}

/// Try to build a Loki tracing layer from env vars.
///
/// Returns `(Some(layer), Some(task))` when `LOKI_URL` is set, or
/// `(None, None)` otherwise. The background task must be spawned on the tokio
/// runtime for log delivery to work.
fn init_loki_layer() -> (
    Option<tracing_loki::Layer>,
    Option<tracing_loki::BackgroundTask>,
) {
    let Ok(loki_url) = std::env::var("LOKI_URL") else {
        return (None, None);
    };

    let url = match tracing_loki::url::Url::parse(&loki_url) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("Invalid LOKI_URL: {e}");
            return (None, None);
        }
    };

    let service_name = std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "hofvarpnir".into());

    // The label key is a hardcoded literal and the value comes from
    // OTEL_SERVICE_NAME; only a malformed key can fail here, which cannot
    // happen with a fixed literal.
    #[allow(clippy::expect_used)]
    let mut builder = tracing_loki::builder()
        .label("service", service_name)
        .expect("valid label");

    // Same format as OTEL_EXPORTER_OTLP_HEADERS, so the push token behind the
    // otel host's proxy can be passed to both pipelines as one string.
    if let Ok(raw) = std::env::var("LOKI_HEADERS") {
        for (key, value) in parse_header_list(&raw) {
            builder = match builder.http_header(&key, &value) {
                Ok(b) => b,
                Err(e) => {
                    // Shipping without the header would only earn a 401 per
                    // batch, so refuse up front and say why.
                    eprintln!(
                        "Invalid LOKI_HEADERS entry `{key}`: {e}; Loki log shipping disabled"
                    );
                    return (None, None);
                }
            };
        }
    }

    match builder.build_url(url) {
        Ok((layer, task)) => (Some(layer), Some(task)),
        Err(e) => {
            eprintln!("Failed to create Loki layer: {e}");
            (None, None)
        }
    }
}

/// Parse the OTLP headers format: comma-separated `key=value` pairs, values
/// percent-encoded (`Authorization=Bearer%20<token>`).
///
/// Mirrors how `opentelemetry-otlp` reads `OTEL_EXPORTER_OTLP_HEADERS`: keys
/// and values are trimmed, a value that is not valid percent-encoded UTF-8 is
/// used as written, and entries with an empty key or value are dropped.
fn parse_header_list(input: &str) -> Vec<(String, String)> {
    input
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let Some((key, value)) = entry.split_once('=') else {
                // Never echo the entry: it may be a token missing its key.
                eprintln!("Ignoring a header entry without `=` (expected key=value)");
                return None;
            };
            let value = value.trim();
            let decoded = percent_encoding::percent_decode_str(value)
                .decode_utf8()
                .map_or_else(|_| value.to_string(), std::borrow::Cow::into_owned);
            Some((key.trim().to_string(), decoded))
        })
        .filter(|(key, value)| !key.is_empty() && !value.is_empty())
        .collect()
}

/// Record the OpenTelemetry trace id on the current span's `trace_id` field.
///
/// Log lines carry the fields of every span in scope, so this is what lets a
/// line found in Loki be opened as a trace. The span must declare
/// `trace_id = tracing::field::Empty`; child spans and events inherit it.
///
/// Kameo starts every message handler as a new root span (linked, not
/// parented, to the sender), so each handler that begins a unit of work calls
/// this once at the top. No-op when OTLP export is disabled.
pub fn record_trace_id() {
    record_trace_id_on(&Span::current());
}

fn record_trace_id_on(span: &Span) {
    use opentelemetry::trace::{TraceContextExt, TraceId};
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let trace_id = span.context().span().span_context().trace_id();
    if trace_id != TraceId::INVALID {
        span.record("trace_id", tracing::field::display(trace_id));
    }
}

/// Generates a ULID-based `x-request-id` for each incoming HTTP request.
#[derive(Clone, Copy, Debug, Default)]
pub struct UlidRequestId;

impl MakeRequestId for UlidRequestId {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = Ulid::generate().to_string();
        // ULID is always valid ASCII, so this parse cannot fail
        let header_value = id.parse().ok()?;
        Some(RequestId::new(header_value))
    }
}

/// Creates a tracing span for each HTTP request using OpenTelemetry HTTP semantic conventions.
///
/// Attributes recorded:
/// - `otel.kind` = "server" (required for service graph edge detection)
/// - `http.request.method` — HTTP method (GET, POST, etc.)
/// - `url.path` — request path
/// - `url.query` — query string (if present)
/// - `http.response.status_code` — recorded by [`HttpResponseRecorder`] on response
/// - `request_id` — from `x-request-id` header
#[derive(Clone, Copy, Debug)]
pub struct RequestSpan;

impl<B> MakeSpan<B> for RequestSpan {
    fn make_span(&mut self, request: &Request<B>) -> Span {
        let request_id = request
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");

        let path = request.uri().path();
        let query = request.uri().query().unwrap_or("");

        let span = tracing::span!(
            Level::INFO,
            "HTTP request",
            "otel.kind" = "server",
            "http.request.method" = %request.method(),
            "url.path" = %path,
            "url.query" = %query,
            "http.response.status_code" = tracing::field::Empty,
            request_id = %request_id,
            trace_id = tracing::field::Empty,
        );
        record_trace_id_on(&span);
        span
    }
}

/// Records HTTP response attributes on the request span.
///
/// Used with `tower_http::trace::TraceLayer` to record `http.response.status_code`
/// after the response is generated, enabling proper error rate metrics in service graphs.
#[derive(Clone, Copy, Debug)]
pub struct HttpResponseRecorder;

impl<B> OnResponse<B> for HttpResponseRecorder {
    fn on_response(self, response: &Response<B>, _latency: Duration, span: &Span) {
        span.record(
            "http.response.status_code",
            i64::from(response.status().as_u16()),
        );
    }
}
