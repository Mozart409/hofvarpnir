pub mod routes;

use axum::Router;

/// Build the API router with all JSON + SSE endpoints.
///
/// Mount at `/api` in the top-level application.
pub fn router() -> Router {
    Router::new()
}

/// Build the Scalar `OpenAPI` documentation router.
///
/// Mount at `/docs` in the top-level application.
pub fn scalar_router() -> Router {
    Router::new()
}
