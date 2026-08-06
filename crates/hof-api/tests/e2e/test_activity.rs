//! Activity endpoint tests, including the unhealthy-sources health report.

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use sqlx::PgPool;
use ulid::Ulid;

use crate::helpers::{ApiKeyBuilder, ProfileBuilder, SourceBuilder, TestApp, UserBuilder};

/// Insert a single activity event with an explicit timestamp so ordering is
/// deterministic across the success/error streak assertions.
async fn insert_event(
    pool: &PgPool,
    source_id: Ulid,
    event_type: &str,
    severity: &str,
    message: &str,
    created_at: chrono::DateTime<Utc>,
) {
    sqlx::query(
        r"
        INSERT INTO activity_events (id, event_type, severity, message, source_id, created_at)
        VALUES ($1, $2::activity_event_type, $3::activity_severity, $4, $5, $6)
        ",
    )
    .bind(Ulid::generate().to_string())
    .bind(event_type)
    .bind(severity)
    .bind(message)
    .bind(source_id.to_string())
    .bind(created_at)
    .execute(pool)
    .await
    .expect("insert activity event");
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn unhealthy_sources_without_auth_returns_401(pool: PgPool) {
    let app = TestApp::new(pool).await;

    let response = app.server.get("/api/v1/activity/unhealthy-sources").await;

    response.assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn unhealthy_sources_reports_failing_and_excludes_recovered(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    // Source A: 3 consecutive errors, never succeeded -> should be reported.
    let failing = SourceBuilder::new(profile.id)
        .url("https://youtube.com/playlist?list=FAILING")
        .playlist()
        .custom_name("Failing Source")
        .build(&pool)
        .await;
    let base = Utc::now() - Duration::days(3);
    for i in 0..3 {
        insert_event(
            &pool,
            failing.id,
            "source_error",
            "error",
            "rate limited: age restricted",
            base + Duration::hours(i),
        )
        .await;
    }

    // Source B: errored twice, then succeeded -> recovered, must be excluded.
    let recovered = SourceBuilder::new(profile.id)
        .url("https://youtube.com/playlist?list=RECOVERED")
        .playlist()
        .build(&pool)
        .await;
    insert_event(&pool, recovered.id, "source_error", "error", "boom", base).await;
    insert_event(
        &pool,
        recovered.id,
        "source_error",
        "error",
        "boom",
        base + Duration::hours(1),
    )
    .await;
    insert_event(
        &pool,
        recovered.id,
        "source_indexed",
        "success",
        "Indexed successfully — 1 new, 0 existing, 0 filtered ()",
        base + Duration::hours(2),
    )
    .await;

    let response = app
        .server
        .get("/api/v1/activity/unhealthy-sources?min_errors=3")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();

    assert_eq!(body["threshold"], 3);
    assert_eq!(body["total"], 1, "only the failing source should be listed");

    let listed = &body["sources"][0];
    assert_eq!(listed["source_id"], failing.id.to_string());
    assert_eq!(listed["consecutive_errors"], 3);
    assert_eq!(listed["custom_name"], "Failing Source");
    assert_eq!(listed["last_error_message"], "rate limited: age restricted");
}

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn unhealthy_sources_respects_min_errors_threshold(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;

    let source = SourceBuilder::new(profile.id).build(&pool).await;
    let base = Utc::now() - Duration::days(1);
    for i in 0..3 {
        insert_event(
            &pool,
            source.id,
            "source_error",
            "error",
            "boom",
            base + Duration::hours(i),
        )
        .await;
    }

    // 3 errors < threshold of 5 -> nothing reported.
    let response = app
        .server
        .get("/api/v1/activity/unhealthy-sources?min_errors=5")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["total"], 0);
}

// ---------------------------------------------------------------------------
// Message search and source filter
// ---------------------------------------------------------------------------

#[sqlx::test(migrations = "../hof-core/migrations")]
async fn activity_search_matches_message_substring(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    let now = Utc::now();
    insert_event(
        &pool,
        source.id,
        "source_error",
        "error",
        "Disk quota exceeded while writing",
        now,
    )
    .await;
    insert_event(
        &pool,
        source.id,
        "source_indexed",
        "success",
        "Indexed 12 new videos",
        now - Duration::minutes(1),
    )
    .await;

    let response = app
        .server
        .get("/api/v1/activity?search=quota")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["total"], 1, "only the matching message should count");
    assert_eq!(body["events"][0]["severity"], "error");
}

/// Search is case-insensitive, matching the ILIKE predicate.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn activity_search_is_case_insensitive(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    insert_event(
        &pool,
        source.id,
        "source_error",
        "error",
        "Disk Quota Exceeded",
        Utc::now(),
    )
    .await;

    let response = app
        .server
        .get("/api/v1/activity?search=QUOTA")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["total"], 1);
}

/// Filtering by source is what the clickable name pill drives.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn activity_filters_by_source_id(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let wanted = SourceBuilder::new(profile.id).build(&pool).await;
    let other = SourceBuilder::new(profile.id).build(&pool).await;

    let now = Utc::now();
    insert_event(&pool, wanted.id, "source_indexed", "success", "A", now).await;
    insert_event(
        &pool,
        other.id,
        "source_indexed",
        "success",
        "B",
        now - Duration::minutes(1),
    )
    .await;

    let response = app
        .server
        .get(&format!("/api/v1/activity?source_id={}", wanted.id))
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["total"], 1);
    assert_eq!(body["events"][0]["source_id"], wanted.id.to_string());
}

/// Search and severity must intersect, not replace one another.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn activity_search_combines_with_severity(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    let now = Utc::now();
    insert_event(&pool, source.id, "source_error", "error", "timeout", now).await;
    insert_event(
        &pool,
        source.id,
        "source_indexed",
        "success",
        "timeout",
        now - Duration::minutes(1),
    )
    .await;

    let response = app
        .server
        .get("/api/v1/activity?search=timeout&severity=error")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(
        body["total"], 1,
        "both predicates must apply, not just the last"
    );
    assert_eq!(body["events"][0]["severity"], "error");
}

/// A blank search must behave as no filter at all.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn activity_blank_search_is_ignored(pool: PgPool) {
    let app = TestApp::new(pool.clone()).await;
    let user = UserBuilder::new().build(&pool).await;
    let key = ApiKeyBuilder::new(user.id).read_only().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    insert_event(
        &pool,
        source.id,
        "source_indexed",
        "success",
        "anything",
        Utc::now(),
    )
    .await;

    let response = app
        .server
        .get("/api/v1/activity?search=%20%20")
        .add_header("Authorization", key.bearer())
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["total"], 1);
}
