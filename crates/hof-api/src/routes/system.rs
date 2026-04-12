//! System status and control endpoints.
//!
//! Provides endpoints to view system status and trigger manual operations.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use hof_core::{
    actors::{
        cleanup::{GetCleanupStatus, RunCleanup},
        download_supervisor::GetSupervisorStatus,
        scheduler::GetSchedulerStatus,
    },
    domain::api_key::ApiKeyScope,
};

use crate::{
    AppState,
    auth::{ApiErrorResponse, Auth},
};

/// Build the system router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/status", get(get_system_status))
        .route("/cleanup", post(trigger_cleanup))
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Overall system status response.
#[derive(Debug, Serialize, ToSchema)]
pub struct SystemStatusResponse {
    pub scheduler: SchedulerStatusResponse,
    pub downloads: DownloadsStatusResponse,
    pub cleanup: CleanupStatusResponse,
    pub statistics: StatisticsResponse,
    pub timestamp: DateTime<Utc>,
}

/// Scheduler status in system response.
#[derive(Debug, Serialize, ToSchema)]
pub struct SchedulerStatusResponse {
    pub running: bool,
    pub active_indexers: usize,
    pub check_interval_secs: u64,
}

/// Downloads status in system response.
#[derive(Debug, Serialize, ToSchema)]
pub struct DownloadsStatusResponse {
    pub active_downloads: usize,
    pub available_permits: usize,
    pub rate_limit_backoff: u32,
}

/// Cleanup status in system response.
#[derive(Debug, Serialize, ToSchema)]
pub struct CleanupStatusResponse {
    pub running: bool,
    pub global_retention_days: Option<i32>,
    pub cleanup_interval_secs: u64,
    pub last_run_at: Option<DateTime<Utc>>,
}

/// Statistics in system response.
#[derive(Debug, Serialize, ToSchema)]
pub struct StatisticsResponse {
    pub total_videos: i64,
    pub pending_downloads: i64,
    pub downloading: i64,
    pub completed: i64,
    pub failed: i64,
    pub permanently_failed: i64,
    pub skipped: i64,
    pub cleaned: i64,
}

/// Response for cleanup trigger.
#[derive(Debug, Serialize, ToSchema)]
pub struct CleanupTriggerResponse {
    pub message: String,
    pub result: CleanupResultResponse,
}

/// Cleanup result in response.
#[derive(Debug, Serialize, ToSchema)]
pub struct CleanupResultResponse {
    pub retention_cleaned: usize,
    pub quota_cleaned: usize,
    pub temp_files_cleaned: usize,
    pub bytes_freed: i64,
    pub errors: Vec<String>,
}

/// Error response body.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

// ============================================================================
// Handlers
// ============================================================================

/// Get overall system status.
///
/// Returns a comprehensive overview of the system including:
/// - Scheduler status (running, active indexers)
/// - Download supervisor status (active downloads, permits)
/// - Cleanup actor status (last run, retention settings)
/// - Video statistics (counts by status)
#[utoipa::path(
    get,
    path = "/api/v1/system/status",
    tag = "system",
    responses(
        (status = 200, description = "System status", body = SystemStatusResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn get_system_status(State(state): State<AppState>, auth: Auth) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Read) {
        return e.into_response();
    }
    // Get scheduler status
    let scheduler_status = match state.scheduler.ask(GetSchedulerStatus).await {
        Ok(status) => SchedulerStatusResponse {
            running: status.running,
            active_indexers: status.active_indexers,
            check_interval_secs: status.check_interval_secs,
        },
        Err(e) => {
            tracing::error!(error = %e, "Failed to get scheduler status");
            SchedulerStatusResponse {
                running: false,
                active_indexers: 0,
                check_interval_secs: 0,
            }
        }
    };

    // Get download supervisor status
    let downloads_status = match state.supervisor.ask(GetSupervisorStatus).await {
        Ok(status) => DownloadsStatusResponse {
            active_downloads: status.active_downloads,
            available_permits: status.available_permits,
            rate_limit_backoff: status.rate_limit_backoff,
        },
        Err(e) => {
            tracing::error!(error = %e, "Failed to get supervisor status");
            DownloadsStatusResponse {
                active_downloads: 0,
                available_permits: 0,
                rate_limit_backoff: 0,
            }
        }
    };

    // Get cleanup status
    let cleanup_status = match state.cleanup.ask(GetCleanupStatus).await {
        Ok(status) => CleanupStatusResponse {
            running: status.running,
            global_retention_days: status.global_retention_days,
            cleanup_interval_secs: status.cleanup_interval_secs,
            last_run_at: status.last_run_at,
        },
        Err(e) => {
            tracing::error!(error = %e, "Failed to get cleanup status");
            CleanupStatusResponse {
                running: false,
                global_retention_days: None,
                cleanup_interval_secs: 0,
                last_run_at: None,
            }
        }
    };

    // Get video statistics
    let statistics = match get_video_statistics(&state.pool).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "Failed to get video statistics");
            StatisticsResponse {
                total_videos: 0,
                pending_downloads: 0,
                downloading: 0,
                completed: 0,
                failed: 0,
                permanently_failed: 0,
                skipped: 0,
                cleaned: 0,
            }
        }
    };

    (
        StatusCode::OK,
        Json(SystemStatusResponse {
            scheduler: scheduler_status,
            downloads: downloads_status,
            cleanup: cleanup_status,
            statistics,
            timestamp: Utc::now(),
        }),
    )
        .into_response()
}

/// Trigger manual cleanup.
///
/// Runs the cleanup process immediately, which:
/// - Removes videos past their retention period
/// - Enforces storage quotas per profile
/// - Cleans up orphaned temp files
#[utoipa::path(
    post,
    path = "/api/v1/system/cleanup",
    tag = "system",
    responses(
        (status = 200, description = "Cleanup completed", body = CleanupTriggerResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn trigger_cleanup(State(state): State<AppState>, auth: Auth) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }
    match state.cleanup.ask(RunCleanup).await {
        Ok(result) => (
            StatusCode::OK,
            Json(CleanupTriggerResponse {
                message: "Cleanup completed".to_string(),
                result: CleanupResultResponse {
                    retention_cleaned: result.retention_cleaned,
                    quota_cleaned: result.quota_cleaned,
                    temp_files_cleaned: result.temp_files_cleaned,
                    bytes_freed: result.bytes_freed,
                    errors: result.errors,
                },
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to run cleanup");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to run cleanup: {e}"),
                }),
            )
                .into_response()
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get video counts by status.
async fn get_video_statistics(pool: &sqlx::PgPool) -> Result<StatisticsResponse, sqlx::Error> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM videos")
        .fetch_one(pool)
        .await?;

    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM videos WHERE status = 'pending'")
        .fetch_one(pool)
        .await?;

    let downloading: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM videos WHERE status = 'downloading'")
            .fetch_one(pool)
            .await?;

    let completed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM videos WHERE status = 'completed'")
            .fetch_one(pool)
            .await?;

    let failed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM videos WHERE status = 'failed'")
        .fetch_one(pool)
        .await?;

    let permanently_failed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM videos WHERE status = 'permanently_failed'")
            .fetch_one(pool)
            .await?;

    let skipped: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM videos WHERE status = 'skipped'")
        .fetch_one(pool)
        .await?;

    let cleaned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM videos WHERE status = 'cleaned'")
        .fetch_one(pool)
        .await?;

    Ok(StatisticsResponse {
        total_videos: total,
        pending_downloads: pending,
        downloading,
        completed,
        failed,
        permanently_failed,
        skipped,
        cleaned,
    })
}
