//! Health check endpoints for container orchestration.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use hof_core::actors::root_supervisor::GetActorHealth;
use hof_core::domain::system::{IssueSeverity, SystemIssue};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::AppState;

/// Health check response.
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// Overall health status.
    pub status: HealthStatus,
    /// Database connectivity status.
    pub database: ComponentHealth,
    /// yt-dlp availability status.
    pub ytdlp: ComponentHealth,
    /// Actor system health status.
    pub actors: ActorsHealth,
    /// System issues detected during startup or runtime.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<SystemIssue>,
}

/// Actor system health status.
///
/// The five booleans are a frozen contract: the compose healthcheck and
/// external monitoring both read them, so they are never renamed or
/// re-derived. `details` was *added* alongside them — it answers the question
/// the booleans cannot ("why is it dead, and can it come back?") without
/// changing what they mean.
#[derive(Debug, Serialize, ToSchema)]
#[allow(clippy::struct_excessive_bools)]
pub struct ActorsHealth {
    /// Whether all actors are alive.
    pub healthy: bool,
    /// Individual actor statuses.
    pub supervisor: bool,
    pub scheduler: bool,
    pub cleanup: bool,
    pub jellyfin_metadata: bool,
    /// Per-actor restart and failure detail from the root supervisor.
    ///
    /// Empty when the root supervisor itself did not answer — the booleans
    /// above are still authoritative in that case, because they come from
    /// cheap in-process `is_alive()` checks that cannot fail.
    pub details: Vec<ActorDetail>,
}

/// Per-actor detail behind one of the `ActorsHealth` booleans.
#[derive(Debug, Serialize, ToSchema)]
pub struct ActorDetail {
    /// Actor name in its URL spelling, matching the `{name}` segment of
    /// `POST /api/v1/system/actors/{name}/restart`.
    pub actor: String,
    pub alive: bool,
    /// Restarts the root supervisor has performed since process start.
    pub restart_count: u32,
    pub last_restart_at: Option<DateTime<Utc>>,
    /// Why the actor last died. The single most useful field for an operator:
    /// a dead actor with no explanation is what turned a transient database
    /// error into a 27-hour outage.
    pub last_failure: Option<String>,
    /// The restart budget is spent — no in-process restart can recover this
    /// actor, the process must be restarted. A monitor should page on this
    /// rather than retrying the restart endpoint.
    pub unrecoverable: bool,
}

/// Overall health status.
#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// All components healthy.
    Healthy,
    /// Some components degraded but service is functional.
    Degraded,
    /// Service is unhealthy and should not receive traffic.
    Unhealthy,
}

/// Individual component health status.
#[derive(Debug, Serialize, ToSchema)]
pub struct ComponentHealth {
    /// Whether the component is healthy.
    pub healthy: bool,
    /// Optional message with details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Build the health router.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(health_check))
        .routes(routes!(liveness))
        .routes(routes!(readiness))
}

/// Comprehensive health check.
///
/// Returns overall system health including database and yt-dlp status.
/// Use this for monitoring dashboards.
#[utoipa::path(
    get,
    path = "",
    responses(
        (status = 200, description = "System is healthy", body = HealthResponse),
        (status = 503, description = "System is unhealthy", body = HealthResponse),
    ),
    tag = "health"
)]
pub async fn health_check(State(state): State<AppState>) -> impl IntoResponse {
    let db_health = check_database(&state).await;
    let ytdlp_health = check_ytdlp().await;
    let actors_health = check_actors_detailed(&state).await;
    let issues: Vec<SystemIssue> = state.startup_issues.to_vec();

    // Check if any issues are errors (vs warnings)
    let has_error_issues = issues.iter().any(|i| i.severity == IssueSeverity::Error);

    let status = if !db_health.healthy {
        HealthStatus::Unhealthy
    } else if !ytdlp_health.healthy || !actors_health.healthy || has_error_issues {
        HealthStatus::Degraded
    } else {
        HealthStatus::Healthy
    };

    let response = HealthResponse {
        status: status.clone(),
        database: db_health,
        ytdlp: ytdlp_health,
        actors: actors_health,
        issues,
    };

    let status_code = if status == HealthStatus::Unhealthy {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    (status_code, Json(response))
}

/// Liveness probe: is this process worth keeping, or should it be replaced?
///
/// Returns 503 only when an actor is *unrecoverable* — it exhausted its
/// restart budget, so kameo has unlinked it permanently and no in-process
/// restart can bring it back. A new process is then the only remedy, which
/// is exactly what a `livenessProbe` (or the watchdog's own self-exit) is
/// for.
///
/// Deliberately NOT tripped by a database outage or by an actor that is
/// merely down-and-restarting: the download supervisor now rides out a
/// database fault via its circuit breaker without dying, and supervision
/// recovers an ordinary panic on its own. Reporting either here would turn a
/// transient blip into a container restart — the failure mode this endpoint
/// exists to avoid.
///
/// Reads a shared atomic written by `hof_core::watchdog`, never an actor
/// `ask`: a probe must not be able to hang behind a mailbox, least of all
/// the root supervisor's, whose own health is part of what is being probed.
/// Use `/ready` (not this) to decide whether to send traffic.
#[utoipa::path(
    get,
    path = "/live",
    responses(
        (status = 200, description = "Process is alive and recoverable"),
        (
            status = 503,
            description = "An actor is unrecoverable; this process should be replaced"
        ),
    ),
    tag = "health"
)]
pub async fn liveness(State(state): State<AppState>) -> StatusCode {
    if state.liveness.is_alive() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Kubernetes readiness probe.
///
/// Returns 200 if the service is ready to receive traffic (database connected).
/// Use this for `readinessProbe` in Kubernetes and Docker HEALTHCHECK.
#[utoipa::path(
    get,
    path = "/ready",
    responses(
        (status = 200, description = "Service is ready"),
        (status = 503, description = "Service is not ready"),
    ),
    tag = "health"
)]
pub async fn readiness(State(state): State<AppState>) -> StatusCode {
    let db_health = check_database(&state).await;
    let actors_health = check_actors(&state);

    if db_health.healthy && actors_health.healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Liveness only, from in-process `is_alive()` checks.
///
/// Deliberately synchronous and infallible: `readiness` runs this on every
/// probe (every 31s in the compose deployment), and a probe must never be
/// able to hang behind an actor mailbox — least of all the root supervisor's,
/// whose own death is one of the things being probed for.
fn check_actors(state: &AppState) -> ActorsHealth {
    let supervisor = state.supervisor.is_alive();
    let scheduler = state.scheduler.is_alive();
    let cleanup = state.cleanup.is_alive();
    let jellyfin_metadata = state.jellyfin_metadata.is_alive();

    ActorsHealth {
        healthy: supervisor && scheduler && cleanup && jellyfin_metadata,
        supervisor,
        scheduler,
        cleanup,
        jellyfin_metadata,
        details: Vec::new(),
    }
}

/// `check_actors` plus the root supervisor's per-actor detail.
///
/// The `healthy` flag and the four booleans are still computed by
/// `check_actors`, so the top-level `status` this feeds is unaffected by
/// whether the detail ask succeeds. A failed ask degrades to empty `details`
/// and a warning, never to a worse verdict — reporting "unhealthy" because a
/// *diagnostic* call failed would be its own false alarm.
async fn check_actors_detailed(state: &AppState) -> ActorsHealth {
    let mut health = check_actors(state);

    match state.root_supervisor.ask(GetActorHealth).await {
        Ok(reports) => {
            health.details = reports
                .into_iter()
                .map(|report| ActorDetail {
                    actor: report.actor.as_str().to_string(),
                    alive: report.alive,
                    restart_count: report.restart_count,
                    last_restart_at: report.last_restart_at,
                    last_failure: report.last_failure,
                    unrecoverable: report.unrecoverable,
                })
                .collect();
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Root supervisor did not report actor health; serving liveness only"
            );
        }
    }

    health
}

async fn check_database(state: &AppState) -> ComponentHealth {
    match sqlx::query("SELECT 1").execute(&state.pool).await {
        Ok(_) => ComponentHealth {
            healthy: true,
            message: None,
        },
        Err(e) => ComponentHealth {
            healthy: false,
            message: Some(format!("Database connection failed: {e}")),
        },
    }
}

async fn check_ytdlp() -> ComponentHealth {
    // Check if yt-dlp is available and working
    match tokio::process::Command::new("yt-dlp")
        .arg("--version")
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            ComponentHealth {
                healthy: true,
                message: Some(format!("yt-dlp {version}")),
            }
        }
        Ok(output) => ComponentHealth {
            healthy: false,
            message: Some(format!(
                "yt-dlp exited with code: {:?}",
                output.status.code()
            )),
        },
        Err(e) => ComponentHealth {
            healthy: false,
            message: Some(format!("yt-dlp not found: {e}")),
        },
    }
}
