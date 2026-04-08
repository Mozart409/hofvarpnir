//! Telemetry initialization: tracing subscriber with layered architecture.
//!
//! Supports:
//! - Console output with optional JSON formatting (`LOG_FORMAT=json`)
//! - Environment-based log filtering via `RUST_LOG`
//! - Per-request ID generation and propagation via `x-request-id` header
//! - OpenTelemetry trace export via OTLP/gRPC (when `OTEL_EXPORTER_OTLP_ENDPOINT` is set)
//! - Log shipping to Grafana Loki with `trace_id` correlation (when `LOKI_URL` is set)
//!
//! Both the OpenTelemetry and Loki pipelines are opt-in: if the respective env
//! vars are unset the app behaves exactly as before.

use http::Request;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::Resource;
use tower_http::request_id::{MakeRequestId, RequestId};
use tower_http::trace::MakeSpan;
use tracing::Level;
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
    pub fn shutdown(&self) {
        if let Some(provider) = &self.provider
            && let Err(e) = provider.shutdown()
        {
            tracing::warn!(error = %e, "Failed to shut down OpenTelemetry tracer provider");
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
/// - `OTEL_EXPORTER_OTLP_ENDPOINT` — if set, enables OTLP/gRPC trace export
/// - `OTEL_SERVICE_NAME` — service name reported to the collector (default: `hofvarpnir`)
/// - `LOKI_URL` — if set, ships logs to Grafana Loki (e.g. `http://localhost:3100`)
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

    let exporter = match opentelemetry_otlp::SpanExporterBuilder::new()
        .with_tonic()
        .build()
    {
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

    let builder = tracing_loki::builder()
        .label("service", service_name)
        .expect("valid label");

    match builder.build_url(url) {
        Ok((layer, task)) => (Some(layer), Some(task)),
        Err(e) => {
            eprintln!("Failed to create Loki layer: {e}");
            (None, None)
        }
    }
}

/// Generates a ULID-based `x-request-id` for each incoming HTTP request.
#[derive(Clone, Copy, Debug, Default)]
pub struct UlidRequestId;

impl MakeRequestId for UlidRequestId {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = Ulid::new().to_string();
        // ULID is always valid ASCII, so this parse cannot fail
        let header_value = id.parse().ok()?;
        Some(RequestId::new(header_value))
    }
}

/// Creates a tracing span for each HTTP request, including the `x-request-id` as a field.
#[derive(Clone, Copy, Debug)]
pub struct RequestSpan;

impl<B> MakeSpan<B> for RequestSpan {
    fn make_span(&mut self, request: &Request<B>) -> tracing::Span {
        let request_id = request
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");

        tracing::span!(
            Level::INFO,
            "request",
            method = %request.method(),
            uri = %request.uri(),
            request_id = %request_id,
        )
    }
}
