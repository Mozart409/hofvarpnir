//! Maud page templates and htmx partial endpoints.
#![allow(clippy::wildcard_imports)] // sibling modules share items through `use super::*`

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Form, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Redirect, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use chrono::{NaiveDate, Utc};
use futures::stream::{Stream, StreamExt, unfold};
use hof_api::AppState;
use hof_core::{
    actors::{
        cleanup::{GetCleanupStatus, RunCleanup},
        download_supervisor::{CancelDownload, EnqueueDownload},
        jellyfin_metadata::TriggerSourceMetadata,
        scheduler::{DRAINING_REFUSAL_MESSAGE, IndexSource, PAUSED_REFUSAL_PREFIX},
    },
    auth::generate_api_key,
    db::{self, CreateApiKey, CreateProfile, CreateSource, UpdateProfile, UpdateSource},
    domain::{
        activity::{ActivityEventType, ActivitySeverity},
        api_key::{ApiKey, ApiKeyEventType, ApiKeyScope},
        profile::{OutputPreset, Profile, Quality},
        source::{EntryOrder, Source, SourceType},
        system::IssueSeverity,
        video::{Video, VideoStatus},
    },
    ytdlp::validate_output_template,
};
use maud::{DOCTYPE, Markup, PreEscaped, Render, html};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tower_sessions::Session;
use ulid::Ulid;

use crate::auth::AuthUser;

// Sibling modules reach shared items through these globs.
mod activity;
mod api_keys;
mod components;
mod dashboard;
mod downloads;
mod forms;
mod login;
mod profiles;
mod schedule;
mod sources;
mod urls;

use self::activity::*;
use self::api_keys::*;
pub(crate) use self::components::*;
use self::dashboard::*;
use self::downloads::*;
use self::forms::*;
use self::login::*;
use self::profiles::*;
use self::schedule::*;
use self::sources::*;
use self::urls::*;

/// Static assets embedded at compile time.
#[derive(Embed)]
#[folder = "assets/"]
struct Assets;

// ============================================================================
// SSE Utilities
// ============================================================================

/// Serve embedded static assets.
async fn serve_asset(Path(path): Path<String>) -> Response {
    match Assets::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref())],
                content.data.into_owned(),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavItem {
    Dashboard,
    Profiles,
    Sources,
    Downloads,
    Activity,
    Schedule,
    ApiKeys,
    Runtime,
}

// ============================================================================
// Flash Messages
// ============================================================================

const FLASH_KEY: &str = "flash_message";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FlashMessage {
    pub(crate) level: String,
    pub(crate) message: String,
}

pub(crate) async fn set_flash(session: &Session, level: &str, message: &str) {
    let flash = FlashMessage {
        level: level.to_string(),
        message: message.to_string(),
    };
    let _ = session.insert(FLASH_KEY, flash).await;
}

pub(crate) async fn take_flash(session: &Session) -> Option<FlashMessage> {
    let flash: Option<FlashMessage> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some() {
        let _ = session.remove::<FlashMessage>(FLASH_KEY).await;
    }
    flash
}

pub fn router(state: AppState, oidc_enabled: bool) -> Router {
    Router::new()
        // Auth routes (no session required)
        .route(
            "/login",
            get(move |session: Session| async move { login_page(session, oidc_enabled).await })
                .post(login),
        )
        .route("/register", get(register_page).post(register))
        .route("/logout", post(logout))
        // Protected routes
        .route("/", get(index))
        .route("/dashboard", get(dashboard_page))
        .route("/profiles", get(profiles_page).post(create_profile))
        .route("/profiles/{id}", post(update_profile))
        .route("/profiles/{id}/delete", post(delete_profile))
        .route("/sources", get(sources_page).post(create_source))
        .route("/sources/{id}", get(source_detail_page).post(update_source))
        .route("/sources/{id}/delete", post(delete_source))
        .route("/sources/{id}/toggle", post(toggle_source_enabled))
        .route(
            "/sources/{id}/toggle-cleanup-exclusion",
            post(toggle_source_cleanup_exclusion),
        )
        .route("/sources/{id}/index", post(trigger_index))
        .route("/sources/{id}/metadata", post(trigger_metadata))
        .route("/sources/{id}/reset-order", post(redetect_source_order))
        .route("/web/sources/events", get(sources_events_sse))
        .route("/web/sources/{id}/events", get(source_detail_events_sse))
        .route("/downloads", get(downloads_page))
        .route("/downloads/{id}/retry", post(retry_download))
        .route("/downloads/{id}/download-now", post(download_now))
        .route("/downloads/{id}/cancel", post(cancel_download))
        .route("/downloads/{id}/delete", post(delete_download))
        .route("/downloads/bulk", post(bulk_download_action))
        .route("/web/downloads/list", get(downloads_list_partial))
        .route("/web/downloads/events", get(downloads_events_sse))
        .route("/web/system-banner", get(system_banner))
        .route("/web/dashboard/events", get(dashboard_events_sse))
        .route("/activity", get(activity_page))
        .route("/web/activity/list", get(activity_list_partial))
        .route("/web/activity/events", get(activity_events_sse))
        .route("/schedule", get(schedule_page))
        .route("/schedule/cleanup", post(trigger_cleanup))
        // API Keys management
        .route(
            "/settings/api-keys",
            get(api_keys_page).post(create_api_key),
        )
        .route("/settings/api-keys/{id}/roll", post(roll_api_key))
        .route("/settings/api-keys/{id}/delete", post(delete_api_key))
        .route("/settings/api-keys/{id}/events", get(api_key_events))
        // Static assets (embedded at compile time)
        .merge(crate::runtime::routes())
        .route("/assets/{*path}", get(serve_asset))
        // Fallback for unmatched routes
        .fallback(not_found)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Query-value encoding
    // ------------------------------------------------------------------

    #[test]
    fn encode_query_value_escapes_ampersand_and_space() {
        assert_eq!(encode_query_value("rust & go"), "rust%20%26%20go");
    }

    #[test]
    fn encode_query_value_escapes_fragment_and_equals() {
        assert_eq!(encode_query_value("a#b=c"), "a%23b%3Dc");
    }

    #[test]
    fn encode_query_value_leaves_unreserved_characters_alone() {
        assert_eq!(encode_query_value("a-b_c.d~e9"), "a-b_c.d~e9");
    }

    // ------------------------------------------------------------------
    // Downloads URL construction
    // ------------------------------------------------------------------

    #[test]
    fn downloads_url_omits_defaults() {
        assert_eq!(downloads_page_url(None, None, 1, 25), "/downloads");
    }

    #[test]
    fn downloads_url_includes_non_default_params() {
        assert_eq!(
            downloads_page_url(Some("failed"), Some("cats"), 3, 50),
            "/downloads?status=failed&search=cats&page=3&per_page=50"
        );
    }

    #[test]
    fn downloads_url_treats_all_status_as_unfiltered() {
        assert_eq!(downloads_page_url(Some("all"), None, 1, 25), "/downloads");
    }

    #[test]
    fn downloads_url_percent_encodes_search() {
        assert_eq!(
            downloads_page_url(None, Some("a&b"), 1, 25),
            "/downloads?search=a%26b"
        );
    }

    /// A search term containing `&` must not smuggle in an extra parameter.
    #[test]
    fn downloads_url_search_cannot_inject_parameters() {
        let url = downloads_page_url(None, Some("x&status=completed"), 1, 25);
        assert_eq!(url, "/downloads?search=x%26status%3Dcompleted");
        assert!(!url.contains("&status=completed"));
    }

    // ------------------------------------------------------------------
    // Regression: the pushed URL must be the navigable page, never the
    // partial endpoint. Pushing `/web/downloads/list` meant a reload served
    // a bare fragment with no <head>, leaving the page unstyled.
    // ------------------------------------------------------------------

    #[test]
    fn page_urls_and_partial_urls_are_distinct() {
        let page = downloads_page_url(Some("failed"), None, 2, 25);
        let list = downloads_list_url(Some("failed"), None, 2, 25);
        let events = downloads_events_url(Some("failed"), None, 2, 25);

        assert!(page.starts_with("/downloads"));
        assert!(list.starts_with("/web/downloads/list"));
        assert!(events.starts_with("/web/downloads/events"));
        assert_ne!(page, list);
    }

    #[test]
    fn pushed_download_urls_are_never_partial_endpoints() {
        for page in [1, 2, 7] {
            for status in [None, Some("failed"), Some("all")] {
                let pushed = downloads_page_url(status, Some("q"), page, 25);
                assert!(
                    !pushed.starts_with("/web/"),
                    "pushed URL must be navigable, got {pushed}"
                );
            }
        }
    }

    #[test]
    fn pushed_activity_urls_are_never_partial_endpoints() {
        for page in [1, 4] {
            for severity in [None, Some("error"), Some("all")] {
                let filter = ActivityFilter {
                    severity,
                    search: Some("q"),
                    source: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
                };
                let pushed = activity_page_url(filter, page, 50);
                assert!(
                    !pushed.starts_with("/web/"),
                    "pushed URL must be navigable, got {pushed}"
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // Activity URL construction
    // ------------------------------------------------------------------

    #[test]
    fn activity_url_omits_defaults() {
        assert_eq!(
            activity_page_url(ActivityFilter::default(), 1, 50),
            "/activity"
        );
    }

    #[test]
    fn activity_url_includes_non_default_params() {
        let filter = ActivityFilter {
            severity: Some("error"),
            ..ActivityFilter::default()
        };
        assert_eq!(
            activity_page_url(filter, 2, 100),
            "/activity?severity=error&page=2&per_page=100"
        );
    }

    #[test]
    fn activity_url_carries_search_and_source() {
        let filter = ActivityFilter {
            severity: Some("error"),
            search: Some("disk full"),
            source: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        };
        assert_eq!(
            activity_page_url(filter, 1, 50),
            "/activity?severity=error&search=disk%20full&source=01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
    }

    #[test]
    fn activity_url_treats_all_severity_as_unfiltered() {
        let filter = ActivityFilter {
            severity: Some("all"),
            ..ActivityFilter::default()
        };
        assert_eq!(activity_page_url(filter, 1, 50), "/activity");
    }

    #[test]
    fn activity_url_search_cannot_inject_parameters() {
        let filter = ActivityFilter {
            search: Some("x&severity=error"),
            ..ActivityFilter::default()
        };
        let url = activity_page_url(filter, 1, 50);
        assert_eq!(url, "/activity?search=x%26severity%3Derror");
        assert!(!url.contains("&severity=error"));
    }

    // ------------------------------------------------------------------
    // Post-action redirect target preserves list state
    // ------------------------------------------------------------------

    #[test]
    fn downloads_return_url_preserves_page_and_filters() {
        let query = DownloadsQuery {
            status: Some("failed".to_owned()),
            search: Some("cats".to_owned()),
            page: Some(4),
            per_page: Some(50),
        };
        assert_eq!(
            downloads_return_url(&query),
            "/downloads?status=failed&search=cats&page=4&per_page=50"
        );
    }

    #[test]
    fn downloads_return_url_defaults_to_bare_path() {
        let query = DownloadsQuery {
            status: None,
            search: None,
            page: None,
            per_page: None,
        };
        assert_eq!(downloads_return_url(&query), "/downloads");
    }

    #[test]
    fn downloads_return_url_ignores_blank_search() {
        let query = DownloadsQuery {
            status: None,
            search: Some("   ".to_owned()),
            page: Some(2),
            per_page: None,
        };
        assert_eq!(downloads_return_url(&query), "/downloads?page=2");
    }

    // ------------------------------------------------------------------
    // Bulk action eligibility
    // ------------------------------------------------------------------

    #[test]
    fn bulk_action_parses_known_verbs() {
        assert_eq!(BulkAction::parse("retry"), Some(BulkAction::Retry));
        assert_eq!(BulkAction::parse("cancel"), Some(BulkAction::Cancel));
        assert_eq!(BulkAction::parse("delete"), Some(BulkAction::Delete));
    }

    #[test]
    fn bulk_action_rejects_unknown_verbs() {
        assert_eq!(BulkAction::parse("purge"), None);
        assert_eq!(BulkAction::parse(""), None);
        assert_eq!(BulkAction::parse("RETRY"), None);
    }

    /// These must stay in step with the guards in the single-item handlers,
    /// or a bulk action would accept work the individual button refuses.
    #[test]
    fn bulk_retry_matches_single_retry_eligibility() {
        for status in [
            VideoStatus::Failed,
            VideoStatus::PermanentlyFailed,
            VideoStatus::Cleaned,
        ] {
            assert!(BulkAction::Retry.allows(&status));
        }
        for status in [
            VideoStatus::Pending,
            VideoStatus::Downloading,
            VideoStatus::Completed,
            VideoStatus::Skipped,
        ] {
            assert!(!BulkAction::Retry.allows(&status));
        }
    }

    #[test]
    fn bulk_cancel_matches_single_cancel_eligibility() {
        for status in [VideoStatus::Pending, VideoStatus::Downloading] {
            assert!(BulkAction::Cancel.allows(&status));
        }
        for status in [
            VideoStatus::Completed,
            VideoStatus::Failed,
            VideoStatus::Cleaned,
            VideoStatus::PermanentlyFailed,
            VideoStatus::Skipped,
        ] {
            assert!(!BulkAction::Cancel.allows(&status));
        }
    }

    #[test]
    fn bulk_delete_only_accepts_completed() {
        assert!(BulkAction::Delete.allows(&VideoStatus::Completed));
        for status in [
            VideoStatus::Pending,
            VideoStatus::Downloading,
            VideoStatus::Failed,
            VideoStatus::Cleaned,
            VideoStatus::PermanentlyFailed,
            VideoStatus::Skipped,
        ] {
            assert!(!BulkAction::Delete.allows(&status));
        }
    }

    /// The slug is rendered into `data-status` and parsed back by the status
    /// filter links, so the two directions must agree exactly.
    #[test]
    fn download_status_slug_round_trips_through_parse() {
        for status in [
            VideoStatus::Pending,
            VideoStatus::AwaitingQuality,
            VideoStatus::Downloading,
            VideoStatus::Completed,
            VideoStatus::Failed,
            VideoStatus::Skipped,
            VideoStatus::Cleaned,
            VideoStatus::PermanentlyFailed,
        ] {
            let slug = download_status_slug(&status);
            assert_eq!(
                parse_download_status(Some(slug)),
                Some(status.clone()),
                "slug {slug} did not round-trip"
            );
        }
    }

    #[test]
    fn bulk_action_url_carries_list_state() {
        let url = downloads_url("/downloads/bulk", Some("failed"), Some("cats"), 3, 50);
        assert_eq!(
            url,
            "/downloads/bulk?status=failed&search=cats&page=3&per_page=50"
        );
    }

    // ------------------------------------------------------------------
    // Source search predicate (sources + schedule pages)
    // ------------------------------------------------------------------

    fn test_source(custom_name: Option<&str>, channel_title: Option<&str>, url: &str) -> Source {
        Source {
            id: Ulid::from_string("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid literal"),
            profile_id: Ulid::from_string("01BX5ZZKBKACTAV9WEVGEMMVRZ")
                .expect("valid ulid literal"),
            url: url.to_owned(),
            source_type: hof_core::domain::source::SourceType::Channel,
            custom_name: custom_name.map(ToOwned::to_owned),
            enabled: true,
            exclude_from_cleanup: false,
            index_frequency_secs: 43200,
            cutoff_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date literal"),
            retention_days: None,
            entry_order: hof_core::domain::source::EntryOrder::Unknown,
            entry_order_detected_at: None,
            last_indexed_at: None,
            last_error: None,
            index_error_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            channel_id: None,
            channel_title: channel_title.map(ToOwned::to_owned),
            channel_description: None,
            channel_thumbnail_url: None,
            jellyfin_metadata_at: None,
        }
    }

    #[test]
    fn source_search_matches_custom_name_case_insensitively() {
        let source = test_source(Some("My Cooking Channel"), None, "https://example.com/x");
        assert!(source_matches_search(&source, "cooking"));
        assert!(source_matches_search(&source, "COOKING"));
    }

    #[test]
    fn source_search_matches_channel_title() {
        let source = test_source(None, Some("Veritasium"), "https://example.com/x");
        assert!(source_matches_search(&source, "veritas"));
    }

    #[test]
    fn source_search_matches_url() {
        let source = test_source(None, None, "https://youtube.com/@somechannel");
        assert!(source_matches_search(&source, "somechannel"));
    }

    /// A custom name must not mask a channel-title match, and vice versa —
    /// which field is populated varies by source type and indexing state.
    #[test]
    fn source_search_checks_every_name_field() {
        let source = test_source(Some("Label"), Some("Channel"), "https://example.com/slug");
        assert!(source_matches_search(&source, "Label"));
        assert!(source_matches_search(&source, "Channel"));
        assert!(source_matches_search(&source, "slug"));
    }

    #[test]
    fn source_search_rejects_non_matches() {
        let source = test_source(Some("Cooking"), Some("Food"), "https://example.com/x");
        assert!(!source_matches_search(&source, "astronomy"));
    }

    #[test]
    fn downloads_return_url_clamps_non_positive_page() {
        let query = DownloadsQuery {
            status: None,
            search: None,
            page: Some(0),
            per_page: None,
        };
        assert_eq!(downloads_return_url(&query), "/downloads");
    }
}
