//! End-to-end web frontend tests.
//!
//! These drive the real `hof-web` router, with the real `tower-sessions`
//! session layer and a real login, and assert on rendered HTML. `pages.rs`
//! has a large unit-test module, but nothing there renders a page through the
//! router — these cover that gap.

// Relax some clippy lints for test code
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::literal_string_with_formatting_args)]
#![allow(dead_code)]
// `clippy.toml` sets `allow-unwrap-in-tests` / `allow-expect-in-tests`, but
// those only apply inside `#[test]` functions and `#[cfg(test)]` modules. This
// is an integration-test crate whose failures live in plain helper functions,
// so the exemption has to be declared here instead.
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

mod helpers;

use axum_test::http::StatusCode;
use helpers::{ActivityBuilder, ProfileBuilder, SourceBuilder, UserBuilder, VideoBuilder};
use hof_core::domain::{
    activity::{ActivityEventType, ActivitySeverity},
    video::VideoStatus,
};

/// Every protected page redirects an unauthenticated visitor to `/login`.
///
/// Covers the page routes and the htmx partial endpoints together: a partial
/// that answered unauthenticated would leak rows into the DOM of a logged-out
/// browser.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn protected_pages_redirect_when_unauthenticated(pool: sqlx::PgPool) {
    let app = helpers::TestWebApp::new(pool.clone()).await;

    for route in [
        "/dashboard",
        "/downloads",
        "/sources",
        "/activity",
        "/schedule",
        "/web/downloads/list",
        "/web/activity/list",
    ] {
        let response = app.server.get(route).await;

        assert_eq!(
            response.status_code(),
            StatusCode::SEE_OTHER,
            "{route} should redirect when unauthenticated"
        );

        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();

        assert!(
            location.contains("/login"),
            "{route} should redirect to /login, got {location}"
        );
    }
}

/// The downloads page offers a retry control only for the failed video,
/// renders the completed video's delivered quality, and its status filter
/// actually filters.
///
/// The delivered quality is the interesting half: `videos.video_height` /
/// `video_codec` record what the platform *served*, which diverges from the
/// profile's requested quality, and this is the only place that divergence
/// surfaces in the UI.
///
/// Note the row deliberately does *not* render `videos.last_error` — the list
/// shows a status badge, the attempt count and the actions. The error text
/// reaches the UI through the activity feed instead.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn downloads_page_renders_status_error_and_delivered_quality(pool: sqlx::PgPool) {
    let app = helpers::TestWebApp::new(pool.clone()).await;

    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    VideoBuilder::new(source.id)
        .title("Doomed Clip")
        .status(VideoStatus::Failed)
        .last_error("[DOWNLOAD_VERIFICATION_FAILED] zero run at 8 MiB")
        .build(&pool)
        .await;

    VideoBuilder::new(source.id)
        .title("Delivered Clip")
        .status(VideoStatus::Completed)
        .video_height(720)
        .video_codec("av01")
        .build(&pool)
        .await;

    VideoBuilder::new(source.id)
        .title("Waiting Clip")
        .status(VideoStatus::Pending)
        .build(&pool)
        .await;

    app.login_as(&user).await;

    let body = app.server.get("/downloads").await.text();

    assert!(
        body.contains("Doomed Clip"),
        "failed video should be listed"
    );
    assert!(
        body.contains("Delivered Clip"),
        "completed video should be listed"
    );
    assert!(
        body.contains("720"),
        "the delivered height should be rendered for the completed video"
    );

    // Only the failed video is retryable, so exactly one retry form should be
    // rendered and it should target that video.
    let retry_actions: Vec<_> = body
        .match_indices("/retry")
        .map(|(idx, _)| &body[idx.saturating_sub(40)..idx])
        .collect();
    assert_eq!(
        retry_actions.len(),
        1,
        "only the failed video should offer a retry control, found {}",
        retry_actions.len()
    );

    // Filtering to `failed` must drop the other two rows, not merely
    // highlight the filter.
    let filtered = app.server.get("/downloads?status=failed").await.text();

    assert!(
        filtered.contains("Doomed Clip"),
        "the failed video should survive the failed filter"
    );
    assert!(
        !filtered.contains("Delivered Clip"),
        "a completed video must not appear under the failed filter"
    );
    assert!(
        !filtered.contains("Waiting Clip"),
        "a pending video must not appear under the failed filter"
    );
}

/// Listings are ordered by publish date, newest first — not by the order
/// hofvarpnir happened to discover the videos.
///
/// The listings used to be `ORDER BY created_at DESC`, i.e. insertion order.
/// Indexing discovers videos in batches over many runs, and a run that
/// backfills older videos inserts them after newer ones, so a channel that
/// uploads strictly in sequence rendered as a jumble: 09-21, 09-16, 09-17,
/// 09-18, 09-14. That is what this test seeds — deliberately shuffling the
/// insertion order relative to the publish order, so a regression to
/// `created_at` fails here rather than looking plausible.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn downloads_page_orders_videos_by_publish_date(pool: sqlx::PgPool) {
    let app = helpers::TestWebApp::new(pool.clone()).await;

    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    let day = |d: u32| {
        chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 9, d, 12, 0, 0)
            .single()
            .expect("valid timestamp")
    };

    // Insertion order deliberately differs from publish order.
    for (title, published) in [
        ("Sept Twentyfirst", day(21)),
        ("Sept Sixteenth", day(16)),
        ("Sept Eighteenth", day(18)),
        ("Sept Fourteenth", day(14)),
    ] {
        VideoBuilder::new(source.id)
            .title(title)
            .status(VideoStatus::Completed)
            .published_at(published)
            .build(&pool)
            .await;
    }

    app.login_as(&user).await;

    let body = app.server.get("/downloads").await.text();

    let order = rendered_order(
        &body,
        &[
            "Sept Twentyfirst",
            "Sept Eighteenth",
            "Sept Sixteenth",
            "Sept Fourteenth",
        ],
    );

    assert_eq!(
        order,
        vec![
            "Sept Twentyfirst",
            "Sept Eighteenth",
            "Sept Sixteenth",
            "Sept Fourteenth",
        ],
        "downloads page must list newest published first"
    );
}

/// A video with no publish date sorts last rather than jumping to the top.
///
/// `videos.published_at` is nullable, so the ordering has to say where nulls
/// go. Without `NULLS LAST` Postgres sorts them *first* on a `DESC` sort, and
/// a single undated video would head the list.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn videos_without_a_publish_date_sort_last(pool: sqlx::PgPool) {
    let app = helpers::TestWebApp::new(pool.clone()).await;

    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    // Insertion order matters for what this proves. The undated video is
    // inserted *second*, so it has the newer `created_at` and the old
    // `ORDER BY created_at DESC` would have put it first. Only
    // `published_at DESC NULLS LAST` sorts it last, so this fails if either
    // the column or the null handling regresses.
    VideoBuilder::new(source.id)
        .title("Dated Clip")
        .status(VideoStatus::Completed)
        .published_at(
            chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 9, 14, 12, 0, 0)
                .single()
                .expect("valid timestamp"),
        )
        .build(&pool)
        .await;

    VideoBuilder::new(source.id)
        .title("Undated Clip")
        .status(VideoStatus::Completed)
        .build(&pool)
        .await;

    app.login_as(&user).await;

    let body = app.server.get("/downloads").await.text();

    assert_eq!(
        rendered_order(&body, &["Dated Clip", "Undated Clip"]),
        vec!["Dated Clip", "Undated Clip"],
        "an undated video must sort after dated ones, not ahead of them"
    );
}

/// The order `titles` appear in within `body`, for those that appear at all.
fn rendered_order<'a>(body: &str, titles: &[&'a str]) -> Vec<&'a str> {
    let mut found: Vec<(usize, &str)> = titles
        .iter()
        .filter_map(|t| body.find(t).map(|idx| (idx, *t)))
        .collect();
    found.sort_unstable_by_key(|(idx, _)| *idx);
    found.into_iter().map(|(_, t)| t).collect()
}

/// The source detail page shows the custom name rather than the raw URL, and
/// lists the source's videos.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn source_detail_page_prefers_custom_name_and_lists_videos(pool: sqlx::PgPool) {
    let app = helpers::TestWebApp::new(pool.clone()).await;

    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;

    let opaque_url = "https://youtube.com/channel/UCopaque0000000000000";
    let source = SourceBuilder::new(profile.id)
        .url(opaque_url)
        .custom_name("Hand Tools Rescue")
        .build(&pool)
        .await;

    VideoBuilder::new(source.id)
        .title("Restoring A Bench Vise")
        .status(VideoStatus::Completed)
        .build(&pool)
        .await;

    VideoBuilder::new(source.id)
        .title("Sharpening A Hand Plane")
        .status(VideoStatus::Pending)
        .build(&pool)
        .await;

    app.login_as(&user).await;

    let body = app
        .server
        .get(&format!("/sources/{}", source.id))
        .await
        .text();

    assert!(
        body.contains("Hand Tools Rescue"),
        "the custom name should be the source's display name"
    );
    assert!(
        body.contains("Restoring A Bench Vise") && body.contains("Sharpening A Hand Plane"),
        "both of the source's videos should be listed"
    );
}

/// The activity page filters by severity and searches message substrings.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn activity_page_filters_by_severity_and_search(pool: sqlx::PgPool) {
    let app = helpers::TestWebApp::new(pool.clone()).await;

    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    ActivityBuilder::new(
        ActivityEventType::DownloadFailed,
        ActivitySeverity::Error,
        "Download failed: region locked in your country",
    )
    .source_id(source.id)
    .build(&pool)
    .await;

    ActivityBuilder::new(
        ActivityEventType::SourceIndexed,
        ActivitySeverity::Info,
        "Source indexed: found 15 new videos",
    )
    .source_id(source.id)
    .build(&pool)
    .await;

    app.login_as(&user).await;

    let all = app.server.get("/activity").await.text();
    assert!(
        all.contains("region locked"),
        "error event should be listed"
    );
    assert!(
        all.contains("found 15 new videos"),
        "info event should be listed"
    );

    let errors_only = app.server.get("/activity?severity=error").await.text();
    assert!(
        errors_only.contains("region locked"),
        "error event should survive the error filter"
    );
    assert!(
        !errors_only.contains("found 15 new videos"),
        "info event must not appear under the error filter"
    );

    let searched = app.server.get("/activity?search=region").await.text();
    assert!(
        searched.contains("region locked"),
        "search should match a message substring"
    );
    assert!(
        !searched.contains("found 15 new videos"),
        "search should exclude non-matching events"
    );
}

/// No rendered `hx-push-url` ever points at a partial endpoint.
///
/// `pages.rs` guards this at the unit level
/// (`pushed_download_urls_are_never_partial_endpoints`); this checks the
/// invariant on what the router actually serves. A partial URL pushed into
/// browser history renders a bare fragment with no layout on reload.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn pushed_urls_are_never_partial_endpoints(pool: sqlx::PgPool) {
    let app = helpers::TestWebApp::new(pool.clone()).await;

    let user = UserBuilder::new().build(&pool).await;
    let profile = ProfileBuilder::new(user.id).build(&pool).await;
    let source = SourceBuilder::new(profile.id).build(&pool).await;

    VideoBuilder::new(source.id)
        .title("Pushed Url Fixture")
        .status(VideoStatus::Completed)
        .build(&pool)
        .await;

    ActivityBuilder::new(
        ActivityEventType::SourceIndexed,
        ActivitySeverity::Info,
        "Source indexed for the push-url fixture",
    )
    .source_id(source.id)
    .build(&pool)
    .await;

    app.login_as(&user).await;

    for route in [
        "/downloads",
        "/activity",
        "/web/downloads/list",
        "/web/activity/list",
    ] {
        let response = app.server.get(route).await;
        assert_eq!(
            response.status_code(),
            StatusCode::OK,
            "{route} should render for a logged-in user"
        );

        for pushed in pushed_urls(&response.text()) {
            assert!(
                !pushed.starts_with("/web/"),
                "{route} pushed a partial endpoint into history: {pushed}"
            );
        }
    }
}

/// Extract every `hx-push-url="..."` value from rendered markup.
fn pushed_urls(body: &str) -> Vec<String> {
    body.match_indices("hx-push-url=\"")
        .filter_map(|(start, marker)| {
            let value_start = start + marker.len();
            body[value_start..]
                .find('"')
                .map(|end| body[value_start..value_start + end].to_string())
        })
        .collect()
}
