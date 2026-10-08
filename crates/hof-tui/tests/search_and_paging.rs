//! End to end: `/` search and Activity paging, driven through `App`'s key
//! handling and the real `ApiClient` against a mock API.
//!
//! Failure modes these scenarios guard against, written before the code:
//!
//! 1. A row action on a filtered table hits the row at the same position in
//!    the *unfiltered* list (selection index taken as a data index).
//! 2. Keys typed into the prompt fire their normal bindings (`d` deletes,
//!    `q` quits).
//! 3. `enabled` matches disabled sources (a substring of `disabled`).
//! 4. Refreshing the first Activity page after new events arrived drops
//!    the pages loaded below it, duplicates events, or moves the selection
//!    to a different event.
//! 5. Loading the next page after new events arrived duplicates the events
//!    that shifted across the page boundary.
//! 6. The Activity search reaches the server with the wrong parameters
//!    (the API wants `severity=Error`, not `error`), or a response for the
//!    previous search lands after the search changed.
//!
//! Each scenario ends with the mock server's request log or the action the
//! app would send, not just the app's internal state.

// `allow-unwrap-in-tests` covers `#[test]` fns, not the shared helpers below.
#![allow(clippy::unwrap_used)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hof_tui::app::{ACTIVITY_PAGE, Action, App, Knob, Paging, Popup, Tab};
use hof_tui::client::ApiClient;
use hof_tui::search::ActivityFilter;
use hof_tui::types::{
    ActivitySeverity, ProfileResponse, SettingsResponse, SourceResponse, VideoResponse,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(app: &mut App, code: KeyCode) -> Option<Action> {
    app.handle_key(key(code))
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        assert_eq!(
            press(app, KeyCode::Char(c)),
            None,
            "typing {c:?} fired an action"
        );
    }
}

// ----------------------------------------------------------------------
// Activity: a mock `GET /api/v1/activity` over a mutable event log
// ----------------------------------------------------------------------

/// Event `n`: every tenth is an error mentioning a timeout.
fn event(n: usize) -> Value {
    let (severity, message) = if n.is_multiple_of(10) {
        ("Error", format!("Index timeout on source {n}"))
    } else {
        ("Info", format!("Indexed source {n}"))
    };
    json!({
        "id": format!("e{n:04}"),
        "event_type": "SourceIndexed",
        "severity": severity,
        "message": message,
        "created_at": "2026-10-07T12:00:00Z",
    })
}

/// Pages the shared log newest first, applying `severity` and `search` the
/// way the server does, and records each request's query string.
#[derive(Clone)]
struct ActivityLog {
    /// Event numbers, oldest first.
    events: Arc<Mutex<Vec<usize>>>,
    queries: Arc<Mutex<Vec<String>>>,
}

impl ActivityLog {
    fn with(count: usize) -> Self {
        Self {
            events: Arc::new(Mutex::new((0..count).collect())),
            queries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn push_new(&self, count: usize) {
        let mut events = self.events.lock().unwrap();
        let next = events.len();
        events.extend(next..next + count);
    }
}

impl Respond for ActivityLog {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let param = |name: &str| {
            request
                .url
                .query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        self.queries
            .lock()
            .unwrap()
            .push(request.url.query().unwrap_or_default().to_string());

        let limit: usize = param("limit").unwrap().parse().unwrap();
        let offset: usize = param("offset").unwrap_or_default().parse().unwrap_or(0);
        let matching: Vec<Value> = self
            .events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .map(|&n| event(n))
            .filter(|e| param("severity").is_none_or(|s| e["severity"] == s.as_str()))
            .filter(|e| {
                param("search").is_none_or(|s| {
                    e["message"]
                        .as_str()
                        .unwrap()
                        .to_lowercase()
                        .contains(&s.to_lowercase())
                })
            })
            .collect();
        let page: Vec<&Value> = matching.iter().skip(offset).take(limit).collect();
        ResponseTemplate::new(200).set_body_json(json!({
            "events": page,
            "total": matching.len(),
            "limit": limit,
            "offset": offset,
        }))
    }
}

async fn fetch_first_page(client: &ApiClient, app: &mut App) -> bool {
    let filter = app.activity_filter.clone();
    let resp = client
        .list_activity(ACTIVITY_PAGE, 0, &filter)
        .await
        .unwrap();
    app.set_activity(&filter, resp)
}

/// Execute a `LoadMoreActivity` the way the event loop does.
async fn fetch_next_page(client: &ApiClient, app: &mut App) {
    let filter = app.activity_filter.clone();
    let resp = client
        .list_activity(ACTIVITY_PAGE, app.activity.len(), &filter)
        .await
        .unwrap();
    app.append_activity(&filter, resp);
}

fn ids(app: &App) -> Vec<String> {
    app.activity.iter().map(|e| e.id.clone()).collect()
}

fn selected_id(app: &App) -> String {
    let i = app.activity_state.selected().unwrap();
    app.activity.get(i).unwrap().id.clone()
}

/// Loaded events are unique and strictly newest first (ids sort by age).
fn assert_ordered_unique(app: &App) {
    let ids = ids(app);
    let unique: HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "duplicate events loaded");
    assert!(
        ids.iter().zip(ids.iter().skip(1)).all(|(a, b)| a > b),
        "events out of order: {ids:?}"
    );
}

#[tokio::test]
async fn activity_pages_survive_new_events_and_search_runs_on_the_server() {
    let server = MockServer::start().await;
    let log = ActivityLog::with(120);
    Mock::given(method("GET"))
        .and(path("/api/v1/activity"))
        .respond_with(log.clone())
        .mount(&server)
        .await;
    let client = ApiClient::new(&server.uri(), "hof_sk_test").unwrap();
    let mut app = App {
        tab: Tab::Activity,
        ..App::default()
    };

    // Opening the tab loads one page, not everything.
    assert!(fetch_first_page(&client, &mut app).await);
    assert_eq!(app.activity.len(), 50);
    assert_eq!(app.activity_total, 120);

    // Reaching the bottom asks for the next page exactly once.
    assert_eq!(
        press(&mut app, KeyCode::End),
        Some(Action::LoadMoreActivity)
    );
    assert_eq!(press(&mut app, KeyCode::Char('j')), None);
    fetch_next_page(&client, &mut app).await;
    assert_eq!(app.activity.len(), 100);
    assert_eq!(app.activity_paging, Paging::Idle);

    // Park the selection on event 60 rows down, then three events arrive.
    press(&mut app, KeyCode::Home);
    for _ in 0..60 {
        press(&mut app, KeyCode::Down);
    }
    let parked = selected_id(&app);
    log.push_new(3);

    // The periodic refresh refetches only the first page; the second page
    // stays and the selection follows its event.
    assert!(fetch_first_page(&client, &mut app).await);
    assert_eq!(app.activity.len(), 103);
    assert_eq!(app.activity_total, 123);
    assert_eq!(selected_id(&app), parked);
    assert_ordered_unique(&app);

    // Two more arrive before the next page is fetched: its offset is now
    // two events off, and the overlap must not show up twice.
    log.push_new(2);
    assert_eq!(
        press(&mut app, KeyCode::End),
        Some(Action::LoadMoreActivity)
    );
    fetch_next_page(&client, &mut app).await;
    assert_ordered_unique(&app);
    assert_eq!(ids(&app).len(), 123);
    assert_eq!(ids(&app).last().unwrap(), "e0000");
    // The oldest event is loaded. `total` (125) still counts the two new
    // ones above, but those come with the next refresh, not another page:
    // `j` at the bottom wraps instead of refetching the end forever.
    assert!(!app.activity_has_more());
    assert_eq!(press(&mut app, KeyCode::End), None);
    assert_eq!(press(&mut app, KeyCode::Char('j')), None);
    assert_eq!(app.activity_state.selected(), Some(0));
    assert!(fetch_first_page(&client, &mut app).await);
    assert_eq!(app.activity.len(), 125);
    assert_ordered_unique(&app);

    // Search: severity word plus message text, sent on Enter only.
    assert_eq!(press(&mut app, KeyCode::Char('/')), None);
    type_text(&mut app, "error timeout");
    assert!(!app.activity.is_empty(), "rows cleared before Enter");
    assert_eq!(press(&mut app, KeyCode::Enter), Some(Action::Refresh));
    assert!(app.activity.is_empty());

    // A response for the old search arriving now is discarded.
    let stale = client
        .list_activity(ACTIVITY_PAGE, 0, &ActivityFilter::default())
        .await
        .unwrap();
    assert!(!app.set_activity(&ActivityFilter::default(), stale));
    assert!(app.activity.is_empty());

    assert!(fetch_first_page(&client, &mut app).await);
    // Events 0, 10, ..., 120 of the 125 now in the log.
    assert_eq!(app.activity_total, 13);
    assert_eq!(app.activity.len(), 13);
    assert!(
        app.activity
            .iter()
            .all(|e| e.severity == Some(ActivitySeverity::Error))
    );

    // Esc drops the search and refetches everything.
    assert_eq!(press(&mut app, KeyCode::Esc), Some(Action::Refresh));
    assert!(app.activity_filter.is_empty());
    assert!(fetch_first_page(&client, &mut app).await);
    assert_eq!(app.activity_total, 125);

    let queries = log.queries.lock().unwrap().clone();
    assert!(
        queries
            .iter()
            .any(|q| q == "limit=50&offset=0&severity=Error&search=timeout"),
        "search not sent as expected: {queries:#?}"
    );
    assert!(
        queries.iter().all(|q| q.starts_with("limit=50&")),
        "a request asked for more than one page: {queries:#?}"
    );
}

// ----------------------------------------------------------------------
// Client-side search on the other tabs
// ----------------------------------------------------------------------

fn profiles() -> Vec<ProfileResponse> {
    serde_json::from_value(json!([
        { "id": "p-sci", "name": "Science" },
        { "id": "p-doc", "name": "Documentaries" },
    ]))
    .unwrap()
}

fn sources() -> Vec<SourceResponse> {
    serde_json::from_value(json!([
        { "id": "s-kurz", "profile_id": "p-sci", "url": "https://youtube.com/@kurzgesagt",
          "custom_name": "Kurzgesagt", "enabled": true },
        { "id": "s-dw", "profile_id": "p-doc", "url": "https://youtube.com/@DWDocumentary",
          "custom_name": "DW Documentary", "enabled": false },
        { "id": "s-veri", "profile_id": "p-sci", "url": "https://youtube.com/@veritasium",
          "enabled": false },
    ]))
    .unwrap()
}

fn downloads() -> Vec<VideoResponse> {
    serde_json::from_value(json!([
        { "id": "v1", "title": "The Egg", "status": "Completed",
          "source_display_name": "Kurzgesagt" },
        { "id": "v2", "title": "Why Germany builds trains", "status": "Failed",
          "source_display_name": "DW Documentary" },
        { "id": "v3", "title": "The Most Misunderstood Concept in Physics", "status": "Failed",
          "source_display_name": "Veritasium" },
    ]))
    .unwrap()
}

fn settings() -> SettingsResponse {
    let v = |value: u64| json!({ "value": value, "provenance": "default" });
    serde_json::from_value(json!({
        "pause": {
            "indexing": { "paused": false },
            "downloads": { "paused": true, "indefinite": true },
        },
        "max_concurrent_downloads": v(3),
        "max_indexers_per_tick": v(2),
        "rate_limit_delay_secs": v(5),
        "check_interval_secs": v(300),
        "cleanup_interval_secs": v(3600),
        "drain_timeout_secs": v(30),
    }))
    .unwrap()
}

#[test]
fn search_narrows_each_tab_and_row_actions_hit_the_matched_row() {
    let mut app = App::default();
    app.set_profiles(profiles());
    app.set_sources(sources());
    app.set_downloads(downloads());
    app.set_settings(settings());

    // Sources: `dis` (typed with keys bound to delete/index) matches only
    // disabled sources; adding the profile name narrows to one.
    app.tab = Tab::Sources;
    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "dis");
    assert_eq!(app.visible(Tab::Sources), vec![1, 2]);
    type_text(&mut app, " sci");
    assert_eq!(app.visible(Tab::Sources), vec![2]);
    assert_eq!(press(&mut app, KeyCode::Enter), None);

    // `d` now confirms deleting Veritasium, not the second source overall.
    assert_eq!(press(&mut app, KeyCode::Char('d')), None);
    assert_eq!(
        press(&mut app, KeyCode::Char('y')),
        Some(Action::DeleteSource("s-veri".to_string()))
    );

    // `enabled` is not a match for disabled sources.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "enabled");
    assert_eq!(app.visible(Tab::Sources), vec![0]);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.visible(Tab::Sources), vec![0, 1, 2]);

    // Downloads: by source name; `q` in the prompt is text, not quit.
    app.tab = Tab::Downloads;
    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "q");
    assert!(!app.should_quit);
    press(&mut app, KeyCode::Backspace);
    type_text(&mut app, "veritas");
    assert_eq!(app.visible(Tab::Downloads), vec![2]);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        press(&mut app, KeyCode::Char('r')),
        Some(Action::RetryDownload("v3".to_string()))
    );
    // Queries are per tab: Sources is unfiltered after its Esc.
    assert_eq!(app.query(Tab::Downloads), "veritas");
    assert_eq!(app.query(Tab::Sources), "");

    // Profiles: by name.
    app.tab = Tab::Profiles;
    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "DOC");
    assert_eq!(app.visible(Tab::Profiles), vec![1]);
    press(&mut app, KeyCode::Enter);

    // Settings: by value as displayed (300 s shows as `5m`) and by pause
    // state; Enter edits the matched knob.
    app.tab = Tab::Settings;
    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "5m");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    match &app.popup {
        Some(Popup::Edit(edit)) => assert_eq!(edit.knob, Knob::CheckInterval),
        other => panic!("expected the check-interval editor, got {other:?}"),
    }
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('u'));
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    type_text(&mut app, "indefinite");
    assert_eq!(app.visible(Tab::Settings), vec![1]);
}
