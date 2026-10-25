//! Split out of `pages/mod.rs`.

use super::*;

/// Coalesce window for SSE debouncing: rapid bursts collapse into one push.
pub(super) const SSE_COALESCE_WINDOW: Duration = Duration::from_millis(500);

/// Wrap a `broadcast::Receiver<()>` into a debounced `Stream`.
///
/// On the first signal, starts a coalesce window. Any signals that arrive
/// during the window are drained and discarded. One item is yielded after the
/// window expires. Returns `None` (ends the stream) only when the channel is
/// closed (no more senders).
pub(super) fn debounced_broadcast(rx: broadcast::Receiver<()>) -> impl Stream<Item = ()> {
    use tokio::sync::broadcast::error::RecvError;
    unfold(rx, |mut rx| async move {
        // Wait for the first signal (or detect channel closure).
        match rx.recv().await {
            Ok(()) | Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => return None,
        }
        // Coalesce additional signals within the window.
        tokio::time::sleep(SSE_COALESCE_WINDOW).await;
        while rx.try_recv().is_ok() {}
        Some(((), rx))
    })
}

#[allow(clippy::too_many_lines)]
pub(super) async fn dashboard_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
    let (profiles_result, sources_result, videos_result, source_names_result, storage_usage_result) = tokio::join!(
        db::list_profiles(&state.pool),
        db::list_sources(&state.pool),
        db::list_videos(&state.pool, None),
        db::get_source_names_for_videos(&state.pool),
        db::get_storage_usage_by_profile(&state.pool)
    );

    let profiles = match profiles_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load profiles for dashboard");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load dashboard"),
            );
        }
    };

    let sources = match sources_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load sources for dashboard");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load dashboard"),
            );
        }
    };

    let videos = match videos_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load videos for dashboard");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load dashboard"),
            );
        }
    };

    let source_names = match source_names_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load source names for dashboard");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load dashboard"),
            );
        }
    };

    let storage_usage = match storage_usage_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load storage usage for dashboard");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load dashboard"),
            );
        }
    };

    let pending = videos
        .iter()
        .filter(|video| video.status == VideoStatus::Pending)
        .count();
    let downloading = videos
        .iter()
        .filter(|video| video.status == VideoStatus::Downloading)
        .count();
    let completed = videos
        .iter()
        .filter(|video| video.status == VideoStatus::Completed)
        .count();
    let failed = videos
        .iter()
        .filter(|video| {
            matches!(
                video.status,
                VideoStatus::Failed | VideoStatus::PermanentlyFailed
            )
        })
        .count();

    let recent = videos.iter().take(8).collect::<Vec<_>>();

    let page = layout_with_flash(
        "Dashboard",
        NavItem::Dashboard,
        flash,
        html! {
            div
                hx-ext="sse"
                sse-connect="/web/dashboard/events"
            {
                div id="dashboard-metrics" sse-swap="dashboard-update" hx-swap="innerHTML" {
                    (dashboard_metrics_markup(
                        profiles.len(),
                        sources.len(),
                        pending,
                        downloading,
                        completed,
                        failed,
                        &recent,
                        &source_names,
                        &storage_usage,
                    ))
                }
            }
        },
    );

    (StatusCode::OK, page)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn dashboard_metrics_markup(
    profiles: usize,
    sources: usize,
    pending: usize,
    downloading: usize,
    completed: usize,
    failed: usize,
    recent: &[&Video],
    source_names: &HashMap<Ulid, String>,
    storage_usage: &[db::ProfileStorageUsage],
) -> Markup {
    html! {
        div class="grid gap-4 md:grid-cols-2 xl:grid-cols-4" {
            (metric_card("Profiles", profiles, "Active download configurations"))
            (metric_card("Sources", sources, "Channels and playlists being tracked"))
            (metric_card("Pending", pending, "Queued for download"))
            (metric_card("In Progress", downloading, "Currently downloading"))
        }
        div class="mt-4 grid gap-4 md:grid-cols-2" {
            (metric_card("Completed", completed, "Successfully archived videos"))
            (metric_card("Failed", failed, "Need retry or manual check"))
        }
        (storage_usage_card_markup(storage_usage))
        section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Recent Downloads" }
            @if recent.is_empty() {
                p class="mt-3 text-sm text-slate-500 dark:text-slate-400" { "No downloads found yet." }
            } @else {
                ul class="mt-4 space-y-3" {
                    @for video in recent {
                        li class="flex items-center justify-between gap-3 rounded-xl border border-slate-100 dark:border-slate-700 bg-slate-50/80 dark:bg-slate-800/80 px-3 py-2" {
                            div class="min-w-0 flex-1" {
                                p class="truncate text-sm font-medium text-slate-900 dark:text-slate-100" { (video.title) }
                                p class="text-xs text-slate-500 dark:text-slate-400" { (video.platform) " / " (video.platform_video_id) }
                            }
                            @if let Some(source_name) = source_names.get(&video.id) {
                                span class="shrink-0 truncate max-w-[10rem] text-xs text-slate-600 dark:text-slate-300" title=(source_name) { (source_name) }
                            }
                            (delivered_quality_badge(video))
                            (status_badge(&video.status))
                        }
                    }
                }
            }
        }
    }
}

pub(super) async fn dashboard_events_sse(
    _auth: AuthUser,
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.broadcaster.subscribe_invalidate();

    let stream = debounced_broadcast(rx).filter_map(move |()| {
        let pool = state.pool.clone();
        async move {
            let (
                profiles_result,
                sources_result,
                videos_result,
                source_names_result,
                storage_usage_result,
            ) = tokio::join!(
                db::list_profiles(&pool),
                db::list_sources(&pool),
                db::list_videos(&pool, None),
                db::get_source_names_for_videos(&pool),
                db::get_storage_usage_by_profile(&pool)
            );
            let profiles = profiles_result.ok()?;
            let sources = sources_result.ok()?;
            let videos = videos_result.ok()?;
            let source_names = source_names_result.ok()?;
            let storage_usage = storage_usage_result.ok()?;

            let pending = videos
                .iter()
                .filter(|v| v.status == VideoStatus::Pending)
                .count();
            let downloading = videos
                .iter()
                .filter(|v| v.status == VideoStatus::Downloading)
                .count();
            let completed = videos
                .iter()
                .filter(|v| v.status == VideoStatus::Completed)
                .count();
            let failed = videos
                .iter()
                .filter(|v| {
                    matches!(
                        v.status,
                        VideoStatus::Failed | VideoStatus::PermanentlyFailed
                    )
                })
                .count();

            let recent = videos.iter().take(8).collect::<Vec<_>>();
            let fragment = dashboard_metrics_markup(
                profiles.len(),
                sources.len(),
                pending,
                downloading,
                completed,
                failed,
                &recent,
                &source_names,
                &storage_usage,
            )
            .into_string();

            Some(Ok(Event::default()
                .event("dashboard-update")
                .data(fragment)))
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

pub(super) async fn downloads_events_sse(
    _auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.broadcaster.subscribe_invalidate();

    let stream = debounced_broadcast(rx).filter_map(move |()| {
        let pool = state.pool.clone();
        let query = query.clone();
        async move {
            let page_num = query.page.unwrap_or(1).max(1);
            let per_page = parse_downloads_page_size(query.per_page);
            let offset = (page_num - 1) * per_page;
            let search_query = query
                .search
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let status_filter = parse_download_status(query.status.as_deref());

            let (videos_result, count_result, source_names_result) = tokio::join!(
                db::list_videos_paginated(
                    &pool,
                    status_filter.clone(),
                    search_query,
                    per_page,
                    offset
                ),
                db::count_videos(&pool, status_filter, search_query),
                db::get_source_names_for_videos(&pool),
            );

            let videos = match videos_result {
                Ok(data) => data,
                Err(error) => {
                    tracing::warn!(%error, "failed to render downloads SSE fragment");
                    return None;
                }
            };
            let total = count_result.unwrap_or(0);
            let total_pages = if total == 0 {
                1
            } else {
                (total + per_page - 1) / per_page
            };
            let source_names = source_names_result.unwrap_or_default();

            let fragment = downloads_list_markup(
                &videos,
                &source_names,
                page_num,
                total_pages,
                total,
                query.status.as_deref(),
                search_query,
                per_page,
            )
            .into_string();

            Some(Ok(Event::default()
                .event("downloads-update")
                .data(fragment)))
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

pub(super) async fn activity_events_sse(
    _auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.broadcaster.subscribe_activity();

    let stream = debounced_broadcast(rx).filter_map(move |()| {
        let pool = state.pool.clone();
        let query = query.clone();
        async move {
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
                    &pool,
                    per_page,
                    offset,
                    severity_filter.clone(),
                    None,
                    params.source_id,
                    params.search.as_deref(),
                ),
                db::count_activity_events(
                    &pool,
                    severity_filter.clone(),
                    None,
                    params.source_id,
                    params.search.as_deref(),
                )
            );

            let events = match events_result {
                Ok(data) => data,
                Err(error) => {
                    tracing::warn!(%error, "failed to render activity SSE fragment");
                    return None;
                }
            };
            let source_names = load_activity_source_names(&pool).await;
            let video_source_names = db::get_source_names_for_videos(&pool)
                .await
                .unwrap_or_default();
            let total = count_result.unwrap_or(0);
            let total_pages = if total == 0 {
                1
            } else {
                (total + per_page - 1) / per_page
            };
            let fragment = activity_content_markup(
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
            .into_string();

            Some(Ok(Event::default().event("activity-update").data(fragment)))
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

pub(super) async fn sources_events_sse(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.broadcaster.subscribe_invalidate();
    let user_id = auth.user_id;

    let stream = debounced_broadcast(rx).filter_map(move |()| {
        let pool = state.pool.clone();
        async move {
            let profiles = db::list_profiles_for_user(&pool, user_id).await.ok()?;
            let all_sources = db::list_sources(&pool).await.ok()?;

            let profile_ids: std::collections::HashSet<_> = profiles.iter().map(|p| p.id).collect();
            let mut sources: Vec<_> = all_sources
                .into_iter()
                .filter(|s| profile_ids.contains(&s.profile_id))
                .collect();
            sources.sort_by_key(|s| s.display_name().to_lowercase());

            let fragment = sources_list_markup(&sources, None).into_string();
            Some(Ok(Event::default().event("sources-update").data(fragment)))
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

pub(super) async fn source_detail_events_sse(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let source_id = Ulid::from_string(id.trim()).ok();
    let rx = state.broadcaster.subscribe_invalidate();

    let stream = debounced_broadcast(rx).filter_map(move |()| {
        let pool = state.pool.clone();
        async move {
            let source_id = source_id?;
            let source = db::get_source(&pool, source_id).await.ok()?;
            let videos = db::list_videos_for_source(&pool, source_id).await.ok()?;

            let fragment = source_detail_content_markup(&source, &videos).into_string();
            Some(Ok(Event::default()
                .event("source-detail-update")
                .data(fragment)))
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}
