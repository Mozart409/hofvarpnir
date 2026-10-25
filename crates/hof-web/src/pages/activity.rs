//! Split out of `pages/mod.rs`.

use super::*;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ActivityQuery {
    pub(super) severity: Option<String>,
    pub(super) search: Option<String>,
    /// Restrict to one source; set by clicking a source pill on an event row.
    pub(super) source: Option<String>,
    pub(super) page: Option<i64>,
    pub(super) per_page: Option<i64>,
}

/// Request-scoped activity filter state, normalized once.
///
/// The page, list partial and SSE stream all need the same derived values;
/// deriving them in one place keeps the three views from drifting apart.
pub(super) struct ActivityParams {
    pub(super) page_num: i64,
    pub(super) per_page: i64,
    pub(super) offset: i64,
    pub(super) severity_filter: Option<ActivitySeverity>,
    pub(super) severity_label: String,
    pub(super) search: Option<String>,
    pub(super) source_id: Option<Ulid>,
    pub(super) source: Option<String>,
}

impl ActivityParams {
    pub(super) fn from_query(query: &ActivityQuery) -> Self {
        let page_num = query.page.unwrap_or(1).max(1);
        let per_page = parse_activity_page_size(query.per_page);
        let search = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        // Only keep a source filter we can actually parse; a malformed id would
        // otherwise silently match nothing while still showing an active pill.
        let source_id = query
            .source
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|value| Ulid::from_string(value).ok());

        Self {
            page_num,
            per_page,
            offset: (page_num - 1) * per_page,
            severity_filter: parse_activity_severity(query.severity.as_deref()),
            severity_label: normalized_activity_severity(query.severity.as_deref()).to_owned(),
            search,
            source_id,
            source: source_id.map(|id| id.to_string()),
        }
    }

    pub(super) fn severity_param(&self) -> Option<&str> {
        if self.severity_label == "all" {
            None
        } else {
            Some(&self.severity_label)
        }
    }

    pub(super) fn filter(&self) -> ActivityFilter<'_> {
        ActivityFilter {
            severity: self.severity_param(),
            search: self.search.as_deref(),
            source: self.source.as_deref(),
        }
    }
}

pub(super) async fn activity_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> impl IntoResponse {
    let params = ActivityParams::from_query(&query);
    let ActivityParams {
        page_num,
        per_page,
        offset,
        ref severity_filter,
        ..
    } = params;

    let (events_result, count_result) = tokio::join!(
        db::list_activity_events(
            &state.pool,
            per_page,
            offset,
            severity_filter.clone(),
            None,
            params.source_id,
            params.search.as_deref(),
        ),
        db::count_activity_events(
            &state.pool,
            severity_filter.clone(),
            None,
            params.source_id,
            params.search.as_deref(),
        )
    );

    let events = match events_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load activity events");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load activity log"),
            );
        }
    };

    let source_names = load_activity_source_names(&state.pool).await;
    let video_source_names = db::get_source_names_for_videos(&state.pool)
        .await
        .unwrap_or_default();

    let total = count_result.unwrap_or(0);
    let total_pages = (total + per_page - 1) / per_page;

    let current_severity = params.severity_label.as_str();
    let list_url = activity_list_url(params.filter(), page_num, per_page);
    let events_url = activity_events_url(params.filter(), page_num, per_page);

    let page = layout(
        "Activity",
        NavItem::Activity,
        html! {
            section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Activity Log" }
                div hx-ext="sse" sse-connect=(events_url) {
                    div
                        id="activity-content"
                        sse-swap="activity-update"
                        // `show:none` keeps background SSE refreshes from scrolling the
                        // viewport out from under someone reading mid-list.
                        hx-swap="innerHTML show:none"
                        hx-get=(list_url)
                        hx-trigger="load"
                        hx-target="this"
                    {
                        (activity_content_markup(
                            &events,
                            &source_names,
                            &video_source_names,
                            page_num,
                            total_pages,
                            current_severity,
                            per_page,
                            params.search.as_deref(),
                            params.source.as_deref(),
                        ))
                    }
                }
            }
        },
    );

    (StatusCode::OK, page)
}

pub(super) async fn activity_list_partial(
    _auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> impl IntoResponse {
    let params = ActivityParams::from_query(&query);
    let ActivityParams {
        page_num,
        per_page,
        offset,
        ref severity_filter,
        ..
    } = params;

    let (events_result, count_result) = tokio::join!(
        db::list_activity_events(
            &state.pool,
            per_page,
            offset,
            severity_filter.clone(),
            None,
            params.source_id,
            params.search.as_deref(),
        ),
        db::count_activity_events(
            &state.pool,
            severity_filter.clone(),
            None,
            params.source_id,
            params.search.as_deref(),
        )
    );

    let events = match events_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load activity partial");
            return error_fragment("Failed to load activity log").into_response();
        }
    };

    let source_names = load_activity_source_names(&state.pool).await;
    let video_source_names = db::get_source_names_for_videos(&state.pool)
        .await
        .unwrap_or_default();

    let total = count_result.unwrap_or(0);
    let total_pages = if total == 0 {
        1
    } else {
        (total + per_page - 1) / per_page
    };
    activity_content_markup(
        &events,
        &source_names,
        &video_source_names,
        page_num,
        total_pages,
        &params.severity_label,
        per_page,
        params.search.as_deref(),
        params.source.as_deref(),
    )
    .into_response()
}

/// Load a `source_id -> display_name` lookup map for enriching activity rows.
///
/// Reuses `db::list_sources` rather than adding a new query, so this stays
/// offline-cache-free. Returns an empty map on error so a DB hiccup only
/// degrades the display (no source name pill) instead of failing the page.
pub(super) async fn load_activity_source_names(pool: &sqlx::PgPool) -> HashMap<Ulid, String> {
    match db::list_sources(pool).await {
        Ok(sources) => sources
            .iter()
            .map(|s| (s.id, s.display_name().to_string()))
            .collect(),
        Err(error) => {
            tracing::warn!(%error, "failed to load source names for activity log");
            HashMap::new()
        }
    }
}

pub(super) fn severity_filter_link(
    value: &str,
    label: &str,
    current: &str,
    per_page: i64,
    search: Option<&str>,
    source: Option<&str>,
) -> Markup {
    let active = value == current;
    let classes = if active {
        "rounded-full bg-slate-900 dark:bg-slate-100 px-3 py-1 text-xs font-medium text-white dark:text-slate-900"
    } else {
        "rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
    };
    let severity = if value == "all" { None } else { Some(value) };
    let filter = ActivityFilter {
        severity,
        search,
        source,
    };
    let href = activity_page_url(filter, 1, per_page);
    let hx_get = activity_list_url(filter, 1, per_page);

    html! {
        a
            class=(classes)
            href=(href)
            hx-get=(hx_get)
            hx-target="#activity-content"
            hx-push-url=(href)
        {
            (label)
        }
    }
}

pub(super) fn activity_page_size_link(
    size: i64,
    current_size: i64,
    severity: Option<&str>,
    search: Option<&str>,
    source: Option<&str>,
) -> Markup {
    let active = size == current_size;
    let classes = if active {
        "rounded-full bg-slate-900 dark:bg-slate-100 px-2.5 py-1 text-xs font-medium text-white dark:text-slate-900"
    } else {
        "rounded-full bg-slate-100 dark:bg-slate-700 px-2.5 py-1 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
    };
    let filter = ActivityFilter {
        severity,
        search,
        source,
    };
    let href = activity_page_url(filter, 1, size);
    let hx_get = activity_list_url(filter, 1, size);

    html! {
        a
            class=(classes)
            href=(href)
            hx-get=(hx_get)
            hx-target="#activity-content"
            hx-push-url=(href)
        {
            (size)
        }
    }
}

pub(super) fn page_size_option(size: i64, current_size: i64) -> Markup {
    if size == current_size {
        html! {
            option value=(size) selected { (size) " / page" }
        }
    } else {
        html! {
            option value=(size) { (size) " / page" }
        }
    }
}

pub(super) const fn parse_activity_page_size(value: Option<i64>) -> i64 {
    match value {
        Some(size) if matches!(size, 25 | 50 | 100) => size,
        _ => 50,
    }
}

pub(super) fn parse_activity_severity(value: Option<&str>) -> Option<ActivitySeverity> {
    value.and_then(|severity| match severity {
        "info" => Some(ActivitySeverity::Info),
        "success" => Some(ActivitySeverity::Success),
        "warning" => Some(ActivitySeverity::Warning),
        "error" => Some(ActivitySeverity::Error),
        _ => None,
    })
}

pub(super) fn normalized_activity_severity(value: Option<&str>) -> &str {
    match value {
        Some(severity @ ("info" | "success" | "warning" | "error")) => severity,
        _ => "all",
    }
}

/// The filter state shared by every activity URL variant.
///
/// Grouped into a struct because these four values must travel together through
/// the page, the list partial, the SSE stream and every filter link — passing
/// them positionally invited dropping one and silently resetting the view.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ActivityFilter<'a> {
    pub(super) severity: Option<&'a str>,
    pub(super) search: Option<&'a str>,
    pub(super) source: Option<&'a str>,
}

pub(super) fn activity_page_url(filter: ActivityFilter<'_>, page: i64, per_page: i64) -> String {
    activity_url("/activity", filter, page, per_page)
}

pub(super) fn activity_list_url(filter: ActivityFilter<'_>, page: i64, per_page: i64) -> String {
    activity_url("/web/activity/list", filter, page, per_page)
}

pub(super) fn activity_events_url(filter: ActivityFilter<'_>, page: i64, per_page: i64) -> String {
    activity_url("/web/activity/events", filter, page, per_page)
}

pub(super) fn activity_url(
    base: &str,
    filter: ActivityFilter<'_>,
    page: i64,
    per_page: i64,
) -> String {
    let mut params = Vec::new();

    if let Some(value) = filter.severity.filter(|value| *value != "all") {
        params.push(format!("severity={value}"));
    }
    if let Some(value) = filter.search.filter(|value| !value.is_empty()) {
        params.push(format!("search={}", encode_query_value(value)));
    }
    if let Some(value) = filter.source.filter(|value| !value.is_empty()) {
        params.push(format!("source={}", encode_query_value(value)));
    }
    if page > 1 {
        params.push(format!("page={page}"));
    }
    if per_page != 50 {
        params.push(format!("per_page={per_page}"));
    }

    if params.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", params.join("&"))
    }
}

/// Severity pills, page-size pills, and the message search box.
pub(super) fn activity_filter_bar(
    filter: ActivityFilter<'_>,
    current_severity: &str,
    page_num: i64,
    per_page: i64,
) -> Markup {
    let ActivityFilter {
        severity,
        search,
        source,
    } = filter;

    html! {
        div class="mt-3 flex flex-wrap items-center gap-3" {
            nav class="flex gap-1" {
                (severity_filter_link("all", "All", current_severity, per_page, search, source))
                (severity_filter_link("info", "Info", current_severity, per_page, search, source))
                (severity_filter_link("success", "Success", current_severity, per_page, search, source))
                (severity_filter_link("warning", "Warning", current_severity, per_page, search, source))
                (severity_filter_link("error", "Error", current_severity, per_page, search, source))
            }
            nav class="flex items-center gap-1" {
                span class="px-2 text-xs font-medium text-slate-500 dark:text-slate-400" { "Rows" }
                (activity_page_size_link(25, per_page, severity, search, source))
                (activity_page_size_link(50, per_page, severity, search, source))
                (activity_page_size_link(100, per_page, severity, search, source))
            }
            form
                method="get"
                action="/activity"
                class="flex gap-2"
                hx-get="/web/activity/list"
                hx-target="#activity-content"
                hx-push-url=(activity_page_url(filter, page_num, per_page))
            {
                // Severity and source live outside this form as pills, so they
                // ride along as hidden fields to survive a search submit.
                @if let Some(value) = severity {
                    input type="hidden" name="severity" value=(value);
                }
                @if let Some(value) = source {
                    input type="hidden" name="source" value=(value);
                }
                input type="hidden" name="per_page" value=(per_page);
                input id="activity-page-field" type="hidden" name="page" value=(page_num);
                input
                    type="text"
                    name="search"
                    placeholder="Search messages..."
                    value=(search.unwrap_or(""))
                    data-reset-page="activity-page-field"
                    class="rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-900 dark:text-slate-100";
                button type="submit"
                    class="rounded-lg bg-slate-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-slate-700"
                { "Search" }
            }
        }
    }
}

/// The active source filter, rendered as a pill with a clear affordance.
///
/// `display_name` is `None` for a source that has since been deleted; the raw
/// id is shown instead so the filter is still identifiable and clearable.
pub(super) fn activity_source_pill(
    filter: ActivityFilter<'_>,
    display_name: Option<&str>,
    per_page: i64,
) -> Markup {
    let Some(source_id) = filter.source else {
        return html! {};
    };
    let cleared = ActivityFilter {
        source: None,
        ..filter
    };

    html! {
        div class="mt-3 flex flex-wrap items-center gap-2" {
            span class="text-xs font-medium text-slate-500 dark:text-slate-400" { "Filtered to source" }
            span class="inline-flex items-center gap-2 rounded-full bg-slate-900 dark:bg-slate-100 px-3 py-1 text-xs font-medium text-white dark:text-slate-900" {
                (display_name.unwrap_or(source_id))
                a
                    href=(activity_page_url(cleared, 1, per_page))
                    hx-get=(activity_list_url(cleared, 1, per_page))
                    hx-target="#activity-content"
                    hx-push-url=(activity_page_url(cleared, 1, per_page))
                    class="text-slate-300 dark:text-slate-600 hover:text-white dark:hover:text-slate-900"
                    aria-label="Clear source filter"
                { "\u{00d7}" }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn activity_content_markup(
    events: &[hof_core::domain::activity::ActivityEvent],
    source_names: &HashMap<Ulid, String>,
    video_source_names: &HashMap<Ulid, String>,
    page_num: i64,
    total_pages: i64,
    current_severity: &str,
    per_page: i64,
    search: Option<&str>,
    source: Option<&str>,
) -> Markup {
    let severity_param = if current_severity == "all" {
        None
    } else {
        Some(current_severity)
    };
    let filter = ActivityFilter {
        severity: severity_param,
        search,
        source,
    };
    let has_filters = search.is_some() || source.is_some() || severity_param.is_some();
    // The pill shows the source's display name when we can resolve it, falling
    // back to the raw id for a source that has since been deleted.
    let active_source_name = source
        .and_then(|id| Ulid::from_string(id).ok())
        .and_then(|id| source_names.get(&id))
        .map(String::as_str);

    html! {
        (activity_filter_bar(filter, current_severity, page_num, per_page))
        (activity_source_pill(filter, active_source_name, per_page))

        @if events.is_empty() {
            p class="mt-4 rounded-lg border border-dashed border-slate-300 dark:border-slate-600 bg-slate-50 dark:bg-slate-800 px-4 py-8 text-center text-sm text-slate-500 dark:text-slate-400" {
                @if has_filters {
                    "No activity events match your filters."
                } @else {
                    "No activity events recorded yet."
                }
            }
        } @else {
            div class="mt-4 space-y-2" {
                @for event in events {
                    (activity_event_row(event, source_names, video_source_names, filter, per_page))
                }
            }

            @if total_pages > 1 {
                nav class="mt-6 flex items-center justify-center gap-2" {
                    @if page_num > 1 {
                        a
                            class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                            href=(activity_page_url(filter, page_num - 1, per_page))
                            hx-get=(activity_list_url(filter, page_num - 1, per_page))
                            hx-target="#activity-content"
                            hx-push-url=(activity_page_url(filter, page_num - 1, per_page))
                        {
                            "Previous"
                        }
                    }
                    span class="text-sm text-slate-500 dark:text-slate-400" {
                        (format!("Page {} of {}", page_num, total_pages))
                    }
                    @if page_num < total_pages {
                        a
                            class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                            href=(activity_page_url(filter, page_num + 1, per_page))
                            hx-get=(activity_list_url(filter, page_num + 1, per_page))
                            hx-target="#activity-content"
                            hx-push-url=(activity_page_url(filter, page_num + 1, per_page))
                        {
                            "Next"
                        }
                    }
                }
            }
        }
    }
}

/// Render a single activity log entry.
///
/// `source_names` maps `source_id -> display_name` (from `list_sources`), used
/// directly for source-level events (e.g. `SourceIndexed`). Download events
/// don't carry a `source_id` on the row, so `video_source_names` (a
/// `video_id -> display_name` map derived by joining through `source_videos`,
/// see `db::get_source_names_for_videos`) is used as a fallback so the source
/// pill still resolves via the event's `video_id`. If neither resolves (e.g.
/// the source was since deleted), the pill is simply omitted.
/// Human-facing label for an activity event type.
///
/// Extracted from `activity_event_row` rather than inlined: the row renderer
/// sits right on the `too_many_lines` limit, and a pure enum-to-label map is
/// the obvious thing to lift out when a new variant is added.
pub(super) const fn activity_event_label(event_type: &ActivityEventType) -> &'static str {
    match event_type {
        ActivityEventType::SourceIndexed => "Source Indexed",
        ActivityEventType::SourceError => "Source Error",
        ActivityEventType::DownloadStarted => "Download Started",
        ActivityEventType::DownloadCompleted => "Download Completed",
        ActivityEventType::DownloadFailed => "Download Failed",
        ActivityEventType::RetryScheduled => "Retry Scheduled",
        ActivityEventType::MetadataGenerated => "Metadata Generated",
        ActivityEventType::VideoCleaned => "Video Cleaned",
        ActivityEventType::ProfileCreated => "Profile Created",
        ActivityEventType::ProfileUpdated => "Profile Updated",
        ActivityEventType::ProfileDeleted => "Profile Deleted",
        ActivityEventType::SourceCreated => "Source Created",
        ActivityEventType::SourceUpdated => "Source Updated",
        ActivityEventType::SourceDeleted => "Source Deleted",
        ActivityEventType::SelfRestart => "Self Restart",
    }
}

pub(super) fn activity_event_row(
    event: &hof_core::domain::activity::ActivityEvent,
    source_names: &HashMap<Ulid, String>,
    video_source_names: &HashMap<Ulid, String>,
    filter: ActivityFilter<'_>,
    per_page: i64,
) -> Markup {
    let (icon, border_color) = match event.severity {
        ActivitySeverity::Info => ("i", "border-l-sky-400"),
        ActivitySeverity::Success => ("✓", "border-l-emerald-400"),
        ActivitySeverity::Warning => ("!", "border-l-amber-400"),
        ActivitySeverity::Error => ("✗", "border-l-rose-400"),
    };

    let severity_badge = match event.severity {
        ActivitySeverity::Info => (
            "Info",
            "bg-sky-100 dark:bg-sky-900/50 text-sky-800 dark:text-sky-200",
        ),
        ActivitySeverity::Success => (
            "Success",
            "bg-emerald-100 dark:bg-emerald-900/50 text-emerald-800 dark:text-emerald-200",
        ),
        ActivitySeverity::Warning => (
            "Warning",
            "bg-amber-100 dark:bg-amber-900/50 text-amber-800 dark:text-amber-200",
        ),
        ActivitySeverity::Error => (
            "Error",
            "bg-rose-100 dark:bg-rose-900/50 text-rose-800 dark:text-rose-200",
        ),
    };

    let event_label = activity_event_label(&event.event_type);

    let time_ago = format_time_ago(event.created_at);
    let source_indexing = event.source_indexing_summary();
    let source_name = event
        .source_id
        .and_then(|id| source_names.get(&id))
        .or_else(|| event.video_id.and_then(|id| video_source_names.get(&id)));
    // Only events carrying a source id can be turned into a filter link; names
    // resolved via `video_source_names` have no id to filter on.
    let pill_source_id = event.source_id.filter(|id| source_names.contains_key(id));

    html! {
        div id=(format!("activity-{}", event.id)) class=(format!("flex items-start gap-3 rounded-lg border border-slate-200 dark:border-slate-700 border-l-4 {} bg-white dark:bg-slate-800 p-3", border_color)) {
            span class="mt-0.5 flex h-6 w-6 items-center justify-center rounded-full bg-slate-100 dark:bg-slate-700 text-xs font-bold text-slate-600 dark:text-slate-300" {
                (icon)
            }
            div class="min-w-0 flex-1" {
                div class="flex flex-wrap items-center gap-2" {
                    span class=(format!("inline-flex rounded-full px-2 py-0.5 text-xs font-medium {}", severity_badge.1)) {
                        (severity_badge.0)
                    }
                    span class="text-xs font-medium text-slate-500 dark:text-slate-400" { (event_label) }
                    @if let Some(name) = source_name {
                        @if let Some(source_id) = pill_source_id {
                            @let pill_filter = ActivityFilter {
                                severity: filter.severity,
                                search: filter.search,
                                source: Some(&source_id.to_string()),
                            };
                            a
                                href=(activity_page_url(pill_filter, 1, per_page))
                                hx-get=(activity_list_url(pill_filter, 1, per_page))
                                hx-target="#activity-content"
                                hx-push-url=(activity_page_url(pill_filter, 1, per_page))
                                title=(format!("Show only activity from {name}"))
                                class="inline-flex rounded-full bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                            {
                                (name)
                            }
                        } @else {
                            span class="inline-flex rounded-full bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs font-medium text-slate-600 dark:text-slate-300" {
                                (name)
                            }
                        }
                    }
                    span class="text-xs text-slate-400 dark:text-slate-500" title=(event.created_at.to_rfc3339()) { (time_ago) }
                }
                p class="mt-1 text-sm text-slate-700 dark:text-slate-300 break-words wrap-anywhere" { (event.message) }
                @if let Some(summary) = source_indexing {
                    div class="mt-2 flex flex-wrap gap-1.5" {
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "new: " (summary.new_videos) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "existing: " (summary.existing_videos) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "filtered: " (summary.filtered_total) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "cutoff: " (summary.filtered_before_cutoff) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "shorts: " (summary.filtered_shorts) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "live: " (summary.filtered_livestreams) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "unavailable: " (summary.filtered_unavailable) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "private: " (summary.filtered_private) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "other: " (summary.filtered_other) }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod activity_event_row_tests {
    use super::{
        ActivityEventType, ActivityFilter, ActivitySeverity, HashMap, Ulid, activity_event_row,
    };
    use hof_core::domain::activity::ActivityEvent;

    fn base_event(event_type: ActivityEventType, video_id: Option<Ulid>) -> ActivityEvent {
        ActivityEvent {
            id: Ulid::from_string("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid literal"),
            event_type,
            severity: ActivitySeverity::Success,
            message: "Completed \"300G.mp4\" (13.0 MB)".to_string(),
            source_id: None,
            video_id,
            profile_id: None,
            created_at: chrono::Utc::now(),
        }
    }

    /// Default (unfiltered) view state for rendering a row under test.
    fn filter() -> ActivityFilter<'static> {
        ActivityFilter::default()
    }

    /// Download activity rows never carry a `source_id` (it's only set for
    /// source-level events like `SourceIndexed`). The pill should still
    /// resolve, via the event's `video_id`, using the `video_id ->
    /// display_name` map that `db::get_source_names_for_videos` provides.
    #[test]
    fn download_completed_event_resolves_source_pill_via_video_id() {
        let video_id = Ulid::from_string("01BX5ZZKBKACTAV9WEVGEMMVRZ").expect("valid ulid literal");
        let event = base_event(ActivityEventType::DownloadCompleted, Some(video_id));
        assert!(
            event.source_id.is_none(),
            "precondition: no source_id on row"
        );

        let source_names: HashMap<Ulid, String> = HashMap::new();
        let mut video_source_names: HashMap<Ulid, String> = HashMap::new();
        video_source_names.insert(video_id, "Kobosil 300G".to_string());

        let markup = activity_event_row(&event, &source_names, &video_source_names, filter(), 50)
            .into_string();

        assert!(
            markup.contains("Kobosil 300G"),
            "expected source pill text in rendered markup: {markup}"
        );
    }

    /// When the source can't be resolved at all (e.g. it was deleted, or the
    /// event has no video association), the pill must be omitted rather than
    /// rendered empty.
    #[test]
    fn download_event_without_resolvable_source_omits_pill() {
        let video_id = Ulid::from_string("01BX5ZZKBKACTAV9WEVGEMMVRZ").expect("valid ulid literal");
        let event = base_event(ActivityEventType::DownloadCompleted, Some(video_id));

        let source_names: HashMap<Ulid, String> = HashMap::new();
        let video_source_names: HashMap<Ulid, String> = HashMap::new();

        let markup = activity_event_row(&event, &source_names, &video_source_names, filter(), 50)
            .into_string();

        assert!(
            !markup.contains("inline-flex rounded-full bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs font-medium text-slate-600 dark:text-slate-300"),
            "did not expect a source pill span when the source cannot be resolved: {markup}"
        );
    }

    /// Download failure messages embed unbroken tokens (googlevideo URLs with
    /// long query strings). Without wrapping classes they overflow the card
    /// and stretch the page horizontally, so the message paragraph must keep
    /// both `break-words` and `wrap-anywhere`.
    #[test]
    fn long_error_message_paragraph_carries_wrapping_classes() {
        let mut event = base_event(ActivityEventType::DownloadFailed, None);
        event.severity = ActivitySeverity::Error;
        event.message = "[DOWNLOAD_FORMAT_UNAVAILABLE] Permanently failed after 2 attempts — \
             Download 128 failed: Unexpected HTTP status 401 for \
             https://rr2---sn-hoxu-h0jz.googlevideo.com/videoplayback?expire=1785506552&itag=278&source=youtube"
            .to_string();

        let source_names: HashMap<Ulid, String> = HashMap::new();
        let video_source_names: HashMap<Ulid, String> = HashMap::new();

        let markup = activity_event_row(&event, &source_names, &video_source_names, filter(), 50)
            .into_string();

        assert!(
            markup.contains("break-words wrap-anywhere"),
            "expected wrapping classes on the message paragraph: {markup}"
        );
    }
}
