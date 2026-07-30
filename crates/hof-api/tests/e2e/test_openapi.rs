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
