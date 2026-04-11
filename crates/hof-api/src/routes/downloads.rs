//! Download status, progress SSE (JSON), and manual retry endpoints.
//!
//! Provides endpoints to list videos with their download status,
//! stream real-time progress updates via SSE, and manually retry failed downloads.

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{MethodFilter, get, post},
};
use chrono::{DateTime, Utc};
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use ulid::Ulid;
use utoipa::ToSchema;

use hof_core::{
    actors::download_supervisor::{CancelDownload, EnqueueDownload},
    db::{self},
    domain::video::{Video, VideoStatus},
};

use crate::AppState;

/// Build the downloads router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_downloads).post(bulk_retry_downloads))
        .route("/progress", get(get_download_progress))
        .route(
            "/{id}",
            get(get_download).on(MethodFilter::DELETE, delete_download),
        )
        .route("/{id}/cancel", post(cancel_download))
        .route("/{id}/retry", post(retry_download))
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
    path = "/api/v1/downloads",
    tag = "downloads",
    params(
        ("status" = Option<VideoStatus>, Query, description = "Filter by video status"),
        ("source_id" = Option<String>, Query, description = "Filter by source ID")
    ),
    responses(
        (status = 200, description = "List of videos/downloads", body = Vec<VideoResponse>),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn list_downloads(
    State(state): State<AppState>,
    Query(query): Query<ListDownloadsQuery>,
) -> impl IntoResponse {
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

        match db::list_videos_for_source(&state.pool, source_id).await {
            Ok(videos) => {
                // Apply status filter if provided
                let filtered: Vec<VideoResponse> = videos
                    .into_iter()
                    .filter(|v| query.status.as_ref().is_none_or(|s| *s == v.status))
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

    // Otherwise, list all videos with optional status filter
    match db::list_videos(&state.pool, query.status).await {
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

/// Stream download progress updates via Server-Sent Events (SSE).
///
/// This endpoint provides real-time progress updates for all active downloads.
/// Each event contains JSON data with download progress information.
///
/// **Note:** This is a long-lived connection. Clients should handle reconnection
/// if the connection drops.
#[utoipa::path(
    get,
    path = "/api/v1/downloads/progress",
    tag = "downloads",
    responses(
        (status = 200, description = "SSE stream of progress events", content_type = "text/event-stream")
    )
)]
pub async fn get_download_progress(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
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

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Manually retry a failed download.
///
/// This resets the video status to pending and enqueues it for download,
/// bypassing the normal retry schedule.
#[utoipa::path(
    post,
    path = "/api/v1/downloads/{id}/retry",
    tag = "downloads",
    params(
        ("id" = String, Path, description = "Video ID (ULID)")
    ),
    responses(
        (status = 202, description = "Retry enqueued", body = RetryResponse),
        (status = 400, description = "Invalid ID format or video not eligible for retry", body = ErrorResponse),
        (status = 404, description = "Video not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[allow(clippy::too_many_lines)]
pub async fn retry_download(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
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
    path = "/api/v1/downloads/{id}",
    tag = "downloads",
    params(
        ("id" = String, Path, description = "Video ID (ULID)")
    ),
    responses(
        (status = 200, description = "Video details", body = VideoResponse),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 404, description = "Video not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn get_download(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(video_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid video ID format".to_string(),
            }),
        )
            .into_response();
    };

    match db::get_video(&state.pool, video_id).await {
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
    path = "/api/v1/downloads/{id}/cancel",
    tag = "downloads",
    params(
        ("id" = String, Path, description = "Video ID (ULID)")
    ),
    responses(
        (status = 200, description = "Download cancelled", body = CancelResponse),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 404, description = "Video not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn cancel_download(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
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
    path = "/api/v1/downloads/{id}",
    tag = "downloads",
    params(
        ("id" = String, Path, description = "Video ID (ULID)")
    ),
    responses(
        (status = 200, description = "Video deleted", body = DeleteResponse),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 404, description = "Video not found", body = ErrorResponse),
        (status = 409, description = "Video is currently downloading", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn delete_download(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
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
    path = "/api/v1/downloads/retry",
    tag = "downloads",
    responses(
        (status = 202, description = "Bulk retry started", body = BulkRetryResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[allow(clippy::too_many_lines)]
pub async fn bulk_retry_downloads(State(state): State<AppState>) -> impl IntoResponse {
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
