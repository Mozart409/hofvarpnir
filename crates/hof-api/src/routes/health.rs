//! Health check endpoints for container orchestration.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use utoipa::ToSchema;

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
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(health_check))
        .route("/live", get(liveness))
        .route("/ready", get(readiness))
}

/// Comprehensive health check.
///
/// Returns overall system health including database and yt-dlp status.
/// Use this for monitoring dashboards.
#[utoipa::path(
    get,
    path = "/api/health",
    responses(
        (status = 200, description = "System is healthy", body = HealthResponse),
        (status = 503, description = "System is unhealthy", body = HealthResponse),
    ),
    tag = "health"
)]
pub async fn health_check(State(state): State<AppState>) -> impl IntoResponse {
    let db_health = check_database(&state).await;
    let ytdlp_health = check_ytdlp().await;

    let status = if db_health.healthy && ytdlp_health.healthy {
        HealthStatus::Healthy
    } else if db_health.healthy {
        // Database works, yt-dlp issues are degraded (can still serve API)
        HealthStatus::Degraded
    } else {
        HealthStatus::Unhealthy
    };

    let response = HealthResponse {
        status: status.clone(),
        database: db_health,
        ytdlp: ytdlp_health,
    };

    let status_code = if status == HealthStatus::Unhealthy {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    (status_code, Json(response))
}

/// Kubernetes liveness probe.
///
/// Returns 200 if the process is alive. Does not check dependencies.
/// Use this for `livenessProbe` in Kubernetes.
#[utoipa::path(
    get,
    path = "/api/health/live",
    responses(
        (status = 200, description = "Process is alive"),
    ),
    tag = "health"
)]
pub async fn liveness() -> StatusCode {
    StatusCode::OK
}

/// Kubernetes readiness probe.
///
/// Returns 200 if the service is ready to receive traffic (database connected).
/// Use this for `readinessProbe` in Kubernetes and Docker HEALTHCHECK.
#[utoipa::path(
    get,
    path = "/api/health/ready",
    responses(
        (status = 200, description = "Service is ready"),
        (status = 503, description = "Service is not ready"),
    ),
    tag = "health"
)]
pub async fn readiness(State(state): State<AppState>) -> StatusCode {
    let db_health = check_database(&state).await;

    if db_health.healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
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
