//! Telemetry initialization: tracing subscriber with layered architecture.
//!
//! Supports:
//! - Console output with optional JSON formatting (`LOG_FORMAT=json`)
//! - Environment-based log filtering via `RUST_LOG`
//! - Per-request ID generation and propagation via `x-request-id` header
//!
//! Future phases will add OpenTelemetry and Loki layers here.

use http::Request;
use tower_http::request_id::{MakeRequestId, RequestId};
use tower_http::trace::MakeSpan;
use tracing::Level;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
use ulid::Ulid;

/// Initialize the global tracing subscriber.
///
/// Reads configuration from environment variables:
/// - `RUST_LOG` — filter directives (default: `info`)
/// - `LOG_FORMAT` — `json` for structured JSON output, anything else for human-readable (default)
///
/// # Panics
///
/// Panics if a global subscriber has already been set.
pub fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let log_format = std::env::var("LOG_FORMAT").unwrap_or_default();

    let registry = tracing_subscriber::registry().with(env_filter);

    if log_format.eq_ignore_ascii_case("json") {
        registry.with(fmt::layer().json()).init();
    } else {
        registry.with(fmt::layer()).init();
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
