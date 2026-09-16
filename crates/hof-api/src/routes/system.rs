//! System status and control endpoints.
//!
//! Provides endpoints to view system status and trigger manual operations.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use hof_core::{
    actors::{
        cleanup::{GetCleanupStatus, RunCleanup},
        download_supervisor::GetSupervisorStatus,
        root_supervisor::{RestartActor, SupervisedActor},
        scheduler::GetSchedulerStatus,
    },
    domain::api_key::ApiKeyScope,
};

use crate::{
    AppState,
    auth::{ApiErrorResponse, Auth},
    routes::settings::{DrainStatusResponse, PauseSummaryResponse},
};

/// Build the system router.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_system_status))
        .routes(routes!(trigger_cleanup))
        .routes(routes!(restart_actor))
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
    /// Pause state for indexing and downloads (see ADR-0003).
    pub pause: PauseSummaryResponse,
    /// Drain progress (see ADR-0004).
    pub drain: DrainStatusResponse,
    pub timestamp: DateTime<Utc>,
}

/// Scheduler status in system response.
#[derive(Debug, Serialize, ToSchema)]
pub struct SchedulerStatusResponse {
    pub running: bool,
    pub active_indexers: usize,
    pub check_interval_secs: u64,
    /// Effective per-tick indexer cap currently in force.
    pub max_indexers_per_tick: u32,
}

/// Downloads status in system response.
///
/// # Why the counts are nullable
///
/// Every count here comes from an `ask` to the download supervisor, and that
/// ask fails outright when the actor is dead. This response used to fall back
/// to all-zeros in that case, which cost a 27-hour outage: `available_permits:
/// 0` is also exactly what a leaked semaphore permit looks like, so the
/// investigation chased a phantom permit leak while the actor had simply died.
/// A dead actor now reports `supervisor_reachable: false` and `null` counts —
/// "we do not know" is a different fact from "zero", and only one of them is
/// true here.
///
/// In the healthy case the JSON is unchanged: every count is a number, exactly
/// as before.
#[derive(Debug, Serialize, ToSchema)]
pub struct DownloadsStatusResponse {
    /// Whether the download supervisor answered. When `false`, every field
    /// sourced from the actor below is `null` and the supervisor needs
    /// restarting (`POST /api/v1/system/actors/download_supervisor/restart`).
    pub supervisor_reachable: bool,
    pub active_downloads: Option<usize>,
    /// Videos with an in-flight dispatch reservation that have not yet
    /// registered a worker. Needed alongside `active_downloads` to judge
    /// drain quiescence: `active_downloads` alone can read zero while
    /// downloads are about to start.
    pub dispatching: Option<usize>,
    pub available_permits: Option<usize>,
    pub rate_limit_backoff: Option<u32>,
    /// Effective download concurrency cap currently in force. Read from the
    /// runtime settings watch channel rather than the actor, so it stays
    /// populated even when the supervisor is unreachable.
    pub max_concurrent_downloads: u32,
    /// While set and in the future, the supervisor is sitting out a database
    /// backoff and dispatching nothing. Distinct from a dead actor: this is
    /// the recoverable case the supervisor handles itself.
    pub db_backoff_until: Option<DateTime<Utc>>,
    /// Consecutive database failures behind `db_backoff_until`. `Some(0)` is
    /// the healthy steady state; `None` means the supervisor was unreachable.
    pub consecutive_db_failures: Option<u32>,
    /// Text of the most recent database error the supervisor saw, so an
    /// operator does not need log access to tell a connection-pool exhaustion
    /// from a migration mismatch.
    pub last_db_error: Option<String>,
}

impl DownloadsStatusResponse {
    /// The degraded shape: the concurrency cap is still known (it comes from
    /// settings, not the actor), everything else is explicitly unknown.
    const fn unreachable(max_concurrent_downloads: u32) -> Self {
        Self {
            supervisor_reachable: false,
            active_downloads: None,
            dispatching: None,
            available_permits: None,
            rate_limit_backoff: None,
            max_concurrent_downloads,
            db_backoff_until: None,
            consecutive_db_failures: None,
            last_db_error: None,
        }
    }
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

/// Response for a successful actor restart.
#[derive(Debug, Serialize, ToSchema)]
pub struct ActorRestartResponse {
    /// The actor that was restarted, in its URL spelling.
    pub actor: String,
    pub message: String,
}

/// Error response body.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

/// Every actor the root supervisor will restart on request.
///
/// The URL spellings are derived from `SupervisedActor::as_str` rather than
/// written out a second time, so the accepted names, the 400 message listing
/// them, and the web panel's form actions cannot drift apart.
pub const SUPERVISED_ACTORS: [SupervisedActor; 4] = [
    SupervisedActor::DownloadSupervisor,
    SupervisedActor::Scheduler,
    SupervisedActor::Cleanup,
    SupervisedActor::JellyfinMetadata,
];

/// Resolve a `{name}` path segment to the actor it names.
///
/// `None` is a client error (400), never a server one: the caller typed a
/// name that does not exist.
#[must_use]
pub fn parse_supervised_actor(name: &str) -> Option<SupervisedActor> {
    SUPERVISED_ACTORS
        .into_iter()
        .find(|actor| actor.as_str() == name)
}

/// The accepted `{name}` spellings, for error messages and form actions.
#[must_use]
pub fn supervised_actor_names() -> Vec<&'static str> {
    SUPERVISED_ACTORS
        .iter()
        .map(SupervisedActor::as_str)
        .collect()
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
/// - Pause state for indexing and downloads
/// - Drain progress
#[utoipa::path(
    get,
    path = "/status",
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

    let settings = state.runtime_config.current();
    let now = Utc::now();

    // Get scheduler status
    let scheduler_status = match state.scheduler.ask(GetSchedulerStatus).await {
        Ok(status) => SchedulerStatusResponse {
            running: status.running,
            active_indexers: status.active_indexers,
            check_interval_secs: status.check_interval_secs,
            max_indexers_per_tick: settings.max_indexers_per_tick.value,
        },
        Err(e) => {
            tracing::error!(error = %e, "Failed to get scheduler status");
            SchedulerStatusResponse {
                running: false,
                active_indexers: 0,
                check_interval_secs: 0,
                max_indexers_per_tick: settings.max_indexers_per_tick.value,
            }
        }
    };

    // Get download supervisor status.
    //
    // The error arm reports `supervisor_reachable: false` with null counts
    // rather than zeros — see `DownloadsStatusResponse` for why that
    // distinction is load-bearing.
    let downloads_status = match state.supervisor.ask(GetSupervisorStatus).await {
        Ok(status) => DownloadsStatusResponse {
            supervisor_reachable: true,
            active_downloads: Some(status.active_downloads),
            dispatching: Some(status.dispatching),
            available_permits: Some(status.available_permits),
            rate_limit_backoff: Some(status.rate_limit_backoff),
            max_concurrent_downloads: settings.max_concurrent_downloads.value,
            db_backoff_until: status.db_backoff_until,
            consecutive_db_failures: Some(status.consecutive_db_failures),
            last_db_error: status.last_db_error,
        },
        Err(e) => {
            tracing::error!(error = %e, "Failed to get supervisor status");
            DownloadsStatusResponse::unreachable(settings.max_concurrent_downloads.value)
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

    let pause = PauseSummaryResponse::from_settings(&settings, now);
    let drain = DrainStatusResponse::new(&state.drain, now);

    (
        StatusCode::OK,
        Json(SystemStatusResponse {
            scheduler: scheduler_status,
            downloads: downloads_status,
            cleanup: cleanup_status,
            statistics,
            pause,
            drain,
            timestamp: now,
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
    path = "/cleanup",
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

/// Restart one supervised actor.
///
/// The recovery path for the failure mode that stopped downloads for 27 hours:
/// a supervised actor died and nothing brought it back. An operator (or a
/// monitor reacting to `/api/health`) can restart it here without an SSH
/// session or a process bounce.
///
/// A `503` means the root supervisor refused: the actor has exhausted its
/// restart budget, so nothing in-process can recover it and the process itself
/// must be restarted. That is deliberately not a `500` — the request was
/// understood and answered truthfully, the capability is just gone.
#[utoipa::path(
    post,
    path = "/actors/{name}/restart",
    tag = "system",
    params(
        ("name" = String, Path, description = "Actor name: download_supervisor, scheduler, cleanup, or jellyfin_metadata")
    ),
    responses(
        (status = 200, description = "Actor restarted", body = ActorRestartResponse),
        (status = 400, description = "Unknown actor name", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 503, description = "Restart limit exhausted - process restart required", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn restart_actor(
    State(state): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let Some(which) = parse_supervised_actor(&name) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!(
                    "Unknown actor '{name}'. Valid names: {}",
                    supervised_actor_names().join(", ")
                ),
            }),
        )
            .into_response();
    };

    // Operator-visible and state-changing, so it is logged regardless of
    // outcome — an unexplained restart in the activity timeline is worse than
    // no restart at all.
    tracing::info!(actor = which.as_str(), "Actor restart requested via API");

    match state.root_supervisor.ask(RestartActor { which }).await {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(ActorRestartResponse {
                actor: which.as_str().to_string(),
                message: format!("Restarted {}", which.as_str()),
            }),
        )
            .into_response(),
        // The supervisor answered, and the answer is "no". Almost always the
        // restart limit: retrying will not help, so say so rather than
        // inviting a retry loop.
        Ok(Err(reason)) => {
            tracing::error!(
                actor = which.as_str(),
                %reason,
                "Root supervisor refused to restart actor"
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: format!("Cannot restart {}: {reason}", which.as_str()),
                }),
            )
                .into_response()
        }
        // The root supervisor itself is unreachable. Nothing in-process can
        // fix that either, but it is a different fault than a refusal.
        Err(e) => {
            tracing::error!(actor = which.as_str(), error = %e, "Failed to reach the root supervisor");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to reach the root supervisor: {e}"),
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use hof_core::runtime_config::indefinite_pause;

    use super::*;
    use crate::routes::settings::PauseStateResponse;

    #[test]
    fn system_status_response_serializes_with_pause_and_drain() {
        let now = Utc::now();
        let response = SystemStatusResponse {
            scheduler: SchedulerStatusResponse {
                running: true,
                active_indexers: 1,
                check_interval_secs: 60,
                max_indexers_per_tick: 5,
            },
            downloads: DownloadsStatusResponse {
                supervisor_reachable: true,
                active_downloads: Some(2),
                dispatching: Some(1),
                available_permits: Some(1),
                rate_limit_backoff: Some(0),
                max_concurrent_downloads: 3,
                db_backoff_until: None,
                consecutive_db_failures: Some(0),
                last_db_error: None,
            },
            cleanup: CleanupStatusResponse {
                running: false,
                global_retention_days: None,
                cleanup_interval_secs: 900,
                last_run_at: None,
            },
            statistics: StatisticsResponse {
                total_videos: 0,
                pending_downloads: 0,
                downloading: 0,
                completed: 0,
                failed: 0,
                permanently_failed: 0,
                skipped: 0,
                cleaned: 0,
            },
            // Indefinite indexing pause is the case most likely to leak the
            // sentinel timestamp into JSON (ADR-0003) — assert directly on it.
            pause: PauseSummaryResponse {
                indexing: PauseStateResponse::new(Some(indefinite_pause()), now),
                downloads: PauseStateResponse::new(None, now),
            },
            drain: DrainStatusResponse {
                draining: false,
                started_at: None,
                deadline: None,
                remaining_secs: None,
            },
            timestamp: now,
        };

        let json = serde_json::to_string(&response).expect("serialization cannot fail");
        assert!(json.contains("\"pause\""));
        assert!(json.contains("\"drain\""));
        assert!(!json.contains("infinity"));
        // `indefinite_pause()` is a *finite* sentinel (9999-12-31...), not
        // `MAX_UTC` — a leak would serialize as this timestamp, not the
        // string "infinity". Check for the sentinel itself (R-I).
        assert!(!json.contains("9999"));
    }

    #[test]
    fn downloads_status_response_round_trips_dispatching_and_cap() {
        let response = DownloadsStatusResponse {
            supervisor_reachable: true,
            active_downloads: Some(2),
            dispatching: Some(3),
            available_permits: Some(1),
            rate_limit_backoff: Some(0),
            max_concurrent_downloads: 4,
            db_backoff_until: None,
            consecutive_db_failures: Some(0),
            last_db_error: None,
        };
        let json = serde_json::to_string(&response).expect("serialization cannot fail");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["dispatching"], 3);
        assert_eq!(value["max_concurrent_downloads"], 4);
        // The healthy shape is unchanged from before the supervision fix:
        // plain numbers, not wrapped in anything.
        assert_eq!(value["available_permits"], 1);
        assert_eq!(value["supervisor_reachable"], true);
    }

    /// The regression this whole response-shape change exists for: a dead
    /// supervisor must not report zeros that read as a leaked permit.
    #[test]
    fn unreachable_supervisor_reports_null_counts_not_zeros() {
        let json = serde_json::to_string(&DownloadsStatusResponse::unreachable(4))
            .expect("serialization cannot fail");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        assert_eq!(value["supervisor_reachable"], false);
        assert!(value["available_permits"].is_null());
        assert!(value["active_downloads"].is_null());
        assert!(value["dispatching"].is_null());
        assert!(value["consecutive_db_failures"].is_null());
        // The cap comes from settings, not the actor, so it survives.
        assert_eq!(value["max_concurrent_downloads"], 4);
    }

    /// The URL spellings are a public contract shared with the web panel's
    /// form actions; pin them so a rename in `hof-core` cannot silently break
    /// both the endpoint and the panel at once.
    #[test]
    fn every_supervised_actor_name_parses_back_to_its_variant() {
        assert_eq!(
            supervised_actor_names(),
            vec![
                "download_supervisor",
                "scheduler",
                "cleanup",
                "jellyfin_metadata"
            ]
        );
        for name in supervised_actor_names() {
            let parsed = parse_supervised_actor(name).expect("name comes from as_str");
            assert_eq!(parsed.as_str(), name);
        }
    }

    #[test]
    fn unknown_actor_name_does_not_parse() {
        assert!(parse_supervised_actor("downloadsupervisor").is_none());
        assert!(parse_supervised_actor("DownloadSupervisor").is_none());
        assert!(parse_supervised_actor("").is_none());
    }

    #[test]
    fn scheduler_status_response_round_trips_max_indexers_per_tick() {
        let response = SchedulerStatusResponse {
            running: true,
            active_indexers: 0,
            check_interval_secs: 60,
            max_indexers_per_tick: 7,
        };
        let json = serde_json::to_string(&response).expect("serialization cannot fail");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["max_indexers_per_tick"], 7);
    }
}
