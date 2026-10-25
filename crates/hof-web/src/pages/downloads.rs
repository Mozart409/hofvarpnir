//! Split out of `pages/mod.rs`.

use super::*;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct DownloadsQuery {
    pub(super) status: Option<String>,
    pub(super) search: Option<String>,
    pub(super) page: Option<i64>,
    pub(super) per_page: Option<i64>,
}

#[allow(clippy::too_many_lines)]
pub(super) async fn downloads_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
    let page_num = query.page.unwrap_or(1).max(1);
    let per_page = parse_downloads_page_size(query.per_page);
    let offset = (page_num - 1) * per_page;
    let search_query = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let status_filter = parse_download_status(query.status.as_deref());
    let current_status = normalized_download_status(query.status.as_deref());

    let (videos_result, count_result, source_names_result) = tokio::join!(
        db::list_videos_paginated(
            &state.pool,
            status_filter.clone(),
            search_query,
            per_page,
            offset
        ),
        db::count_videos(&state.pool, status_filter, search_query),
        db::get_source_names_for_videos(&state.pool),
    );

    let videos = match videos_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load downloads page");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load downloads page"),
            );
        }
    };

    let total = count_result.unwrap_or(0);
    let total_pages = if total == 0 {
        1
    } else {
        (total + per_page - 1) / per_page
    };
    let source_names = source_names_result.unwrap_or_default();

    let list_url = downloads_list_url(query.status.as_deref(), search_query, page_num, per_page);

    let page = layout_with_flash(
        "Downloads",
        NavItem::Downloads,
        flash,
        html! {
            // Filter & search bar
            section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                div class="flex flex-wrap items-center gap-4" {
                    nav class="flex flex-wrap gap-1" {
                        (download_status_filter_link("all", "All", current_status, search_query, per_page))
                        (download_status_filter_link("pending", "Pending", current_status, search_query, per_page))
                        (download_status_filter_link("awaiting_quality", "Awaiting Quality", current_status, search_query, per_page))
                        (download_status_filter_link("downloading", "Downloading", current_status, search_query, per_page))
                        (download_status_filter_link("completed", "Completed", current_status, search_query, per_page))
                        (download_status_filter_link("failed", "Failed", current_status, search_query, per_page))
                        (download_status_filter_link("cleaned", "Cleaned", current_status, search_query, per_page))
                        (download_status_filter_link("permanently_failed", "Perm. Failed", current_status, search_query, per_page))
                        (download_status_filter_link("skipped", "Skipped", current_status, search_query, per_page))
                    }
                    form
                        method="get"
                        action="/downloads"
                        class="flex gap-2"
                        hx-get="/web/downloads/list"
                        hx-target="#downloads-list"
                        hx-push-url=(downloads_page_url(query.status.as_deref(), search_query, page_num, per_page))
                    {
                        @if let Some(ref status) = query.status {
                            input type="hidden" name="status" value=(status);
                        }
                        // Keep the current page across per_page changes. Editing the
                        // search term narrows the result set, so `data-reset-page`
                        // snaps this back to 1 before the form is serialised.
                        input id="downloads-page-field" type="hidden" name="page" value=(page_num);
                        select
                            name="per_page"
                            class="rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-2 py-1.5 text-sm text-slate-900 dark:text-slate-100"
                        {
                            (page_size_option(25, per_page))
                            (page_size_option(50, per_page))
                            (page_size_option(100, per_page))
                        }
                        input
                            type="text"
                            name="search"
                            placeholder="Search title or channel..."
                            value=(search_query.unwrap_or(""))
                            data-reset-page="downloads-page-field"
                            class="rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-900 dark:text-slate-100";
                        button type="submit"
                            class="rounded-lg bg-slate-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-slate-700"
                        { "Search" }
                        @if search_query.is_some() || query.status.is_some() {
                            a
                                href=(downloads_page_url(None, None, 1, per_page))
                                hx-get=(downloads_list_url(None, None, 1, per_page))
                                hx-target="#downloads-list"
                                hx-push-url=(downloads_page_url(None, None, 1, per_page))
                                class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                            { "Clear" }
                        }
                    }
                }
            }

            div
                hx-ext="sse"
                sse-connect=(downloads_events_url(query.status.as_deref(), search_query, page_num, per_page))
            {
                div
                    id="downloads-list"
                    sse-swap="downloads-update"
                    // `show:none` keeps background SSE refreshes from scrolling the
                    // viewport out from under someone reading mid-list.
                    hx-swap="innerHTML show:none"
                    hx-get=(list_url)
                    hx-trigger="load"
                    hx-target="this"
                {
                    (downloads_list_markup(
                        &videos,
                        &source_names,
                        page_num,
                        total_pages,
                        total,
                        query.status.as_deref(),
                        search_query,
                        per_page,
                    ))
                }
            }
        },
    );

    (StatusCode::OK, page)
}

pub(super) async fn downloads_list_partial(
    _auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
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
            &state.pool,
            status_filter.clone(),
            search_query,
            per_page,
            offset
        ),
        db::count_videos(&state.pool, status_filter, search_query),
        db::get_source_names_for_videos(&state.pool),
    );

    let videos = match videos_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load downloads partial");
            return error_fragment("Failed to load downloads page").into_response();
        }
    };

    let total = count_result.unwrap_or(0);
    let total_pages = if total == 0 {
        1
    } else {
        (total + per_page - 1) / per_page
    };
    let source_names = source_names_result.unwrap_or_default();

    downloads_list_markup(
        &videos,
        &source_names,
        page_num,
        total_pages,
        total,
        query.status.as_deref(),
        search_query,
        per_page,
    )
    .into_response()
}

/// A bulk action submitted from the downloads table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BulkAction {
    Retry,
    Cancel,
    Delete,
}

impl BulkAction {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "retry" => Some(Self::Retry),
            "cancel" => Some(Self::Cancel),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    /// Whether a video in `status` can take this action.
    ///
    /// Mirrors the guards in the single-item handlers; keep the two in step.
    pub(super) const fn allows(self, status: &VideoStatus) -> bool {
        match self {
            Self::Retry => matches!(
                status,
                VideoStatus::Failed | VideoStatus::PermanentlyFailed | VideoStatus::Cleaned
            ),
            Self::Cancel => matches!(
                status,
                VideoStatus::Pending | VideoStatus::AwaitingQuality | VideoStatus::Downloading
            ),
            Self::Delete => matches!(status, VideoStatus::Completed),
        }
    }

    pub(super) const fn past_tense(self) -> &'static str {
        match self {
            Self::Retry => "re-queued",
            Self::Cancel => "cancelled",
            Self::Delete => "deleted",
        }
    }
}

/// Bulk action form payload.
///
/// `ids` is a comma-separated list rather than repeated checkbox fields:
/// `serde_urlencoded` (what `axum::Form` uses) cannot collect repeated keys
/// into a `Vec`, and selection already requires JS to drive the toolbar.
#[derive(Debug, Deserialize)]
pub(super) struct BulkDownloadForm {
    pub(super) action: String,
    #[serde(default)]
    pub(super) ids: String,
}

/// Apply retry/cancel/delete to a selected set of downloads.
///
/// Ineligible and missing ids are skipped rather than failing the batch, and
/// the flash message reports both counts so a partial result is never silently
/// presented as a complete one.
#[allow(clippy::too_many_lines)]
pub(super) async fn bulk_download_action(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
    axum::extract::Form(form): axum::extract::Form<BulkDownloadForm>,
) -> impl IntoResponse {
    let return_url = downloads_return_url(&query);

    let Some(action) = BulkAction::parse(form.action.trim()) else {
        set_flash(&session, "error", "Unknown bulk action").await;
        return Redirect::to(&return_url).into_response();
    };

    let ids: Vec<Ulid> = form
        .ids
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter_map(|value| Ulid::from_string(value).ok())
        .collect();

    if ids.is_empty() {
        set_flash(&session, "error", "No downloads selected").await;
        return Redirect::to(&return_url).into_response();
    }

    let videos = match db::list_videos_by_ids(&state.pool, &ids).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to load videos for bulk action");
            set_flash(&session, "error", "Failed to load selected downloads").await;
            return Redirect::to(&return_url).into_response();
        }
    };

    let requested = ids.len();
    let (eligible, ineligible): (Vec<_>, Vec<_>) = videos
        .into_iter()
        .partition(|video| action.allows(&video.status));
    // Ids that matched no row at all (deleted since the page rendered).
    let missing = requested - eligible.len() - ineligible.len();

    let mut succeeded = 0usize;
    let mut failed = 0usize;

    match action {
        BulkAction::Retry => {
            for video in &eligible {
                if bulk_retry_one(&state, video).await {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
            }
        }
        BulkAction::Cancel => {
            for video in &eligible {
                match state
                    .supervisor
                    .ask(CancelDownload { video_id: video.id })
                    .await
                {
                    Ok(()) => succeeded += 1,
                    Err(error) => {
                        tracing::error!(%error, video_id = %video.id, "bulk cancel failed");
                        failed += 1;
                    }
                }
            }
        }
        BulkAction::Delete => {
            for video in &eligible {
                if bulk_delete_one(&state, video).await {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
            }
        }
    }

    if succeeded > 0 {
        state.broadcaster.invalidate();
    }

    let mut parts = vec![format!("{succeeded} {}", action.past_tense())];
    if !ineligible.is_empty() {
        parts.push(format!("{} not eligible", ineligible.len()));
    }
    if missing > 0 {
        parts.push(format!("{missing} no longer exist"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    let level = if failed > 0 || succeeded == 0 {
        "error"
    } else {
        "success"
    };
    set_flash(&session, level, &parts.join(", ")).await;

    Redirect::to(&return_url).into_response()
}

/// Reset one video to pending and re-enqueue it. Returns whether it worked.
pub(super) async fn bulk_retry_one(
    state: &AppState,
    video: &hof_core::domain::video::Video,
) -> bool {
    if let Err(error) = db::update_video_status(&state.pool, video.id, VideoStatus::Pending).await {
        tracing::error!(%error, video_id = %video.id, "bulk retry: status reset failed");
        return false;
    }

    let Ok(source_ids) = db::get_sources_for_video(&state.pool, video.id).await else {
        tracing::error!(video_id = %video.id, "bulk retry: source lookup failed");
        return false;
    };
    let Some(source_id) = source_ids.first() else {
        tracing::warn!(video_id = %video.id, "bulk retry: video has no linked source");
        return false;
    };
    let Ok(source) = db::get_source(&state.pool, *source_id).await else {
        tracing::error!(video_id = %video.id, "bulk retry: source load failed");
        return false;
    };
    let Ok(profile) = db::get_profile(&state.pool, source.profile_id).await else {
        tracing::error!(video_id = %video.id, "bulk retry: profile load failed");
        return false;
    };
    // Re-read so the enqueued copy carries the pending status just written.
    let Ok(refreshed) = db::get_video(&state.pool, video.id).await else {
        tracing::error!(video_id = %video.id, "bulk retry: video reload failed");
        return false;
    };

    match state
        .supervisor
        .tell(EnqueueDownload {
            video: refreshed,
            profile,
            source,
        })
        .await
    {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(%error, video_id = %video.id, "bulk retry: enqueue failed");
            false
        }
    }
}

/// Remove one completed video's file and mark it cleaned.
pub(super) async fn bulk_delete_one(
    state: &AppState,
    video: &hof_core::domain::video::Video,
) -> bool {
    if let Some(path) = video.file_path.as_ref() {
        // A file already gone from disk should still let the row be cleaned.
        if let Err(error) = tokio::fs::remove_file(path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::error!(%error, video_id = %video.id, "bulk delete: file removal failed");
            return false;
        }
    }

    match db::update_video_status(&state.pool, video.id, VideoStatus::Cleaned).await {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(%error, video_id = %video.id, "bulk delete: status update failed");
            false
        }
    }
}

#[allow(clippy::too_many_lines)]
pub(super) async fn retry_download(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
    let Ok(video_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid video ID provided"),
        )
            .into_response();
    };

    let video = match db::get_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Video not found")).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to load video before retry");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load video"),
            )
                .into_response();
        }
    };

    if !matches!(
        video.status,
        VideoStatus::Failed | VideoStatus::PermanentlyFailed | VideoStatus::Cleaned
    ) {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Only failed, permanently_failed, or cleaned videos can be retried"),
        )
            .into_response();
    }

    if let Err(error) = db::update_video_status(&state.pool, video_id, VideoStatus::Pending).await {
        tracing::error!(%error, "failed to reset video status before retry");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_page("Failed to reset video status"),
        )
            .into_response();
    }

    let source_ids = match db::get_sources_for_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to load source ids for video retry");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load source mapping for video"),
            )
                .into_response();
        }
    };

    let Some(source_id) = source_ids.first() else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Video has no linked sources, cannot retry"),
        )
            .into_response();
    };

    let source = match db::get_source(&state.pool, *source_id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to load source for video retry");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load source for video"),
            )
                .into_response();
        }
    };

    let profile = match db::get_profile(&state.pool, source.profile_id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to load profile for video retry");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load profile for video"),
            )
                .into_response();
        }
    };

    let refreshed_video = match db::get_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to reload video for retry enqueue");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to reload video"),
            )
                .into_response();
        }
    };

    match state
        .supervisor
        .tell(EnqueueDownload {
            video: refreshed_video,
            profile,
            source,
        })
        .await
    {
        Ok(()) => {
            state.broadcaster.invalidate();
            set_flash(&session, "info", "Download re-queued").await;
            Redirect::to(&downloads_return_url(&query)).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to enqueue retry download");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to enqueue retry"),
            )
                .into_response()
        }
    }
}

/// "Download now": release a video held by the quality gate and enqueue it.
#[allow(clippy::too_many_lines)]
pub(super) async fn download_now(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
    let Ok(video_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid video ID provided"),
        )
            .into_response();
    };

    let video = match db::get_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Video not found")).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to load video before download now");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load video"),
            )
                .into_response();
        }
    };

    if video.status != VideoStatus::AwaitingQuality {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Only videos awaiting quality can be downloaded now"),
        )
            .into_response();
    }

    let released = match db::release_from_quality_gate(&state.pool, video_id).await {
        Ok(value) => value,
        Err(db::DbError::NotFound) => {
            return (
                StatusCode::BAD_REQUEST,
                error_page("Only videos awaiting quality can be downloaded now"),
            )
                .into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to release video from the quality gate");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to release video from the quality gate"),
            )
                .into_response();
        }
    };

    let source_ids = match db::get_sources_for_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to load source ids for download now");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load source mapping for video"),
            )
                .into_response();
        }
    };

    let Some(source_id) = source_ids.first() else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Video has no linked sources, cannot download"),
        )
            .into_response();
    };

    let source = match db::get_source(&state.pool, *source_id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to load source for download now");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load source for video"),
            )
                .into_response();
        }
    };

    let profile = match db::get_profile(&state.pool, source.profile_id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to load profile for download now");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load profile for video"),
            )
                .into_response();
        }
    };

    match state
        .supervisor
        .tell(EnqueueDownload {
            video: released,
            profile,
            source,
        })
        .await
    {
        Ok(()) => {
            state.broadcaster.invalidate();
            set_flash(&session, "info", "Download started").await;
            Redirect::to(&downloads_return_url(&query)).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to enqueue download now");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to enqueue download"),
            )
                .into_response()
        }
    }
}

pub(super) async fn cancel_download(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
    let Ok(video_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid video ID provided"),
        )
            .into_response();
    };

    let video = match db::get_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Video not found")).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to load video for cancel");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load video"),
            )
                .into_response();
        }
    };

    if !matches!(
        video.status,
        VideoStatus::Pending | VideoStatus::Downloading
    ) {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Only pending or downloading videos can be cancelled"),
        )
            .into_response();
    }

    let cancel_result = state.supervisor.ask(CancelDownload { video_id }).await;

    match cancel_result {
        Ok(()) => {
            state.broadcaster.invalidate();
            set_flash(&session, "info", "Download cancelled").await;
            Redirect::to(&downloads_return_url(&query)).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to cancel download");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to cancel download"),
            )
                .into_response()
        }
    }
}

pub(super) async fn delete_download(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
    let Ok(video_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid video ID provided"),
        )
            .into_response();
    };

    let video = match db::get_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Video not found")).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to load video for delete");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load video"),
            )
                .into_response();
        }
    };

    if video.status != VideoStatus::Completed {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Only completed videos can be deleted"),
        )
            .into_response();
    }

    // Delete the file from disk
    if let Some(ref file_path) = video.file_path {
        let path = std::path::Path::new(file_path);
        if path.exists()
            && let Err(error) = tokio::fs::remove_file(path).await
        {
            tracing::error!(%error, file_path, "failed to delete video file");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to delete video file from disk"),
            )
                .into_response();
        }
    }

    // Mark as cleaned in DB
    if let Err(error) = db::update_video_status(&state.pool, video_id, VideoStatus::Cleaned).await {
        tracing::error!(%error, "failed to mark video as cleaned");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_page("Failed to update video status"),
        )
            .into_response();
    }

    // Display-only MB figure; precision loss is immaterial below ~9 PB.
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    let size_mb = video.file_size_bytes.unwrap_or(0) as f64 / 1_048_576.0;
    let title = &video.title;
    state
        .broadcaster
        .log_and_broadcast(
            &state.pool,
            ActivityEventType::VideoCleaned,
            ActivitySeverity::Info,
            &format!("Manually deleted \"{title}\" ({size_mb:.1} MB freed)"),
            None,
            Some(video_id),
            None,
        )
        .await;

    set_flash(
        &session,
        "success",
        &format!("\"{title}\" deleted ({size_mb:.1} MB freed)"),
    )
    .await;
    Redirect::to(&downloads_return_url(&query)).into_response()
}

/// Returns an HTML fragment for the system issues banner.
/// If there are no issues, returns an empty response.
pub(super) async fn system_banner(State(state): State<AppState>) -> impl IntoResponse {
    let issues = &state.startup_issues;

    if issues.is_empty() {
        return html! {}.into_response();
    }

    // Find the most severe issue level
    let has_errors = issues.iter().any(|i| i.severity == IssueSeverity::Error);
    let (bg_class, border_class, text_class, icon) = if has_errors {
        (
            "bg-rose-50",
            "border-rose-200",
            "text-rose-800",
            "⚠", // Warning sign
        )
    } else {
        (
            "bg-amber-50",
            "border-amber-200",
            "text-amber-800",
            "⚡", // Lightning for warning
        )
    };

    html! {
        div
            id="system-banner"
            class=(format!("mb-4 rounded-xl border {border_class} {bg_class} p-4 shadow-sm"))
        {
            div class="flex items-start gap-3" {
                span class="text-xl" { (icon) }
                div class="flex-1" {
                    h3 class=(format!("font-semibold {text_class}")) {
                        "System Issues Detected"
                    }
                    ul class=(format!("mt-1 list-inside list-disc text-sm {text_class}")) {
                        @for issue in issues.iter() {
                            li { (issue.message) }
                        }
                    }
                }
            }
        }
    }
    .into_response()
}

pub(super) fn download_status_filter_link(
    value: &str,
    label: &str,
    current: &str,
    search: Option<&str>,
    per_page: i64,
) -> Markup {
    let active = value == current;
    let classes = if active {
        "rounded-full bg-slate-900 dark:bg-slate-100 px-3 py-1 text-xs font-medium text-white dark:text-slate-900"
    } else {
        "rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
    };
    let status = if value == "all" { None } else { Some(value) };
    let href = downloads_page_url(status, search, 1, per_page);
    let hx_get = downloads_list_url(status, search, 1, per_page);

    html! {
        a
            class=(classes)
            href=(href)
            hx-get=(hx_get)
            hx-target="#downloads-list"
            hx-push-url=(href)
        {
            (label)
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
/// Toolbar for acting on the current checkbox selection.
///
/// Hidden until something is selected. The buttons stay disabled unless the
/// selection contains at least one video eligible for that action, so the
/// affordance matches what the server will actually do.
pub(super) fn bulk_action_bar(
    status: Option<&str>,
    search: Option<&str>,
    page_num: i64,
    per_page: i64,
) -> Markup {
    let action_url = downloads_url("/downloads/bulk", status, search, page_num, per_page);

    html! {
        form
            method="post"
            action=(action_url)
            data-bulk-form="1"
            hidden
            class="mt-4 flex flex-wrap items-center gap-2 rounded-xl border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 px-4 py-3"
        {
            input type="hidden" name="ids" data-bulk-ids="1" value="";
            input type="hidden" name="action" data-bulk-action="1" value="";
            span class="text-sm font-medium text-slate-700 dark:text-slate-300" {
                span data-bulk-count="1" { "0" }
                " selected"
            }
            button
                type="submit"
                value="retry"
                data-bulk-button="retry"
                class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-xs font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900 disabled:opacity-40"
            { "Retry selected" }
            button
                type="submit"
                value="cancel"
                data-bulk-button="cancel"
                data-confirm="Cancel the selected downloads?"
                class="rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/50 px-3 py-1.5 text-xs font-medium text-amber-700 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900 disabled:opacity-40"
            { "Cancel selected" }
            button
                type="submit"
                value="delete"
                data-bulk-button="delete"
                data-confirm="Delete the selected videos? Their files will be removed from disk."
                class="rounded-lg border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/50 px-3 py-1.5 text-xs font-medium text-rose-700 dark:text-rose-300 hover:bg-rose-100 dark:hover:bg-rose-900 disabled:opacity-40"
            { "Delete selected" }
            button
                type="button"
                data-bulk-clear="1"
                class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
            { "Clear selection" }
        }
    }
}

/// One download row, including its per-item action buttons.
///
/// The action forms carry the current list state so the post-redirect returns
/// to the same page and filters rather than resetting to the top.
#[allow(clippy::too_many_arguments)]
pub(super) fn downloads_row(
    video: &Video,
    source_names: &HashMap<Ulid, String>,
    status_param: Option<&str>,
    search: Option<&str>,
    page_num: i64,
    per_page: i64,
) -> Markup {
    let action_url = |verb: &str| {
        downloads_url(
            &format!("/downloads/{}/{verb}", video.id),
            status_param,
            search,
            page_num,
            per_page,
        )
    };

    html! {
        tr id=(format!("video-{}", video.id)) {
            td class="px-3 py-2" {
                input
                    type="checkbox"
                    data-bulk-select=(video.id.to_string())
                    data-status=(download_status_slug(&video.status))
                    aria-label=(format!("Select {}", video.title))
                    class="h-4 w-4 rounded border-slate-300 dark:border-slate-600";
            }
            td class="max-w-lg px-3 py-2 text-slate-900 dark:text-slate-100" {
                p class="truncate font-medium" { (video.title) }
                div class="flex items-center gap-2" {
                    p class="truncate text-xs text-slate-500 dark:text-slate-400" { (video.id.to_string()) }
                    (delivered_quality_badge(video))
                }
            }
            td class="max-w-xs px-3 py-2 text-slate-600 dark:text-slate-400" {
                p class="truncate" {
                    @if let Some(name) = source_names.get(&video.id) {
                        (name)
                    } @else {
                        span class="text-slate-400 dark:text-slate-500 italic" { "—" }
                    }
                }
            }
            td class="px-3 py-2 text-slate-600 dark:text-slate-400" { (video.platform) }
            td class="px-3 py-2" {
                (status_badge(&video.status))
                // The quality gate's note: what is available and when it rechecks.
                @if video.status == VideoStatus::AwaitingQuality {
                    @if let Some(note) = &video.last_error {
                        p class="mt-1 text-xs text-slate-500 dark:text-slate-400" { (note) }
                    }
                }
            }
            td class="px-3 py-2 text-slate-600 dark:text-slate-400" { (video.attempts) }
            td class="px-3 py-2" {
                div class="flex gap-2" {
                    @if BulkAction::Retry.allows(&video.status) {
                        form method="post" action=(action_url("retry")) {
                            button class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-xs font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900" type="submit" {
                                "Retry"
                            }
                        }
                    }
                    @if video.status == VideoStatus::AwaitingQuality {
                        form method="post" action=(action_url("download-now")) {
                            button class="rounded-lg border border-emerald-200 dark:border-emerald-800 bg-emerald-50 dark:bg-emerald-900/50 px-3 py-1.5 text-xs font-medium text-emerald-700 dark:text-emerald-300 hover:bg-emerald-100 dark:hover:bg-emerald-900" type="submit" {
                                "Download now"
                            }
                        }
                    }
                    @if BulkAction::Cancel.allows(&video.status) {
                        form method="post" action=(action_url("cancel")) {
                            button
                                class="rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/50 px-3 py-1.5 text-xs font-medium text-amber-700 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900"
                                type="submit"
                                onclick="return confirm('Cancel this download?')"
                            {
                                "Cancel"
                            }
                        }
                    }
                    @if BulkAction::Delete.allows(&video.status) {
                        form method="post" action=(action_url("delete")) {
                            button
                                class="rounded-lg border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/50 px-3 py-1.5 text-xs font-medium text-rose-700 dark:text-rose-300 hover:bg-rose-100 dark:hover:bg-rose-900"
                                type="submit"
                                onclick="return confirm('Delete this video? The file will be removed from disk.')"
                            {
                                "Delete"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn downloads_list_markup(
    videos: &[Video],
    source_names: &HashMap<Ulid, String>,
    page_num: i64,
    total_pages: i64,
    total: i64,
    status: Option<&str>,
    search: Option<&str>,
    per_page: i64,
) -> Markup {
    let current_status = normalized_download_status(status);
    let status_param = if current_status == "all" {
        None
    } else {
        Some(current_status)
    };

    html! {
        section class="mt-4 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" {
                "Downloads"
                span class="ml-2 text-sm font-normal text-slate-500 dark:text-slate-400" {
                    "(" (total) " results)"
                }
            }
            @if videos.is_empty() {
                p class="mt-3 text-sm text-slate-500 dark:text-slate-400" { "No downloads match your filters." }
            } @else {
                (bulk_action_bar(status_param, search, page_num, per_page))
                div class="mt-4 overflow-x-auto" {
                    table class="min-w-full divide-y divide-slate-200 text-sm" data-bulk-table="1" {
                        thead class="bg-slate-50 dark:bg-slate-800" {
                            tr {
                                th class="w-8 px-3 py-2 text-left" {
                                    input
                                        type="checkbox"
                                        data-bulk-select-all="1"
                                        aria-label="Select all downloads on this page"
                                        class="h-4 w-4 rounded border-slate-300 dark:border-slate-600";
                                }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Title" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Source" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Platform" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Status" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Attempts" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Actions" }
                            }
                        }
                        tbody class="divide-y divide-slate-100 dark:divide-slate-700 bg-white dark:bg-slate-900" {
                            @for video in videos {
                                (downloads_row(video, source_names, status_param, search, page_num, per_page))
                            }
                        }
                    }
                }
            }

            @if total_pages > 1 {
                nav class="mt-6 flex items-center justify-center gap-2" {
                    @if page_num > 1 {
                        a
                            class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                            href=(downloads_page_url(status_param, search, page_num - 1, per_page))
                            hx-get=(downloads_list_url(status_param, search, page_num - 1, per_page))
                            hx-target="#downloads-list"
                            hx-push-url=(downloads_page_url(status_param, search, page_num - 1, per_page))
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
                            href=(downloads_page_url(status_param, search, page_num + 1, per_page))
                            hx-get=(downloads_list_url(status_param, search, page_num + 1, per_page))
                            hx-target="#downloads-list"
                            hx-push-url=(downloads_page_url(status_param, search, page_num + 1, per_page))
                        {
                            "Next"
                        }
                    }
                }
            }
        }
    }
}
