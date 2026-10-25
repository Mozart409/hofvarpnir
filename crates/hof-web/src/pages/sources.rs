//! Split out of `pages/mod.rs`.

use super::*;

/// Free-text filter for the sources and schedule pages.
///
/// Both pages already load every source to render, so the term is applied
/// in-memory rather than pushed into SQL.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SourcesQuery {
    pub(super) search: Option<String>,
}

/// Plain GET search form shared by the sources and schedule pages.
///
/// Both filter the same `Source` set in-memory, so neither needs htmx here —
/// a full navigation keeps the URL shareable and the back button meaningful.
pub(super) fn source_search_form(action: &str, placeholder: &str, search: Option<&str>) -> Markup {
    html! {
        form method="get" action=(action) class="flex gap-2" {
            input
                type="text"
                name="search"
                placeholder=(placeholder)
                value=(search.unwrap_or(""))
                class="rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-900 dark:text-slate-100";
            button type="submit"
                class="rounded-lg bg-slate-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-slate-700"
            { "Search" }
            @if search.is_some() {
                a href=(action)
                    class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                { "Clear" }
            }
        }
    }
}

/// Case-insensitive match of a source against a search term.
///
/// Covers every name a source can be known by — the user's custom label, the
/// platform channel/playlist title, and the URL — because which one is
/// populated varies by source type and indexing state.
pub(super) fn source_matches_search(source: &Source, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    let haystacks = [
        source.custom_name.as_deref(),
        source.channel_title.as_deref(),
        Some(source.url.as_str()),
    ];
    haystacks
        .into_iter()
        .flatten()
        .any(|value| value.to_lowercase().contains(&needle))
}

pub(super) async fn sources_page(
    auth: AuthUser,
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
    // Get profiles for the current user to populate the dropdown
    let profiles = match db::list_profiles_for_user(&state.pool, auth.user_id).await {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load profiles for sources page");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load sources page"),
            );
        }
    };

    // Get sources for the user's profiles
    let mut sources = match db::list_sources(&state.pool).await {
        Ok(data) => {
            // Filter to only show sources belonging to user's profiles
            let profile_ids: std::collections::HashSet<_> = profiles.iter().map(|p| p.id).collect();
            data.into_iter()
                .filter(|s| profile_ids.contains(&s.profile_id))
                .collect::<Vec<_>>()
        }
        Err(error) => {
            tracing::error!(%error, "failed to load sources for sources page");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load sources page"),
            );
        }
    };

    if let Some(needle) = search {
        sources.retain(|source| source_matches_search(source, needle));
    }

    // Sort sources alphabetically by display name
    sources.sort_by_key(|s| s.display_name().to_lowercase());

    // Calculate default cutoff date (7 days ago)
    let default_cutoff_date = (Utc::now() - chrono::Duration::days(7))
        .format("%Y-%m-%d")
        .to_string();

    let page = layout_with_flash(
        "Sources",
        NavItem::Sources,
        flash,
        html! {
            section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Create Source" }
                form class="mt-4 grid gap-4 md:grid-cols-2" method="post" action="/sources" {
                    div {
                        label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for="profile_id" { "Profile" }
                        select class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm" name="profile_id" id="profile_id" required {
                            @for profile in &profiles {
                                option value=(profile.id.to_string()) {
                                    (profile.name) " (" (profile.id.to_string()) ")"
                                }
                            }
                        }
                    }
                    div {
                        label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for="source_type" { "Source Type" }
                        select class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm" name="source_type" id="source_type" required {
                            option value="channel" { "Channel" }
                            option value="playlist" { "Playlist" }
                        }
                    }
                    (input_text("URL", "url", "https://youtube.com/@channel", true, ""))
                    (input_text("Custom Name", "custom_name", "Optional label", false, ""))
                    (input_index_frequency("Index Frequency", "index_frequency_secs", 43200))
                    (input_cutoff_date("Cutoff Date", "cutoff_date", &default_cutoff_date))
                    (input_number("Retention Days", "retention_days", "Optional", false, ""))
                    div class="md:col-span-2" {
                        button class="inline-flex items-center rounded-lg bg-sky-600 dark:bg-sky-500 px-4 py-2 text-sm font-medium text-white transition hover:bg-sky-700 dark:hover:bg-sky-600" type="submit" { "Create Source" }
                    }
                }
            }

            section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                div class="flex flex-wrap items-center justify-between gap-3" {
                    h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Existing Sources" }
                    (source_search_form("/sources", "Search name, channel or URL...", search))
                }
                // No SSE here: a live refresh would re-render the unfiltered
                // list and silently discard the user's search.
                @if search.is_some() {
                    div id="sources-list" {
                        (sources_list_markup(&sources, search))
                    }
                } @else {
                    div hx-ext="sse" sse-connect="/web/sources/events" {
                        div id="sources-list" sse-swap="sources-update" hx-swap="innerHTML show:none" {
                            (sources_list_markup(&sources, search))
                        }
                    }
                }
            }
        },
    );

    (StatusCode::OK, page)
}

pub(super) async fn create_source(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<SourceForm>,
) -> impl IntoResponse {
    let Ok(profile_id) = Ulid::from_string(form.profile_id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid profile ID provided"),
        )
            .into_response();
    };

    let Ok(cutoff_date) = NaiveDate::parse_from_str(form.cutoff_date.trim(), "%Y-%m-%d") else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid cutoff date. Use YYYY-MM-DD."),
        )
            .into_response();
    };

    let retention_days = match parse_optional_i32(form.retention_days.as_deref(), "Retention days")
    {
        Ok(value) => value,
        Err(message) => {
            return (StatusCode::BAD_REQUEST, error_page(&message)).into_response();
        }
    };

    let custom_name = form
        .custom_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());

    let create = CreateSource {
        profile_id,
        url: form.url.trim(),
        source_type: form.source_type.into(),
        custom_name,
        index_frequency_secs: form.index_frequency_secs,
        cutoff_date,
        retention_days,
    };

    match db::create_source(&state.pool, create).await {
        Ok(source) => {
            let name = source.display_name();
            state
                .broadcaster
                .log_and_broadcast(
                    &state.pool,
                    ActivityEventType::SourceCreated,
                    ActivitySeverity::Info,
                    &format!("Added source \"{name}\""),
                    Some(source.id),
                    None,
                    Some(profile_id),
                )
                .await;
            set_flash(&session, "success", &format!("Source \"{name}\" added")).await;
            Redirect::to("/sources").into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to create source from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to create source"),
            )
                .into_response()
        }
    }
}

pub(super) async fn update_source(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Form(form): Form<SourceForm>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    let Ok(cutoff_date) = NaiveDate::parse_from_str(form.cutoff_date.trim(), "%Y-%m-%d") else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid cutoff date. Use YYYY-MM-DD."),
        )
            .into_response();
    };

    let retention_days = match parse_optional_i32(form.retention_days.as_deref(), "Retention days")
    {
        Ok(value) => value,
        Err(message) => {
            return (StatusCode::BAD_REQUEST, error_page(&message)).into_response();
        }
    };

    let custom_name = form
        .custom_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());

    let update = UpdateSource {
        url: Some(form.url.trim()),
        source_type: Some(form.source_type.into()),
        custom_name: Some(custom_name),
        index_frequency_secs: Some(form.index_frequency_secs),
        cutoff_date: Some(cutoff_date),
        retention_days: Some(retention_days),
    };

    match db::update_source(&state.pool, source_id, update).await {
        Ok(_) => {
            state.broadcaster.invalidate();
            set_flash(&session, "success", "Source updated").await;
            Redirect::to("/sources").into_response()
        }
        Err(db::DbError::NotFound) => {
            (StatusCode::NOT_FOUND, error_page("Source not found")).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to update source from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to update source"),
            )
                .into_response()
        }
    }
}

pub(super) async fn delete_source(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    match db::delete_source(&state.pool, source_id).await {
        Ok(()) => {
            state
                .broadcaster
                .log_and_broadcast(
                    &state.pool,
                    ActivityEventType::SourceDeleted,
                    ActivitySeverity::Info,
                    &format!("Deleted source {source_id}"),
                    Some(source_id),
                    None,
                    None,
                )
                .await;
            set_flash(&session, "success", "Source deleted").await;
            Redirect::to("/sources").into_response()
        }
        Err(db::DbError::NotFound) => {
            (StatusCode::NOT_FOUND, error_page("Source not found")).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to delete source from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to delete source"),
            )
                .into_response()
        }
    }
}

pub(super) async fn toggle_source_enabled(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    // First get the current state
    let source = match db::get_source(&state.pool, source_id).await {
        Ok(s) => s,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Source not found")).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to get source for toggle");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to toggle source"),
            )
                .into_response();
        }
    };

    let new_enabled = !source.enabled;
    match db::set_source_enabled(&state.pool, source_id, new_enabled).await {
        Ok(()) => {
            let status = if new_enabled { "enabled" } else { "disabled" };
            let name = source.display_name();
            state
                .broadcaster
                .log_and_broadcast(
                    &state.pool,
                    ActivityEventType::SourceUpdated,
                    ActivitySeverity::Info,
                    &format!("Source \"{name}\" {status}"),
                    Some(source_id),
                    None,
                    Some(source.profile_id),
                )
                .await;
            set_flash(&session, "success", &format!("Source {status}")).await;
            Redirect::to(&source_action_redirect(&headers, source_id)).into_response()
        }
        Err(db::DbError::NotFound) => {
            (StatusCode::NOT_FOUND, error_page("Source not found")).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to toggle source enabled state");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to toggle source"),
            )
                .into_response()
        }
    }
}

/// Toggle whether a source's videos are exempt from automatic cleanup.
pub(super) async fn toggle_source_cleanup_exclusion(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    let source = match db::get_source(&state.pool, source_id).await {
        Ok(s) => s,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Source not found")).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to get source for cleanup exclusion toggle");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to toggle cleanup exclusion"),
            )
                .into_response();
        }
    };

    let new_excluded = !source.exclude_from_cleanup;
    match db::set_source_exclude_from_cleanup(&state.pool, source_id, new_excluded).await {
        Ok(()) => {
            let status = if new_excluded {
                "excluded from cleanup"
            } else {
                "included in cleanup"
            };
            let name = source.display_name();
            state
                .broadcaster
                .log_and_broadcast(
                    &state.pool,
                    ActivityEventType::SourceUpdated,
                    ActivitySeverity::Info,
                    &format!("Source \"{name}\" {status}"),
                    Some(source_id),
                    None,
                    Some(source.profile_id),
                )
                .await;
            set_flash(&session, "success", &format!("Source {status}")).await;
            Redirect::to(&source_action_redirect(&headers, source_id)).into_response()
        }
        Err(db::DbError::NotFound) => {
            (StatusCode::NOT_FOUND, error_page("Source not found")).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to toggle source cleanup exclusion");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to toggle cleanup exclusion"),
            )
                .into_response()
        }
    }
}

/// Where a source action should return the operator to.
///
/// The action row renders in two places now — the sources list and a source's
/// detail page — so "back to `/sources`" is no longer a safe default. When the
/// click came from `/sources/{id}` the referer says so, and bouncing the
/// operator off the page they were working on to hunt for the same source in
/// the list is the wrong answer. The schedule page carries the same buttons
/// and keeps its existing special case.
///
/// Anything unrecognised falls back to `/sources`, including a missing or
/// unreadable `Referer`.
pub(super) fn source_action_redirect(headers: &HeaderMap, source_id: Ulid) -> String {
    let Some(referer) = headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
    else {
        return "/sources".to_string();
    };

    // Compare on the last path segment only: the referer is absolute
    // (`https://host/sources/<id>`), and the host depends on how the instance
    // is reached.
    let last_segment = referer
        .split('?')
        .next()
        .unwrap_or(referer)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();

    if last_segment == "schedule" {
        "/schedule".to_string()
    } else if last_segment == source_id.to_string() {
        format!("/sources/{source_id}")
    } else {
        "/sources".to_string()
    }
}

/// Clear a source's stored entry order and index it again so it is re-detected.
///
/// The reset on its own is silent. The indexer only re-detects when
/// `entry_order` is `Unknown` (`source_indexer.rs`), and the next scheduled
/// run can be days out — `index_frequency_secs` is three days on some real
/// sources — so an operator would click this and see nothing change. Indexing
/// straight after is what makes the button do what its label says, and it is
/// not expensive: detection runs before the entries are walked, so the fresh
/// verdict still enables early termination on that same run.
///
/// The two halves are reported separately on purpose. The reset is the part
/// that matters and it has already committed by the time indexing is
/// attempted, so a scheduler that refuses — paused, draining, or already
/// indexing this source — is a flash rather than an error page: the reset
/// stands and the next scheduled index picks it up.
pub(super) async fn redetect_source_order(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    match db::update_source_entry_order(&state.pool, source_id, EntryOrder::Unknown).await {
        Ok(()) => {}
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Source not found")).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to reset entry order from web form");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to reset the entry order for this source"),
            )
                .into_response();
        }
    }

    state.broadcaster.invalidate();

    let redirect_to = source_action_redirect(&headers, source_id);

    match state.scheduler.ask(IndexSource { source_id }).await {
        Ok(()) => {
            set_flash(
                &session,
                "info",
                "Entry order cleared; indexing triggered to detect it again",
            )
            .await;
        }
        Err(error) => {
            tracing::warn!(%error, "entry order reset, but indexing could not be triggered");
            set_flash(
                &session,
                "error",
                &format!(
                    "Entry order cleared, but indexing could not start: {error}. It will be \
                     detected again on the next scheduled index."
                ),
            )
            .await;
        }
    }

    Redirect::to(&redirect_to).into_response()
}

pub(super) async fn trigger_index(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    let redirect_to = source_action_redirect(&headers, source_id);

    match state.scheduler.ask(IndexSource { source_id }).await {
        Ok(()) => {
            set_flash(&session, "info", "Indexing triggered").await;
            Redirect::to(&redirect_to).into_response()
        }
        Err(error) => {
            // A pause or drain refusal is an expected operator condition, not a
            // server fault: the scheduler already produced a message saying
            // exactly why ("Indexing is paused until ..."). Surfacing it as a
            // flash and returning the operator to where they were beats a 500
            // error page that throws that message away. Anything else really
            // is a failure and keeps the error page.
            let message = error.to_string();
            if message.starts_with(PAUSED_REFUSAL_PREFIX)
                || message.contains(DRAINING_REFUSAL_MESSAGE)
                || message.contains("already being indexed")
            {
                set_flash(&session, "error", &message).await;
                return Redirect::to(&redirect_to).into_response();
            }
            tracing::error!(%error, "failed to trigger source index from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to trigger indexing for this source"),
            )
                .into_response()
        }
    }
}

pub(super) async fn trigger_metadata(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    // Check if source has channel metadata
    match db::get_source(&state.pool, source_id).await {
        Ok(source) if source.channel_thumbnail_url.is_none() => {
            return (
                StatusCode::BAD_REQUEST,
                error_page(
                    "Source has no channel metadata. Run 'Trigger Index' first to fetch \
                     channel information from YouTube.",
                ),
            )
                .into_response();
        }
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Source not found")).into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to get source");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to get source"),
            )
                .into_response();
        }
        Ok(_) => {}
    }

    match state
        .jellyfin_metadata
        .ask(TriggerSourceMetadata { source_id })
        .await
    {
        Ok(result) if result.success => {
            set_flash(&session, "success", "Metadata generation started").await;
            Redirect::to(&source_action_redirect(&headers, source_id)).into_response()
        }
        Ok(result) => {
            let error_msg = result.error.unwrap_or_else(|| "Unknown error".to_string());
            tracing::error!(error = %error_msg, "failed to generate metadata from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page(&format!("Failed to generate metadata: {error_msg}")),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to trigger metadata generation from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to trigger metadata generation for this source"),
            )
                .into_response()
        }
    }
}

pub(super) async fn source_detail_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // The action row renders here now, and its handlers report what they did
    // through a flash. Without this the page would silently drop every one of
    // those messages — and worse, leave them in the session to surface on
    // whatever page the operator opened next.
    let flash = take_flash(&session).await;

    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        );
    };

    let source = match db::get_source(&state.pool, source_id).await {
        Ok(s) => s,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Source not found"));
        }
        Err(error) => {
            tracing::error!(%error, "failed to load source");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load source"),
            );
        }
    };

    let videos = match db::list_videos_for_source(&state.pool, source_id).await {
        Ok(v) => v,
        Err(error) => {
            tracing::error!(%error, "failed to load videos for source");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load videos"),
            );
        }
    };

    let events_url = format!("/web/sources/{source_id}/events");

    let page = layout_with_flash(
        &format!("Source: {}", source.display_name()),
        NavItem::Sources,
        flash,
        html! {
            div class="mb-4" {
                a href="/sources" class="text-sm text-sky-600 dark:text-sky-400 hover:underline" {
                    "← Back to Sources"
                }
            }
            div hx-ext="sse" sse-connect=(events_url) {
                div id="source-detail-content" sse-swap="source-detail-update" hx-swap="innerHTML" {
                    (source_detail_content_markup(&source, &videos))
                }
            }
        },
    );

    (StatusCode::OK, page)
}

/// The action buttons under a source's edit form.
///
/// All of these submit the surrounding form, overriding its target with
/// `formaction`, so field edits are not lost when triggering a side action.
pub(super) fn source_editor_actions(source: &Source) -> Markup {
    html! {
        div class="md:col-span-2 flex flex-wrap gap-2" {
            button class="rounded-lg bg-slate-900 dark:bg-slate-100 px-4 py-2 text-sm font-medium text-white dark:text-slate-900 hover:bg-slate-700 dark:hover:bg-slate-200" type="submit" {
                "Save Source"
            }
            (source_side_actions(source))
        }
    }
}

/// Every action that operates on the source as a whole rather than on the
/// edit form's fields.
///
/// Split out from `source_editor_actions` so the source detail page can carry
/// the same row. "Save Source" stays behind: it submits the edit form's
/// fields, which only exist in the list's editor.
pub(super) fn source_side_actions(source: &Source) -> Markup {
    let action = |verb: &str| format!("/sources/{}/{verb}", source.id);

    html! {
        @if source.enabled {
            button class="rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/50 px-4 py-2 text-sm font-medium text-amber-700 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900" type="submit" formaction=(action("toggle")) {
                "Disable"
            }
        } @else {
            button class="rounded-lg border border-emerald-200 dark:border-emerald-800 bg-emerald-50 dark:bg-emerald-900/50 px-4 py-2 text-sm font-medium text-emerald-700 dark:text-emerald-300 hover:bg-emerald-100 dark:hover:bg-emerald-900" type="submit" formaction=(action("toggle")) {
                "Enable"
            }
        }
        @if source.exclude_from_cleanup {
            button
                class="rounded-lg border border-emerald-200 dark:border-emerald-800 bg-emerald-50 dark:bg-emerald-900/50 px-4 py-2 text-sm font-medium text-emerald-700 dark:text-emerald-300 hover:bg-emerald-100 dark:hover:bg-emerald-900"
                type="submit"
                title="Videos from this source are currently kept forever"
                formaction=(action("toggle-cleanup-exclusion"))
            {
                "Include in Cleanup"
            }
        } @else {
            button
                class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-4 py-2 text-sm font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                type="submit"
                title="Keep this source's videos forever, ignoring retention and quota"
                formaction=(action("toggle-cleanup-exclusion"))
            {
                "Exclude from Cleanup"
            }
        }
        button class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-4 py-2 text-sm font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900" type="submit" formaction=(action("index")) {
            "Trigger Index"
        }
        button class="rounded-lg border border-violet-200 dark:border-violet-800 bg-violet-50 dark:bg-violet-900/50 px-4 py-2 text-sm font-medium text-violet-700 dark:text-violet-300 hover:bg-violet-100 dark:hover:bg-violet-900" type="submit" formaction=(action("metadata")) {
            "Trigger Image Download"
        }
        button
            class="rounded-lg border border-indigo-200 dark:border-indigo-800 bg-indigo-50 dark:bg-indigo-900/50 px-4 py-2 text-sm font-medium text-indigo-700 dark:text-indigo-300 hover:bg-indigo-100 dark:hover:bg-indigo-900"
            type="submit"
            title="Clear the stored entry order and index again, so it is worked out from scratch"
            formaction=(action("reset-order"))
        {
            "Re-detect Order"
        }
        button
            class="rounded-lg border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/50 px-4 py-2 text-sm font-medium text-rose-700 dark:text-rose-300 hover:bg-rose-100 dark:hover:bg-rose-900"
            type="submit"
            formaction=(action("delete"))
            onclick="return confirm('Delete this source? This cannot be undone.')"
        {
            "Delete"
        }
    }
}

/// The same source-wide actions, rendered on the source detail page.
///
/// `source_editor_actions` reaches these through the edit form they sit
/// inside; here there is no such form, so they get their own. It carries no
/// fields on purpose: every button names its own target with `formaction`, and
/// a form with no text input cannot be submitted implicitly by pressing Enter,
/// so the form's own `action` is never reached. It still points somewhere
/// harmless rather than at `/sources/{id}`, which is `update_source` and would
/// reject a body carrying none of its fields.
pub(super) fn source_detail_actions(source: &Source) -> Markup {
    html! {
        form
            class="mt-4 flex flex-wrap gap-2"
            method="post"
            action=(format!("/sources/{}/index", source.id))
        {
            (source_side_actions(source))
        }
    }
}

pub(super) fn source_editor(source: &Source) -> Markup {
    // Determine border color based on enabled and error state
    let border_class = if !source.enabled {
        "border-slate-300 dark:border-slate-600 bg-slate-100/60 dark:bg-slate-900/60 opacity-60"
    } else if source.last_error.is_some() {
        "border-rose-300 dark:border-rose-700 bg-rose-50/60 dark:bg-rose-900/30"
    } else {
        "border-slate-200 dark:border-slate-700 bg-slate-50/60 dark:bg-slate-800/60"
    };

    html! {
        details class=(format!("group rounded-xl border {} p-4 open:bg-white dark:open:bg-slate-800 open:opacity-100", border_class)) {
            summary class="cursor-pointer list-none" {
                div class="flex flex-wrap items-center justify-between gap-3" {
                    div class="min-w-0" {
                        p class="truncate text-sm font-semibold text-slate-900 dark:text-slate-100" {
                            (source.custom_name.as_deref().unwrap_or(&source.url))
                        }
                        p class="truncate text-xs text-slate-500 dark:text-slate-400" { (source.id.to_string()) }
                    }
                    div class="flex items-center gap-2" {
                        a href=(format!("/sources/{}", source.id))
                            class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-xs font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900"
                        {
                            "View Videos"
                        }
                        @if !source.enabled {
                            (status_chip("Disabled", "amber"))
                        }
                        @if source.exclude_from_cleanup {
                            (status_chip("Cleanup exempt", "emerald"))
                        }
                        @if source.last_error.is_some() {
                            (status_chip(&format!("Error ({})", source.index_error_count), "rose"))
                        }
                        (status_chip(source_type_label(&source.source_type), "slate"))
                    }
                }
            }

            // Show error message if present
            @if let Some(ref error) = source.last_error {
                div class="mt-3 rounded-lg border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/30 p-3" {
                    p class="text-sm font-medium text-rose-800 dark:text-rose-200" { "Last Indexing Error:" }
                    p class="mt-1 text-sm text-rose-700 dark:text-rose-300 font-mono whitespace-pre-wrap break-all" { (error) }
                    p class="mt-2 text-xs text-rose-600 dark:text-rose-400" {
                        "Consecutive errors: " (source.index_error_count)
                    }
                }
            }
            form class="mt-4 grid gap-4 md:grid-cols-2" method="post" action={(format!("/sources/{}", source.id))} {
                (input_text("Profile ID", "profile_id", "", true, &source.profile_id.to_string()))
                div {
                    label class="block text-sm font-medium text-slate-700 dark:text-slate-300" { "Source Type" }
                    select class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm" name="source_type" required {
                        option value="channel" selected[matches!(source.source_type, SourceType::Channel)] { "Channel" }
                        option value="playlist" selected[matches!(source.source_type, SourceType::Playlist)] { "Playlist" }
                    }
                }
                (input_text("URL", "url", "", true, &source.url))
                (input_text("Custom Name", "custom_name", "Optional", false, &source.custom_name.clone().unwrap_or_default()))
                (input_index_frequency("Index Frequency", "index_frequency_secs", source.index_frequency_secs))
                (input_cutoff_date("Cutoff Date", "cutoff_date", &source.cutoff_date.to_string()))
                (input_number("Retention Days", "retention_days", "Optional", false, &source.retention_days.map_or_else(String::new, |days| days.to_string())))
                (source_editor_actions(source))
            }
        }
    }
}

pub(super) fn sources_list_markup(sources: &[Source], search: Option<&str>) -> Markup {
    html! {
        @if sources.is_empty() {
            p class="mt-3 text-sm text-slate-500 dark:text-slate-400" {
                @if search.is_some() {
                    "No sources match your search."
                } @else {
                    "No sources yet."
                }
            }
        } @else {
            div class="mt-4 space-y-4" {
                @for source in sources {
                    (source_editor(source))
                }
            }
        }
    }
}

pub(super) fn source_detail_content_markup(
    source: &Source,
    videos: &[hof_core::domain::video::Video],
) -> Markup {
    html! {
        (source_detail_header(source, videos.len()))
        (source_videos_table(videos))
    }
}

pub(super) fn source_detail_header(source: &Source, video_count: usize) -> Markup {
    html! {
        section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            div class="flex items-start gap-4" {
                @if let Some(ref thumb) = source.channel_thumbnail_url {
                    img src=(thumb) alt="Channel thumbnail" class="h-16 w-16 rounded-full object-cover";
                }
                div {
                    h2 class="text-xl font-semibold text-slate-900 dark:text-slate-100" {
                        (source.display_name())
                    }
                    p class="mt-1 text-sm text-slate-500 dark:text-slate-400" {
                        a href=(source.url) target="_blank" class="hover:underline" { (source.url) }
                    }
                    div class="mt-2 flex flex-wrap gap-2 text-xs text-slate-600 dark:text-slate-400" {
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1" {
                            (format!("{:?}", source.source_type))
                        }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1" {
                            (format!("{video_count} videos"))
                        }
                        span class=(entry_order_badge_class(source.entry_order)) {
                            (entry_order_label(source.entry_order))
                            // When the verdict was reached, not when the
                            // source was last indexed: the two diverge by up
                            // to `REDETECTION_DAYS`, and it is the detection
                            // age that says whether a surprising label is a
                            // fresh reading or one latched months ago. There
                            // is no timestamp while the order is `Unknown` —
                            // resetting nulls it — so nothing renders then.
                            @if let Some(detected_at) = source.entry_order_detected_at {
                                " · detected " (format_time_ago(detected_at))
                            }
                        }
                        @if let Some(indexed_at) = source.last_indexed_at {
                            span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1" {
                                "Last indexed: " (format_time_ago(indexed_at))
                            }
                        }
                    }
                }
            }
            (source_detail_actions(source))
        }
    }
}

pub(super) fn source_videos_table(videos: &[hof_core::domain::video::Video]) -> Markup {
    html! {
        section class="mt-6 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h3 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Videos" }
            @if videos.is_empty() {
                p class="mt-3 text-sm text-slate-500 dark:text-slate-400" {
                    "No videos indexed yet. Try triggering an index from the Sources page."
                }
            } @else {
                div class="mt-4 overflow-x-auto" {
                    table class="min-w-full divide-y divide-slate-200 dark:divide-slate-700 text-sm" {
                        thead class="bg-slate-50 dark:bg-slate-800" {
                            tr {
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Thumbnail" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Title" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Duration" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Published" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Status" }
                            }
                        }
                        tbody class="divide-y divide-slate-100 dark:divide-slate-700 bg-white dark:bg-slate-900" {
                            @for video in videos {
                                (video_table_row(video))
                            }
                        }
                    }
                }
            }
        }
    }
}

pub(super) fn video_table_row(video: &hof_core::domain::video::Video) -> Markup {
    html! {
        tr {
            td class="px-3 py-2" {
                @if let Some(ref thumb) = video.thumbnail_url {
                    img src=(thumb) alt="" class="h-12 w-20 rounded object-cover";
                } @else {
                    div class="h-12 w-20 rounded bg-slate-200 dark:bg-slate-700" {}
                }
            }
            td class="max-w-md px-3 py-2 text-slate-900 dark:text-slate-100" {
                p class="truncate font-medium" { (video.title) }
                div class="flex items-center gap-2" {
                    p class="truncate text-xs text-slate-500 dark:text-slate-400" {
                        (video.platform_video_id)
                    }
                    (delivered_quality_badge(video))
                }
            }
            td class="px-3 py-2 text-slate-600 dark:text-slate-400" {
                @if let Some(secs) = video.duration_secs {
                    (format_duration_human(secs))
                } @else {
                    "—"
                }
            }
            td class="px-3 py-2 text-slate-600 dark:text-slate-400" {
                @if let Some(published) = video.published_at {
                    (published.format("%Y-%m-%d"))
                } @else {
                    "—"
                }
            }
            td class="px-3 py-2" { (status_badge(&video.status)) }
        }
    }
}
