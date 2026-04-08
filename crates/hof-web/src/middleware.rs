//! HTTP middleware for request metrics.

use std::time::Instant;

use axum::{body::Body, extract::MatchedPath, middleware::Next, response::IntoResponse};
use http::Request;
use metrics::{counter, histogram};

use hof_core::metrics::{HTTP_REQUEST_DURATION_SECONDS, HTTP_REQUESTS_TOTAL};

/// Records HTTP request count and duration metrics.
///
/// Must be added as an axum middleware layer. Uses `MatchedPath` when available
/// to group metrics by route pattern rather than concrete path values.
pub async fn http_metrics(request: Request<Body>, next: Next) -> impl IntoResponse {
    let method = request.method().to_string();
    let path = request.extensions().get::<MatchedPath>().map_or_else(
        || request.uri().path().to_owned(),
        |m| m.as_str().to_owned(),
    );

    let start = Instant::now();
    let response = next.run(request).await;
    let duration = start.elapsed().as_secs_f64();

    let status = response.status().as_u16().to_string();

    counter!(HTTP_REQUESTS_TOTAL, "method" => method.clone(), "path" => path.clone(), "status" => status).increment(1);
    histogram!(HTTP_REQUEST_DURATION_SECONDS, "method" => method, "path" => path).record(duration);

    response
}
