pub mod pages;

use axum::Router;

/// Build the web frontend router with Maud + htmx routes.
///
/// Mount at `/` in the top-level application.
pub fn router() -> Router {
    Router::new()
}
