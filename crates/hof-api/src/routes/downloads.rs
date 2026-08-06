//! Download status, progress SSE (JSON), and manual retry endpoints.
//!
//! Provides endpoints to list videos with their download status,
//! stream real-time progress updates via SSE, and manually retry failed downloads.

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use chrono::{DateTime, Utc};
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use ulid::Ulid;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use hof_core::{
    actors::download_supervisor::{CancelDownload, EnqueueDownload},
    db::{self},
    domain::{
        api_key::ApiKeyScope,
        profile::{OutputPreset, Quality},
        source::SourceType,
        video::{Video, VideoContext, VideoPendingDeletion, VideoStatus},
    },
};

use crate::{
    AppState,
    auth::{ApiErrorResponse, Auth},
};

/// Build the downloads router.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_downloads, bulk_retry_downloads))
        .routes(routes!(bulk_download_action))
        .routes(routes!(list_pending_deletion))
        .routes(routes!(get_download_progress))
        .routes(routes!(get_download, delete_download))
        .routes(routes!(cancel_download))
        .routes(routes!(retry_download))
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Query parameters for listing downloads.
#[derive(Debug, Deserialize)]
pub struct ListDownloadsQuery {
    /// Filter by status (optional).
    pub status: Option<VideoStatus>,
    /// Filter by source ID (optional).
    pub source_id: Option<String>,
}

/// Query parameters for listing videos pending retention deletion.
#[derive(Debug, Deserialize)]
pub struct ListPendingDeletionQuery {
    /// Maximum number of videos to return (default: 50, clamped to 1..=200).
    #[serde(default = "default_pending_deletion_limit")]
    pub limit: i64,
    /// Only include videos scheduled for deletion within this many days (optional).
    pub within_days: Option<i32>,
    /// Filter to videos with at least one source under this profile (optional).
    pub profile_id: Option<String>,
}

const fn default_pending_deletion_limit() -> i64 {
    50
}

/// Response body for a video scheduled for retention deletion.
#[derive(Debug, Serialize, ToSchema)]
pub struct PendingDeletionResponse {
    /// The video itself.
    pub video: VideoResponse,
    /// When the video is scheduled to be deleted by the retention cleanup.
    pub scheduled_deletion_at: DateTime<Utc>,
    /// The effective retention in days governing the scheduled deletion.
    pub effective_retention_days: i32,
}

impl From<VideoPendingDeletion> for PendingDeletionResponse {
    fn from(v: VideoPendingDeletion) -> Self {
        Self {
            video: v.video.into(),
            scheduled_deletion_at: v.scheduled_deletion_at,
            effective_retention_days: v.effective_retention_days,
        }
    }
}

/// Response body for a video/download.
#[derive(Debug, Serialize, ToSchema)]
pub struct VideoResponse {
    pub id: String,
    pub platform: String,
    pub platform_video_id: String,
    pub title: String,
    pub description: Option<String>,
    pub duration_secs: Option<i64>,
    pub published_at: Option<DateTime<Utc>>,
    pub thumbnail_url: Option<String>,
    pub status: VideoStatus,
    pub attempts: i32,
    pub next_retry: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_error_code: Option<String>,
    pub file_path: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub downloaded_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    // --- Source/profile context (additive; `None` when the video has no
    // linked source, e.g. the source was deleted). ---
    /// ID of the source this video was downloaded from.
    pub source_id: Option<String>,
    /// The source's channel/playlist URL.
    pub source_url: Option<String>,
    /// Whether the source is a channel or a playlist.
    pub source_type: Option<SourceType>,
    /// User-provided custom name for the source, if set.
    pub source_custom_name: Option<String>,
    /// The effective display name for the source: `custom_name`, falling
    /// back to `channel_title`, then the source URL. Matches the name shown
    /// in the web UI.
    pub source_display_name: Option<String>,
    /// Platform-specific channel ID (e.g. `YouTube` channel ID).
    pub source_channel_id: Option<String>,
    /// Channel title as reported by the platform.
    pub source_channel_title: Option<String>,
    /// URL to the channel's thumbnail/avatar image.
    pub source_channel_thumbnail_url: Option<String>,
    /// ID of the download profile governing this source.
    pub profile_id: Option<String>,
    pub profile_name: Option<String>,
    pub profile_quality: Option<Quality>,
    pub profile_output_preset: Option<OutputPreset>,
}

impl From<Video> for VideoResponse {
    fn from(v: Video) -> Self {
        let last_error_code = v.last_error.as_deref().and_then(extract_error_code);

        Self {
            id: v.id.to_string(),
            platform: v.platform,
            platform_video_id: v.platform_video_id,
            title: v.title,
            description: v.description,
            duration_secs: v.duration_secs,
            published_at: v.published_at,
            thumbnail_url: v.thumbnail_url,
            status: v.status,
            attempts: v.attempts,
            next_retry: v.next_retry,
            last_error: v.last_error,
            last_error_code,
            file_path: v.file_path,
            file_size_bytes: v.file_size_bytes,
            downloaded_at: v.downloaded_at,
            created_at: v.created_at,
            updated_at: v.updated_at,
            source_id: None,
            source_url: None,
            source_type: None,
            source_custom_name: None,
            source_display_name: None,
            source_channel_id: None,
            source_channel_title: None,
            source_channel_thumbnail_url: None,
            profile_id: None,
            profile_name: None,
            profile_quality: None,
            profile_output_preset: None,
        }
    }
}

impl From<VideoContext> for VideoResponse {
    fn from(ctx: VideoContext) -> Self {
        let source_display_name = ctx.source_display_name().map(ToString::to_string);
        let VideoContext {
            video: v,
            source_id,
            source_url,
            source_type,
            source_custom_name,
            source_channel_id,
            source_channel_title,
            source_channel_thumbnail_url,
            profile_id,
            profile_name,
            profile_quality,
            profile_output_preset,
        } = ctx;

        let last_error_code = v.last_error.as_deref().and_then(extract_error_code);

        Self {
            id: v.id.to_string(),
            platform: v.platform,
            platform_video_id: v.platform_video_id,
            title: v.title,
            description: v.description,
            duration_secs: v.duration_secs,
            published_at: v.published_at,
            thumbnail_url: v.thumbnail_url,
            status: v.status,
            attempts: v.attempts,
            next_retry: v.next_retry,
            last_error: v.last_error,
            last_error_code,
            file_path: v.file_path,
            file_size_bytes: v.file_size_bytes,
            downloaded_at: v.downloaded_at,
            created_at: v.created_at,
            updated_at: v.updated_at,
            source_id: source_id.map(|id| id.to_string()),
            source_url,
            source_type,
            source_custom_name,
            source_display_name,
            source_channel_id,
            source_channel_title,
            source_channel_thumbnail_url,
            profile_id: profile_id.map(|id| id.to_string()),
            profile_name,
            profile_quality,
            profile_output_preset,
        }
    }
}

fn extract_error_code(error: &str) -> Option<String> {
    if !error.starts_with('[') {
        return None;
    }

    let closing = error.find(']')?;
    if closing <= 1 {
        return None;
    }

    Some(error[1..closing].to_string())
}

/// Response for retry endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetryResponse {
    pub message: String,
    pub video_id: String,
}

/// Response for bulk retry endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct BulkRetryResponse {
    pub message: String,
    pub retried_count: usize,
    pub video_ids: Vec<String>,
}

/// The action to apply to a selection of downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BulkActionKind {
    Retry,
    Cancel,
    Delete,
}

impl BulkActionKind {
    /// Whether a video in `status` can take this action.
    const fn allows(self, status: &VideoStatus) -> bool {
        match self {
            Self::Retry => matches!(
                status,
                VideoStatus::Failed | VideoStatus::PermanentlyFailed | VideoStatus::Cleaned
            ),
            Self::Cancel => matches!(status, VideoStatus::Pending | VideoStatus::Downloading),
            Self::Delete => matches!(status, VideoStatus::Completed),
        }
    }
}

/// Request body for applying an action to a specific set of downloads.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkActionRequest {
    /// The action to apply.
    pub action: BulkActionKind,
    /// ULIDs of the videos to act on.
    pub video_ids: Vec<String>,
}

/// Outcome of a bulk action, reported per category.
///
/// A bulk call is best-effort: ineligible and unknown ids are skipped rather
/// than failing the batch, so the counts are the only way for a caller to tell
/// a complete result from a partial one.
#[derive(Debug, Serialize, ToSchema)]
pub struct BulkActionResponse {
    pub message: String,
    /// Videos the action was applied to successfully.
    pub succeeded: Vec<String>,
    /// Videos whose current status does not permit the action.
    pub ineligible: Vec<String>,
    /// Requested ids that matched no video.
    pub not_found: Vec<String>,
    /// Videos that were eligible but errored during the action.
    pub failed: Vec<String>,
}

/// Response for cancel endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct CancelResponse {
    pub message: String,
    pub video_id: String,
}

/// Response for delete endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteResponse {
    pub message: String,
    pub video_id: String,
}

/// Progress event data (matches `DownloadProgress`).
#[derive(Debug, Serialize, ToSchema)]
pub struct ProgressEvent {
    pub video_id: String,
    pub platform_video_id: String,
    pub percent: f64,
    pub speed: Option<String>,
    pub eta: Option<String>,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

/// Error response body.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

// ============================================================================
// Handlers
// ============================================================================

/// List videos with their download status.
///
/// Optionally filter by status or source ID.
#[utoipa::path(
    get,
    path = "",
    tag = "downloads",
    params(
        ("status" = Option<VideoStatus>, Query, description = "Filter by video status"),
        ("source_id" = Option<String>, Query, description = "Filter by source ID")
    ),
    responses(
        (status = 200, description = "List of videos/downloads", body = Vec<VideoResponse>),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn list_downloads(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<ListDownloadsQuery>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Read) {
        return e.into_response();
    }

    // If filtering by source_id, use that query
    if let Some(source_id_str) = query.source_id {
        let Ok(source_id) = Ulid::from_string(&source_id_str) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid source_id format".to_string(),
                }),
            )
                .into_response();
        };

        match db::list_videos_for_source_with_context(&state.pool, source_id).await {
            Ok(videos) => {
                // Apply status filter if provided
                let filtered: Vec<VideoResponse> = videos
                    .into_iter()
                    .filter(|ctx| query.status.as_ref().is_none_or(|s| *s == ctx.video.status))
                    .map(Into::into)
                    .collect();
                return (StatusCode::OK, Json(filtered)).into_response();
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to list videos for source");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "Failed to list downloads".to_string(),
                    }),
                )
                    .into_response();
            }
        }
    }

    // Otherwise, list all videos with optional status filter.
    // `list_videos_with_context` is a single query (LEFT JOIN LATERAL for the
    // first-linked source + its profile) -- no per-row N+1 lookups.
    match db::list_videos_with_context(&state.pool, query.status).await {
        Ok(videos) => {
            let responses: Vec<VideoResponse> = videos.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(responses)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to list videos");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to list downloads".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// List completed videos scheduled for retention deletion, soonest first.
///
/// Previews which videos the retention cleanup will delete and when, based on
/// each video's effective retention (source ?? profile ?? global `RETENTION_DAYS`).
/// Optionally filter by a deletion window (`within_days`) or `profile_id`.
#[utoipa::path(
    get,
    path = "/pending-deletion",
    tag = "downloads",
    params(
        ("limit" = Option<i64>, Query, description = "Max videos to return (default 50, max 200)"),
        ("within_days" = Option<i32>, Query, description = "Only videos deleted within this many days"),
        ("profile_id" = Option<String>, Query, description = "Filter by profile ID")
    ),
    responses(
        (status = 200, description = "Videos scheduled for deletion, soonest first", body = Vec<PendingDeletionResponse>),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn list_pending_deletion(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<ListPendingDeletionQuery>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Read) {
        return e.into_response();
    }

    let profile_id = match query.profile_id {
        Some(ref s) => match Ulid::from_string(s) {
            Ok(id) => Some(id),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: "Invalid profile_id format".to_string(),
                    }),
                )
                    .into_response();
            }
        },
        None => None,
    };

    let limit = query.limit.clamp(1, 200);

    match db::list_videos_pending_deletion(
        &state.pool,
        state.global_retention_days,
        profile_id,
        query.within_days,
        limit,
    )
    .await
    {
        Ok(videos) => {
            let responses: Vec<PendingDeletionResponse> =
                videos.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(responses)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to list videos pending deletion");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to list videos pending deletion".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Stream download progress updates via Server-Sent Events (SSE).
///
/// This endpoint provides real-time progress updates for all active downloads.
/// Each event contains JSON data with download progress information.
///
/// **Note:** This is a long-lived connection. Clients should handle reconnection
/// if the connection drops.
///
/// # Errors
///
/// Returns `ApiError::Forbidden` if the authentication lacks the `Read` scope.
#[utoipa::path(
    get,
    path = "/progress",
    tag = "downloads",
    responses(
        (status = 200, description = "SSE stream of progress events", content_type = "text/event-stream"),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse)
    )
)]
pub async fn get_download_progress(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, crate::auth::ApiError> {
    auth.require_scope(ApiKeyScope::Read)?;

    // Subscribe to the progress broadcast channel
    let rx = state.progress_tx.subscribe();

    // Convert broadcast receiver to a stream
    let stream = BroadcastStream::new(rx).filter_map(|result| async move {
        match result {
            Ok(progress) => {
                let event = ProgressEvent {
                    video_id: progress.video_id.to_string(),
                    platform_video_id: progress.platform_video_id,
                    percent: progress.percent,
                    speed: progress.speed,
                    eta: progress.eta,
                    downloaded_bytes: progress.downloaded_bytes,
                    total_bytes: progress.total_bytes,
                };

                match serde_json::to_string(&event) {
                    Ok(json) => Some(Ok(Event::default().data(json).event("progress"))),
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to serialize progress event");
                        None
                    }
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "Broadcast stream error (likely lagged)");
                None
            }
        }
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

/// Manually retry a failed download.
///
/// This resets the video status to pending and enqueues it for download,
/// bypassing the normal retry schedule.
#[utoipa::path(
    post,
    path = "/{id}/retry",
    tag = "downloads",
    params(
        ("id" = String, Path, description = "Video ID (ULID)")
    ),
    responses(
        (status = 202, description = "Retry enqueued", body = RetryResponse),
        (status = 400, description = "Invalid ID format or video not eligible for retry", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Video not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[allow(clippy::too_many_lines)]
pub async fn retry_download(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let Ok(video_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid video ID format".to_string(),
            }),
        )
            .into_response();
    };

    // Get the video
    let video = match db::get_video(&state.pool, video_id).await {
        Ok(v) => v,
        Err(db::DbError::NotFound) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "Video not found".to_string(),
                }),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to get video");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get video".to_string(),
                }),
            )
                .into_response();
        }
    };

    // Check if video is eligible for retry
    match video.status {
        VideoStatus::Failed | VideoStatus::PermanentlyFailed | VideoStatus::Cleaned => {
            // Eligible for retry
        }
        VideoStatus::Pending | VideoStatus::Downloading => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Video is already pending or downloading".to_string(),
                }),
            )
                .into_response();
        }
        VideoStatus::Completed => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Video has already been downloaded successfully".to_string(),
                }),
            )
                .into_response();
        }
        VideoStatus::Skipped => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Video was skipped; cannot retry".to_string(),
                }),
            )
                .into_response();
        }
    }

    // Reset video status to pending
    if let Err(e) = db::update_video_status(&state.pool, video_id, VideoStatus::Pending).await {
        tracing::error!(error = %e, "Failed to reset video status");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Failed to reset video status".to_string(),
            }),
        )
            .into_response();
    }

    // Get the profile for this video (through source linkage)
    let source_ids = match db::get_sources_for_video(&state.pool, video_id).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::error!(error = %e, "Failed to get sources for video");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get video sources".to_string(),
                }),
            )
                .into_response();
        }
    };

    if source_ids.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Video has no linked sources".to_string(),
            }),
        )
            .into_response();
    }

    // Get the first source and its profile
    let source = match db::get_source(&state.pool, source_ids[0]).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "Failed to get source");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get source".to_string(),
                }),
            )
                .into_response();
        }
    };

    let profile = match db::get_profile(&state.pool, source.profile_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to get profile");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get profile".to_string(),
                }),
            )
                .into_response();
        }
    };

    // Re-fetch the video with updated status
    let video = match db::get_video(&state.pool, video_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "Failed to get updated video");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get video".to_string(),
                }),
            )
                .into_response();
        }
    };

    // Enqueue the download
    match state
        .supervisor
        .tell(EnqueueDownload {
            video,
            profile,
            source,
        })
        .await
    {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(RetryResponse {
                message: "Retry enqueued".to_string(),
                video_id: video_id.to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to enqueue download");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to enqueue download".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Get a single video/download by ID.
///
/// Returns detailed information about a specific video including
/// download status, error messages, and file information.
#[utoipa::path(
    get,
    path = "/{id}",
    tag = "downloads",
    params(
        ("id" = String, Path, description = "Video ID (ULID)")
    ),
    responses(
        (status = 200, description = "Video details", body = VideoResponse),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Video not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn get_download(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Read) {
        return e.into_response();
    }

    let Ok(video_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid video ID format".to_string(),
            }),
        )
            .into_response();
    };

    match db::get_video_with_context(&state.pool, video_id).await {
        Ok(video) => {
            let response: VideoResponse = video.into();
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(db::DbError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Video not found".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to get video");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get video".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Cancel an active download.
///
/// Stops an in-progress download and marks the video as failed.
/// The video can be retried later using the retry endpoint.
#[utoipa::path(
    post,
    path = "/{id}/cancel",
    tag = "downloads",
    params(
        ("id" = String, Path, description = "Video ID (ULID)")
    ),
    responses(
        (status = 200, description = "Download cancelled", body = CancelResponse),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Video not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn cancel_download(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let Ok(video_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid video ID format".to_string(),
            }),
        )
            .into_response();
    };

    // Verify video exists
    match db::get_video(&state.pool, video_id).await {
        Ok(video) => {
            // Check if video is actually downloading
            if video.status != VideoStatus::Downloading {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: "Video is not currently downloading".to_string(),
                    }),
                )
                    .into_response();
            }
        }
        Err(db::DbError::NotFound) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "Video not found".to_string(),
                }),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to get video");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get video".to_string(),
                }),
            )
                .into_response();
        }
    }

    // Send cancel message to supervisor
    match state.supervisor.ask(CancelDownload { video_id }).await {
        Ok(()) => (
            StatusCode::OK,
            Json(CancelResponse {
                message: "Download cancelled".to_string(),
                video_id: video_id.to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to cancel download");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to cancel download: {e}"),
                }),
            )
                .into_response()
        }
    }
}

/// Delete a video and its file.
///
/// Removes the video record from the database and deletes the
/// downloaded file from disk. This action cannot be undone.
#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "downloads",
    params(
        ("id" = String, Path, description = "Video ID (ULID)")
    ),
    responses(
        (status = 200, description = "Video deleted", body = DeleteResponse),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Video not found", body = ErrorResponse),
        (status = 409, description = "Video is currently downloading", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn delete_download(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Delete) {
        return e.into_response();
    }

    let Ok(video_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid video ID format".to_string(),
            }),
        )
            .into_response();
    };

    // Get the video
    let video = match db::get_video(&state.pool, video_id).await {
        Ok(v) => v,
        Err(db::DbError::NotFound) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "Video not found".to_string(),
                }),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to get video");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get video".to_string(),
                }),
            )
                .into_response();
        }
    };

    // Don't allow deleting while downloading
    if video.status == VideoStatus::Downloading {
        return (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "Cannot delete video while it is downloading. Cancel the download first."
                    .to_string(),
            }),
        )
            .into_response();
    }

    // Delete the file if it exists
    if let Some(file_path) = &video.file_path {
        let path = std::path::Path::new(file_path);
        if path.exists()
            && let Err(e) = tokio::fs::remove_file(path).await
        {
            tracing::warn!(error = %e, file_path, "Failed to delete video file");
        }
    }

    // Delete the video from database
    match db::delete_video(&state.pool, video_id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(DeleteResponse {
                message: "Video deleted".to_string(),
                video_id: video_id.to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to delete video");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to delete video".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Bulk retry all failed downloads.
///
/// Resets all failed and permanently failed videos to pending status
/// and enqueues them for download. Useful for recovering from network
/// outages or temporary `YouTube` blocks.
#[utoipa::path(
    post,
    path = "",
    tag = "downloads",
    responses(
        (status = 202, description = "Bulk retry started", body = BulkRetryResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[allow(clippy::too_many_lines)]
pub async fn bulk_retry_downloads(State(state): State<AppState>, auth: Auth) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }
    // Get all failed and permanently failed videos
    let failed_videos = match db::list_videos(&state.pool, Some(VideoStatus::Failed)).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "Failed to list failed videos");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to list failed videos".to_string(),
                }),
            )
                .into_response();
        }
    };

    let permanently_failed =
        match db::list_videos(&state.pool, Some(VideoStatus::PermanentlyFailed)).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "Failed to list permanently failed videos");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "Failed to list permanently failed videos".to_string(),
                    }),
                )
                    .into_response();
            }
        };

    let cleaned = match db::list_videos(&state.pool, Some(VideoStatus::Cleaned)).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "Failed to list cleaned videos");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to list cleaned videos".to_string(),
                }),
            )
                .into_response();
        }
    };

    let all_videos: Vec<Video> = failed_videos
        .into_iter()
        .chain(permanently_failed)
        .chain(cleaned)
        .collect();

    let mut retried_ids = Vec::new();

    for video in all_videos {
        // Reset status to pending
        if let Err(e) = db::update_video_status(&state.pool, video.id, VideoStatus::Pending).await {
            tracing::warn!(error = %e, video_id = %video.id, "Failed to reset video status");
            continue;
        }

        // Get the source(s) for this video
        let source_ids = match db::get_sources_for_video(&state.pool, video.id).await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, video_id = %video.id, "Failed to get sources for video");
                continue;
            }
        };

        if source_ids.is_empty() {
            tracing::warn!(video_id = %video.id, "Video has no linked sources, skipping");
            continue;
        }

        // Get the first source and its profile
        let source = match db::get_source(&state.pool, source_ids[0]).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, video_id = %video.id, "Failed to get source");
                continue;
            }
        };

        let profile = match db::get_profile(&state.pool, source.profile_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, video_id = %video.id, "Failed to get profile");
                continue;
            }
        };

        // Re-fetch the video with updated status
        let video = match db::get_video(&state.pool, video.id).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, video_id = %video.id, "Failed to get updated video");
                continue;
            }
        };

        let video_id = video.id;

        // Enqueue the download
        if let Err(e) = state
            .supervisor
            .tell(EnqueueDownload {
                video,
                profile,
                source,
            })
            .await
        {
            tracing::warn!(error = %e, %video_id, "Failed to enqueue download");
            continue;
        }

        retried_ids.push(video_id);
    }

    (
        StatusCode::ACCEPTED,
        Json(BulkRetryResponse {
            message: format!("Retrying {} downloads", retried_ids.len()),
            retried_count: retried_ids.len(),
            video_ids: retried_ids.iter().map(Ulid::to_string).collect(),
        }),
    )
        .into_response()
}

/// Apply an action to a specific set of downloads.
///
/// Unlike `bulk_retry_downloads`, which retries everything currently failed,
/// this acts only on the ids supplied. Ineligible and unknown ids are reported
/// back rather than failing the request.
#[utoipa::path(
    post,
    path = "/bulk",
    tag = "downloads",
    request_body = BulkActionRequest,
    responses(
        (status = 202, description = "Bulk action applied", body = BulkActionResponse),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn bulk_download_action(
    State(state): State<AppState>,
    auth: Auth,
    Json(payload): Json<BulkActionRequest>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    if payload.video_ids.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "video_ids must not be empty".to_string(),
            }),
        )
            .into_response();
    }

    // Malformed ids are reported as not-found rather than rejecting the batch.
    let mut not_found: Vec<String> = Vec::new();
    let mut ids: Vec<Ulid> = Vec::new();
    for raw in &payload.video_ids {
        match Ulid::from_string(raw.trim()) {
            Ok(id) => ids.push(id),
            Err(_) => not_found.push(raw.clone()),
        }
    }

    let videos = match db::list_videos_by_ids(&state.pool, &ids).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "Failed to load videos for bulk action");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to load selected downloads".to_string(),
                }),
            )
                .into_response();
        }
    };

    for id in &ids {
        if !videos.iter().any(|video| video.id == *id) {
            not_found.push(id.to_string());
        }
    }

    let mut succeeded: Vec<String> = Vec::new();
    let mut ineligible: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    for video in videos {
        if !payload.action.allows(&video.status) {
            ineligible.push(video.id.to_string());
            continue;
        }

        let id = video.id.to_string();
        let ok = match payload.action {
            BulkActionKind::Retry => retry_one(&state, video).await,
            BulkActionKind::Cancel => state
                .supervisor
                .ask(CancelDownload { video_id: video.id })
                .await
                .is_ok(),
            BulkActionKind::Delete => delete_one(&state, video).await,
        };

        if ok {
            succeeded.push(id);
        } else {
            failed.push(id);
        }
    }

    (
        StatusCode::ACCEPTED,
        Json(BulkActionResponse {
            message: format!(
                "{} succeeded, {} ineligible, {} not found, {} failed",
                succeeded.len(),
                ineligible.len(),
                not_found.len(),
                failed.len()
            ),
            succeeded,
            ineligible,
            not_found,
            failed,
        }),
    )
        .into_response()
}

/// Reset one video to pending and re-enqueue it.
async fn retry_one(state: &AppState, video: Video) -> bool {
    let video_id = video.id;

    if let Err(e) = db::update_video_status(&state.pool, video_id, VideoStatus::Pending).await {
        tracing::warn!(error = %e, %video_id, "bulk retry: status reset failed");
        return false;
    }

    let Ok(source_ids) = db::get_sources_for_video(&state.pool, video_id).await else {
        return false;
    };
    let Some(source_id) = source_ids.first() else {
        tracing::warn!(%video_id, "bulk retry: video has no linked source");
        return false;
    };
    let Ok(source) = db::get_source(&state.pool, *source_id).await else {
        return false;
    };
    let Ok(profile) = db::get_profile(&state.pool, source.profile_id).await else {
        return false;
    };
    // Re-read so the enqueued copy carries the pending status just written.
    let Ok(refreshed) = db::get_video(&state.pool, video_id).await else {
        return false;
    };

    state
        .supervisor
        .tell(EnqueueDownload {
            video: refreshed,
            profile,
            source,
        })
        .await
        .is_ok()
}

/// Remove one completed video's file and mark it cleaned.
async fn delete_one(state: &AppState, video: Video) -> bool {
    if let Some(path) = video.file_path.as_ref() {
        // A file already gone from disk should still let the row be cleaned.
        if let Err(e) = tokio::fs::remove_file(path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %e, video_id = %video.id, "bulk delete: file removal failed");
            return false;
        }
    }

    db::update_video_status(&state.pool, video.id, VideoStatus::Cleaned)
        .await
        .is_ok()
}
