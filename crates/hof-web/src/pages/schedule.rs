//! Split out of `pages/mod.rs`.

use super::*;

pub(super) async fn trigger_cleanup(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
) -> impl IntoResponse {
    match state.cleanup.ask(RunCleanup).await {
        Ok(result) => {
            tracing::info!(
                retention = result.retention_cleaned,
                quota = result.quota_cleaned,
                bytes_freed = result.bytes_freed,
                "Manual cleanup triggered from web UI"
            );
            let total = result.retention_cleaned + result.quota_cleaned;
            set_flash(
                &session,
                "success",
                &format!("Cleanup done: {total} files cleaned"),
            )
            .await;
            Redirect::to("/schedule").into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to trigger cleanup from web UI");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to trigger cleanup"),
            )
                .into_response()
        }
    }
}

/// A view model combining source data with computed schedule info.
pub(super) struct ScheduleEntry {
    pub(super) source: Source,
    pub(super) profile_name: String,
    pub(super) next_index_at: Option<chrono::DateTime<Utc>>,
    pub(super) is_overdue: bool,
}

pub(super) async fn schedule_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(query): axum::extract::Query<SourcesQuery>,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let (sources_result, profiles_result, recent_activity_result, cleanup_status) = tokio::join!(
        db::list_sources(&state.pool),
        db::list_profiles(&state.pool),
        db::list_activity_events(&state.pool, 20, 0, None, None, None, None),
        state.cleanup.ask(GetCleanupStatus)
    );

    let sources = match sources_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load sources for schedule");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load schedule"),
            );
        }
    };

    let profiles = profiles_result.unwrap_or_default();
    let recent_activity = recent_activity_result.unwrap_or_default();

    // Filter to only source-related activity
    let recent_runs: Vec<_> = recent_activity
        .iter()
        .filter(|e| {
            matches!(
                e.event_type,
                ActivityEventType::SourceIndexed | ActivityEventType::SourceError
            )
        })
        .collect();

    // Build source name lookup for recent runs
    let source_names: std::collections::HashMap<Ulid, String> = sources
        .iter()
        .map(|s| (s.id, s.display_name().to_string()))
        .collect();

    let now = Utc::now();

    // Build schedule entries. The name lookup above is built from the full set
    // so recent runs still resolve names for sources filtered out of the table.
    let mut entries: Vec<ScheduleEntry> = sources
        .into_iter()
        .filter(|source| search.is_none_or(|needle| source_matches_search(source, needle)))
        .map(|source| {
            let profile_name = profiles
                .iter()
                .find(|p| p.id == source.profile_id)
                .map_or_else(|| "Unknown".to_string(), |p| p.name.clone());

            let next_index_at = source
                .last_indexed_at
                .map(|last| last + chrono::Duration::seconds(source.index_frequency_secs));

            let is_overdue = source.enabled
                && next_index_at.is_some_and(|next| next < now)
                && source.last_error.is_some();

            ScheduleEntry {
                source,
                profile_name,
                next_index_at,
                is_overdue,
            }
        })
        .collect();

    // Sort: enabled sources first (disabled sink to the bottom), then overdue first,
    // then by next index time (soonest first)
    entries.sort_by(|a, b| {
        b.source
            .enabled
            .cmp(&a.source.enabled)
            .then_with(|| b.is_overdue.cmp(&a.is_overdue))
            .then_with(|| a.next_index_at.cmp(&b.next_index_at))
    });

    let page = layout_with_flash(
        "Schedule",
        NavItem::Schedule,
        flash,
        html! {
            (cleanup_status_section(cleanup_status.ok().as_ref(), now))

            section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                div class="flex flex-wrap items-center justify-between gap-3" {
                    h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Upcoming Indexing" }
                    (source_search_form("/schedule", "Search channel name...", search))
                }
                @if entries.is_empty() {
                    p class="mt-4 rounded-lg border border-dashed border-slate-300 dark:border-slate-600 bg-slate-50 dark:bg-slate-800 px-4 py-8 text-center text-sm text-slate-500 dark:text-slate-400" {
                        @if search.is_some() {
                            "No sources match your search."
                        } @else {
                            "No sources configured yet. Add sources to start scheduling."
                        }
                    }
                } @else {
                    div class="mt-4 space-y-2" {
                        @for entry in &entries {
                            (schedule_entry_row(entry, now))
                        }
                    }
                }
            }

            (recent_runs_section(&recent_runs, &source_names))
        },
    );

    (StatusCode::OK, page)
}

pub(super) fn cleanup_status_section(
    status: Option<&hof_core::actors::cleanup::CleanupStatus>,
    now: chrono::DateTime<Utc>,
) -> Markup {
    html! {
        section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            div class="flex flex-wrap items-center justify-between gap-4" {
                div {
                    h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Cleanup" }
                    p class="mt-1 text-sm text-slate-500 dark:text-slate-400" {
                        "Enforces retention policies and storage quotas by removing old files."
                    }
                }
                form method="post" action="/schedule/cleanup" {
                    button
                        class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-4 py-2 text-sm font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900"
                        type="submit"
                        onclick="return confirm('Run cleanup now? This will delete files past their retention period.')"
                    {
                        "Run Now"
                    }
                }
            }
            @if let Some(status) = status {
                div class="mt-4 grid gap-4 sm:grid-cols-3" {
                    // Status
                    div class="rounded-lg border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 p-3" {
                        p class="text-xs font-medium uppercase tracking-wide text-slate-500 dark:text-slate-400" { "Status" }
                        p class="mt-1 text-sm font-semibold text-slate-900 dark:text-slate-100" {
                            @if status.running { "Running" } @else { "Stopped" }
                        }
                    }
                    // Interval
                    @let interval_secs = i64::try_from(status.cleanup_interval_secs).unwrap_or(i64::MAX);
                    div class="rounded-lg border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 p-3" {
                        p class="text-xs font-medium uppercase tracking-wide text-slate-500 dark:text-slate-400" { "Interval" }
                        p class="mt-1 text-sm font-semibold text-slate-900 dark:text-slate-100" {
                            "every " (format_duration_human(interval_secs))
                        }
                    }
                    // Next run
                    div class="rounded-lg border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 p-3" {
                        p class="text-xs font-medium uppercase tracking-wide text-slate-500 dark:text-slate-400" { "Next Run" }
                        p class="mt-1 text-sm font-semibold text-slate-900 dark:text-slate-100" {
                            @if let Some(last) = status.last_run_at {
                                @let next = last + chrono::Duration::seconds(interval_secs);
                                @if next > now {
                                    "in " (format_time_delta(next - now))
                                } @else {
                                    "due now"
                                }
                            } @else {
                                "pending"
                            }
                        }
                    }
                }
                @if let Some(days) = status.global_retention_days {
                    p class="mt-3 text-xs text-slate-500 dark:text-slate-400" {
                        "Global retention: " (days) " days"
                    }
                }
            } @else {
                p class="mt-4 text-sm text-slate-500 dark:text-slate-400" { "Unable to retrieve cleanup status." }
            }
        }
    }
}

pub(super) fn recent_runs_section(
    recent_runs: &[&hof_core::domain::activity::ActivityEvent],
    source_names: &std::collections::HashMap<Ulid, String>,
) -> Markup {
    html! {
        section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Recent Indexing Runs" }
            @if recent_runs.is_empty() {
                p class="mt-4 rounded-lg border border-dashed border-slate-300 dark:border-slate-600 bg-slate-50 dark:bg-slate-800 px-4 py-8 text-center text-sm text-slate-500 dark:text-slate-400" {
                    "No indexing runs recorded yet."
                }
            } @else {
                div class="mt-4 overflow-x-auto" {
                    table class="min-w-full divide-y divide-slate-200 text-sm" {
                        thead class="bg-slate-50 dark:bg-slate-800" {
                            tr {
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Time" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Source" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Result" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Details" }
                            }
                        }
                        tbody class="divide-y divide-slate-100 dark:divide-slate-700 bg-white dark:bg-slate-900" {
                            @for run in recent_runs {
                                tr {
                                    td class="whitespace-nowrap px-3 py-2 text-slate-600 dark:text-slate-400" title=(run.created_at.to_rfc3339()) {
                                        (format_time_ago(run.created_at))
                                    }
                                    td class="px-3 py-2 text-slate-700 dark:text-slate-300" {
                                        @if let Some(source_id) = run.source_id {
                                            @if let Some(name) = source_names.get(&source_id) {
                                                (name)
                                            } @else {
                                                span class="text-slate-400 dark:text-slate-500 italic" { "deleted" }
                                            }
                                        } @else {
                                            span class="text-slate-400 dark:text-slate-500" { "—" }
                                        }
                                    }
                                    td class="px-3 py-2" {
                                        @if run.event_type == ActivityEventType::SourceIndexed {
                                            span class="inline-flex rounded-full bg-emerald-100 dark:bg-emerald-900/50 px-2.5 py-1 text-xs font-medium text-emerald-900 dark:text-emerald-100" {
                                                "OK"
                                            }
                                        } @else {
                                            span class="inline-flex rounded-full bg-rose-100 dark:bg-rose-900/50 px-2.5 py-1 text-xs font-medium text-rose-900 dark:text-rose-100" {
                                                "Error"
                                            }
                                        }
                                    }
                                    td class="max-w-md px-3 py-2 text-slate-700 dark:text-slate-300 break-words wrap-anywhere" {
                                        (run.message)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub(super) fn schedule_entry_row(entry: &ScheduleEntry, now: chrono::DateTime<Utc>) -> Markup {
    let frequency = format_duration_human(entry.source.index_frequency_secs);

    let (status_text, status_classes) = if !entry.source.enabled {
        ("disabled".to_string(), "text-slate-500 dark:text-slate-400")
    } else if entry.is_overdue {
        let overdue_duration = entry.next_index_at.map_or_else(
            || "unknown".to_string(),
            |next| format_time_delta(now - next),
        );
        (
            format!("overdue by {overdue_duration}"),
            "text-rose-600 dark:text-rose-400 font-medium",
        )
    } else if let Some(next) = entry.next_index_at {
        if next > now {
            (
                format!("in {}", format_time_delta(next - now)),
                "text-slate-600 dark:text-slate-400",
            )
        } else {
            (
                "due now".to_string(),
                "text-amber-600 dark:text-amber-400 font-medium",
            )
        }
    } else {
        (
            "not yet indexed".to_string(),
            "text-slate-500 dark:text-slate-400 italic",
        )
    };

    let border = if !entry.source.enabled {
        "border-l-4 border-l-slate-300 dark:border-l-slate-600"
    } else if entry.is_overdue {
        "border-l-4 border-l-rose-400"
    } else if entry.source.last_error.is_some() {
        "border-l-4 border-l-amber-400"
    } else {
        "border-l-4 border-l-emerald-400"
    };

    html! {
        div class=(format!("flex flex-wrap items-center justify-between gap-3 rounded-lg border border-slate-200 dark:border-slate-700 {} bg-white dark:bg-slate-800 p-4", border)) {
            div class="min-w-0 flex-1" {
                div class="flex flex-wrap items-center gap-2" {
                    p class="text-sm font-semibold text-slate-900 dark:text-slate-100" { (entry.source.display_name()) }
                    span class="rounded-full bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-500 dark:text-slate-400" {
                        (entry.profile_name)
                    }
                    @if !entry.source.enabled {
                        (status_chip("Disabled", "slate"))
                    }
                }
                @if let Some(ref error) = entry.source.last_error {
                    p class="mt-1 truncate text-xs text-rose-600 dark:text-rose-400" title=(error) {
                        "Error: " (error)
                    }
                }
            }
            div class="flex items-center gap-4 text-right" {
                div {
                    p class=(format!("text-sm {}", status_classes)) { (status_text) }
                    p class="text-xs text-slate-400 dark:text-slate-500" { "every " (frequency) }
                }
                @if entry.source.enabled {
                    form method="post" action=(format!("/sources/{}/index", entry.source.id)) {
                        button class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-xs font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900" type="submit" {
                            "Index Now"
                        }
                    }
                }
            }
        }
    }
}

pub(super) fn format_time_ago(timestamp: chrono::DateTime<Utc>) -> String {
    let delta = Utc::now() - timestamp;
    format_time_delta(delta) + " ago"
}

pub(super) fn format_time_delta(delta: chrono::TimeDelta) -> String {
    let secs = delta.num_seconds().max(0);
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        let mins = secs / 60;
        format!("{mins}m")
    } else if secs < 86_400 {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        if mins > 0 {
            format!("{hours}h {mins}m")
        } else {
            format!("{hours}h")
        }
    } else {
        let days = secs / 86_400;
        format!("{days}d")
    }
}

pub(super) fn format_duration_human(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{h}h")
        }
    } else {
        let d = secs / 86_400;
        format!("{d}d")
    }
}

pub(super) fn metric_card(title: &str, value: impl std::fmt::Display, description: &str) -> Markup {
    html! {
        article class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-5 shadow-sm" {
            p class="text-xs uppercase tracking-wide text-slate-500 dark:text-slate-400" { (title) }
            p class="mt-2 text-3xl font-semibold text-slate-900 dark:text-slate-100" { (value) }
            p class="mt-2 text-sm text-slate-600 dark:text-slate-400" { (description) }
        }
    }
}

/// Format a byte count using decimal (1000-based) SI units with a single decimal place,
/// e.g. `0.0 B`, `13.0 MB`, `1.4 GB`.
pub(super) fn format_bytes_human(bytes: i64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

    // Display-only human-readable size; precision loss is immaterial.
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    let mut value = bytes.max(0) as f64;
    let mut unit_index = 0usize;
    while value >= 1000.0 && unit_index < UNITS.len() - 1 {
        value /= 1000.0;
        unit_index += 1;
    }
    // The loop bounds `unit_index` to `UNITS.len() - 1`, so `get` always
    // succeeds; the fallback exists only to avoid an indexing panic path.
    let unit = UNITS.get(unit_index).copied().unwrap_or("B");
    format!("{value:.1} {unit}")
}

/// Percentage of quota used, clamped to `0.0..=100.0`.
///
/// Returns `0.0` when the quota is non-positive and nothing has been used, so callers never
/// divide by zero. A non-positive quota with any usage at all is treated as fully over budget.
pub(super) fn storage_usage_percent(used_bytes: i64, quota_bytes: i64) -> f64 {
    let quota = quota_bytes.max(0);
    if quota == 0 {
        return if used_bytes > 0 { 100.0 } else { 0.0 };
    }

    // Display-only percentage; precision loss is immaterial at these magnitudes.
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    let percent = (used_bytes.max(0) as f64 / quota as f64) * 100.0;
    percent.clamp(0.0, 100.0)
}

/// Whether usage exceeds the quota, treating a non-positive quota as leaving no room at all.
pub(super) fn is_storage_over_quota(used_bytes: i64, quota_bytes: i64) -> bool {
    used_bytes > quota_bytes.max(0)
}

/// Render the storage quota usage card for the dashboard: a headline "used / quota" figure with
/// a progress bar, plus a per-profile breakdown when more than one profile exists.
pub(super) fn storage_usage_card_markup(usage: &[db::ProfileStorageUsage]) -> Markup {
    let total_used: i64 = usage.iter().map(|profile| profile.used_bytes).sum();
    let total_quota: i64 = usage.iter().map(|profile| profile.quota_bytes).sum();
    let percent = storage_usage_percent(total_used, total_quota);
    let over_quota = is_storage_over_quota(total_used, total_quota);
    let headline_class = if over_quota {
        "text-rose-600 dark:text-rose-400"
    } else {
        "text-slate-900 dark:text-slate-100"
    };
    let bar_class = if over_quota {
        "bg-rose-500 dark:bg-rose-400"
    } else {
        "bg-emerald-500 dark:bg-emerald-400"
    };

    html! {
        section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Storage Quota" }
            @if usage.is_empty() {
                p class="mt-3 text-sm text-slate-500 dark:text-slate-400" { "No profiles configured yet." }
            } @else {
                p class=(format!("mt-3 text-2xl font-semibold {headline_class}")) {
                    (format_bytes_human(total_used)) " / " (format_bytes_human(total_quota))
                }
                div class="mt-2 h-2 w-full overflow-hidden rounded-full bg-slate-200 dark:bg-slate-700" {
                    div
                        class=(format!("h-full rounded-full {bar_class}"))
                        style=(format!("width: {percent:.1}%"))
                    {}
                }
                @if usage.len() > 1 {
                    ul class="mt-4 space-y-2" {
                        @for profile in usage {
                            @let profile_over = is_storage_over_quota(profile.used_bytes, profile.quota_bytes);
                            @let profile_class = if profile_over {
                                "text-rose-600 dark:text-rose-400 font-medium"
                            } else {
                                "text-slate-500 dark:text-slate-400"
                            };
                            li class="flex items-center justify-between gap-3 text-sm" {
                                span class="text-slate-700 dark:text-slate-300" { (profile.profile_name) }
                                span class=(profile_class) {
                                    (format_bytes_human(profile.used_bytes)) " / " (format_bytes_human(profile.quota_bytes))
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod storage_usage_tests {
    use super::{format_bytes_human, is_storage_over_quota, storage_usage_percent};

    #[test]
    fn format_bytes_human_table_driven() {
        let cases = [
            (0, "0.0 B"),
            (512, "512.0 B"),
            (1000, "1.0 KB"),
            (1000 * 1000, "1.0 MB"),
            (13 * 1000 * 1000, "13.0 MB"),
            (1_000_000_000_000_000, "1.0 PB"),
            (i64::MAX, "9223.4 PB"),
            (1_000_000_000_i64 + 400_000_000, "1.4 GB"),
            (1000_i64.pow(4), "1.0 TB"),
            (-5, "0.0 B"),
        ];

        for (bytes, expected) in cases {
            assert_eq!(
                format_bytes_human(bytes),
                expected,
                "Failed for bytes={bytes}"
            );
        }
    }

    #[test]
    fn storage_usage_percent_table_driven() {
        let cases = [
            (0, 0, 0.0),
            (5, 0, 100.0),
            (50, 100, 50.0),
            (150, 100, 100.0),
        ];

        for (used, quota, expected) in cases {
            assert!(
                (storage_usage_percent(used, quota) - expected).abs() < f64::EPSILON,
                "Failed for used={used}, quota={quota}, expected {expected}"
            );
        }
    }

    #[test]
    fn over_quota_detection() {
        assert!(!is_storage_over_quota(50, 100));
        assert!(is_storage_over_quota(150, 100));
        assert!(is_storage_over_quota(1, 0));
        assert!(!is_storage_over_quota(0, 0));
    }
}
