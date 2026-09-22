//! `OpenAPI` documentation and endpoint consistency tests.
//!
//! Verifies that:
//! - `/docs` Scalar UI is accessible
//! - `/docs/` redirects permanently to `/docs`
//! - `/docs/openapi.json` returns valid `OpenAPI` spec
//! - All paths in the spec start with `/api`
//! - All documented endpoints are reachable

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::helpers::TestApp;

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn docs_ui_returns_html(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/docs").await;

    response.assert_status_ok();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/html"),
        "Expected HTML content-type, got: {content_type}"
    );
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn docs_trailing_slash_redirects_to_docs(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/docs/").await;

    // 308 Permanent Redirect: `/docs/` is not a distinct resource, it's the same
    // Scalar UI reachable at the canonical `/docs` - a permanent redirect that
    // preserves the request method (unlike 301, historically rewritten to GET by
    // some clients, or 303, which always forces GET).
    response.assert_status(StatusCode::PERMANENT_REDIRECT);
    let location = response
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(location, "/docs", "Expected redirect Location: /docs");
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn openapi_json_returns_valid_spec(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/docs/openapi.json").await;

    response.assert_status_ok();

    let spec: serde_json::Value = response.json();
    assert!(spec.get("openapi").is_some(), "Missing openapi version");
    assert!(spec.get("info").is_some(), "Missing info section");
    assert!(spec.get("paths").is_some(), "Missing paths section");
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn all_openapi_paths_have_api_prefix(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/docs/openapi.json").await;
    response.assert_status_ok();

    let spec: serde_json::Value = response.json();
    let paths = spec.get("paths").and_then(|p| p.as_object());

    assert!(paths.is_some(), "No paths in OpenAPI spec");

    let paths = paths.expect("paths should exist");
    assert!(!paths.is_empty(), "OpenAPI spec has no paths defined");

    for path in paths.keys() {
        assert!(
            path.starts_with("/api"),
            "Path '{path}' does not start with /api"
        );
    }
}

/// No route path carries the `/api` prefix twice.
///
/// `routes!()` derives the axum route from the `#[utoipa::path]` `path`, and
/// `OpenApiRouter::nest` prepends the nest prefix to *both* the route and the
/// spec entry. So a `path` written as a full path instead of relative to its
/// nest — `path = "/api/sources/{id}/reset-order"` under a
/// `.nest("/api/v1/sources", ..)` — produces
/// `/api/v1/sources/api/sources/{id}/reset-order` in the router *and* in the
/// spec, consistently.
///
/// That consistency is why the reachability check above cannot catch it:
/// probing the advertised path hits the handler and gets a normal 401. The
/// endpoint is nonetheless unreachable at any path a client would construct.
/// `sources.rs`'s `reset_entry_order` shipped this way. The duplicated prefix
/// is the one signal that survives, so this asserts on it directly.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn no_openapi_path_repeats_the_api_prefix(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/docs/openapi.json").await;
    response.assert_status_ok();

    let spec: serde_json::Value = response.json();
    let paths = spec
        .get("paths")
        .and_then(|p| p.as_object())
        .expect("paths should exist");

    for path in paths.keys() {
        assert_eq!(
            path.matches("/api/").count(),
            1,
            "path '{path}' repeats the /api prefix, which means its \
             #[utoipa::path] declares a full path instead of one relative to \
             its nest prefix"
        );
    }
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn all_openapi_get_endpoints_are_reachable(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/docs/openapi.json").await;
    response.assert_status_ok();

    let spec: serde_json::Value = response.json();
    let paths = spec
        .get("paths")
        .and_then(|p| p.as_object())
        .expect("paths should exist");

    for (path, methods) in paths {
        let methods = methods.as_object().expect("methods should be an object");

        // Only test GET endpoints without path parameters
        if methods.contains_key("get") && !path.contains('{') {
            let response = app.server.get(path).await;

            // Should not be 404 (endpoint exists)
            // May be 401 (auth required) or 200 (success)
            let status = response.status_code();
            assert_ne!(
                status.as_u16(),
                404,
                "GET {path} returned 404 - endpoint not found"
            );
        }
    }
}
