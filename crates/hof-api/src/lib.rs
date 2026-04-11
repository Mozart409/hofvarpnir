//! REST API for Hofvarpnir video archival system.
//!
//! This crate provides:
//! - Profile CRUD endpoints
//! - Source CRUD endpoints with manual indexing trigger
//! - Download status, progress SSE, and retry endpoints
//! - Activity log endpoints
//! - System status and control endpoints
//! - `OpenAPI` documentation via utoipa + Scalar
#![allow(clippy::needless_for_each)]

pub mod routes;

use axum::{Json, Router, routing::get};
use std::sync::Arc;

use hof_core::actors::cleanup::CleanupActor;
use hof_core::actors::download_supervisor::DownloadSupervisor;
use hof_core::actors::jellyfin_metadata::JellyfinMetadataActor;
use hof_core::actors::scheduler::SchedulerActor;
use hof_core::db::ActivityBroadcaster;
use hof_core::domain::system::SystemIssue;
use hof_core::domain::video::DownloadProgress;
use kameo::actor::ActorRef;
use sqlx::PgPool;
use tokio::sync::broadcast;
use utoipa::OpenApi;
use utoipa_scalar::{Scalar, Servable};

use routes::{activity, downloads, health, profiles, sources, system};

/// Shared application state for API handlers.
#[derive(Clone)]
pub struct AppState {
    /// Database connection pool.
    pub pool: PgPool,
    /// Reference to the download supervisor actor.
    pub supervisor: ActorRef<DownloadSupervisor>,
    /// Reference to the scheduler actor.
    pub scheduler: ActorRef<SchedulerActor>,
    /// Reference to the Jellyfin metadata actor.
    pub jellyfin_metadata: ActorRef<JellyfinMetadataActor>,
    /// Reference to the cleanup actor.
    pub cleanup: ActorRef<CleanupActor>,
    /// Broadcast channel for download progress updates (for SSE).
    pub progress_tx: broadcast::Sender<DownloadProgress>,
    /// Issues detected during startup (non-fatal warnings/errors).
    pub startup_issues: Arc<[SystemIssue]>,
    /// Broadcaster for real-time SSE notifications (activity + invalidation).
    pub broadcaster: ActivityBroadcaster,
}

impl AppState {
    /// Create a new `AppState`.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        supervisor: ActorRef<DownloadSupervisor>,
        scheduler: ActorRef<SchedulerActor>,
        jellyfin_metadata: ActorRef<JellyfinMetadataActor>,
        cleanup: ActorRef<CleanupActor>,
        progress_tx: broadcast::Sender<DownloadProgress>,
        startup_issues: Vec<SystemIssue>,
        broadcaster: ActivityBroadcaster,
    ) -> Self {
        Self {
            pool,
            supervisor,
            scheduler,
            jellyfin_metadata,
            cleanup,
            progress_tx,
            startup_issues: startup_issues.into(),
            broadcaster,
        }
    }
}

/// `OpenAPI` documentation for the API.
#[allow(clippy::needless_for_each)]
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Hofvarpnir API",
        version = "1.0.0",
        description = "Video archival system API for managing profiles, sources, and downloads."
    ),
    paths(
        health::health_check,
        health::liveness,
        health::readiness,
        profiles::list_profiles,
        profiles::create_profile,
        profiles::get_profile,
        profiles::update_profile,
        profiles::delete_profile,
        sources::list_sources,
        sources::create_source,
        sources::get_source,
        sources::update_source,
        sources::delete_source,
        sources::trigger_index,
        sources::trigger_metadata,
        downloads::list_downloads,
        downloads::get_download_progress,
        downloads::get_download,
        downloads::cancel_download,
        downloads::delete_download,
        downloads::retry_download,
        downloads::bulk_retry_downloads,
        activity::list_activity,
        system::get_system_status,
        system::trigger_cleanup,
    ),
    components(schemas(
        health::HealthResponse,
        health::HealthStatus,
        health::ComponentHealth,
        health::ActorsHealth,
        profiles::ProfileResponse,
        profiles::CreateProfileRequest,
        profiles::UpdateProfileRequest,
        sources::SourceResponse,
        sources::CreateSourceRequest,
        sources::UpdateSourceRequest,
        sources::IndexTriggerResponse,
        sources::MetadataTriggerResponse,
        downloads::VideoResponse,
        downloads::RetryResponse,
        downloads::BulkRetryResponse,
        downloads::CancelResponse,
        downloads::DeleteResponse,
        activity::ActivityEventResponse,
        activity::ActivityListResponse,
        system::SystemStatusResponse,
        system::SchedulerStatusResponse,
        system::DownloadsStatusResponse,
        system::CleanupStatusResponse,
        system::StatisticsResponse,
        system::CleanupTriggerResponse,
        system::CleanupResultResponse,
        hof_core::domain::profile::Quality,
        hof_core::domain::source::SourceType,
        hof_core::domain::video::VideoStatus,
        hof_core::domain::activity::ActivityEventType,
        hof_core::domain::activity::ActivitySeverity,
        hof_core::domain::system::SystemIssue,
        hof_core::domain::system::IssueSeverity,
    )),
    tags(
        (name = "health", description = "Health check endpoints"),
        (name = "profiles", description = "Profile management endpoints"),
        (name = "sources", description = "Source management endpoints"),
        (name = "downloads", description = "Download management endpoints"),
        (name = "activity", description = "Activity log endpoints"),
        (name = "system", description = "System status and control endpoints")
    )
)]
pub struct ApiDoc;

/// Build the API router with all JSON + SSE endpoints.
///
/// Mount at `/api` in the top-level application.
///
/// # Arguments
///
/// * `state` - Shared application state
pub fn router(state: AppState) -> Router {
    Router::new()
        .nest("/health", health::router())
        .nest("/v1/profiles", profiles::router())
        .nest("/v1/sources", sources::router())
        .nest("/v1/downloads", downloads::router())
        .nest("/v1/activity", activity::router())
        .nest("/v1/system", system::router())
        .with_state(state)
}

/// Build the Scalar `OpenAPI` documentation router.
///
/// Mount at `/docs` in the top-level application.
///
/// Provides:
/// - `/docs/` - Scalar UI for interactive API documentation
/// - `/docs/openapi.json` - Raw `OpenAPI` specification in JSON format
pub fn scalar_router() -> Router {
    Router::new()
        .merge(Scalar::with_url("/", ApiDoc::openapi()))
        .route("/openapi.json", get(openapi_json))
}

/// Serve the `OpenAPI` specification as JSON.
async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}
