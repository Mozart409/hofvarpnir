//! REST API for Hofvarpnir video archival system.
//!
//! This crate provides:
//! - Profile CRUD endpoints
//! - Source CRUD endpoints with manual indexing trigger
//! - Download status, progress SSE, and retry endpoints
//! - `OpenAPI` documentation via utoipa + Scalar
#![allow(clippy::needless_for_each)]

pub mod routes;

use axum::Router;
use hof_core::actors::download_supervisor::DownloadSupervisor;
use hof_core::actors::scheduler::SchedulerActor;
use hof_core::domain::video::DownloadProgress;
use kameo::actor::ActorRef;
use sqlx::PgPool;
use tokio::sync::broadcast;
use utoipa::OpenApi;
use utoipa_scalar::{Scalar, Servable};

use routes::{downloads, profiles, sources};

/// Shared application state for API handlers.
#[derive(Clone)]
pub struct AppState {
    /// Database connection pool.
    pub pool: PgPool,
    /// Reference to the download supervisor actor.
    pub supervisor: ActorRef<DownloadSupervisor>,
    /// Reference to the scheduler actor.
    pub scheduler: ActorRef<SchedulerActor>,
    /// Broadcast channel for download progress updates (for SSE).
    pub progress_tx: broadcast::Sender<DownloadProgress>,
}

impl AppState {
    /// Create a new `AppState`.
    #[must_use]
    pub fn new(
        pool: PgPool,
        supervisor: ActorRef<DownloadSupervisor>,
        scheduler: ActorRef<SchedulerActor>,
        progress_tx: broadcast::Sender<DownloadProgress>,
    ) -> Self {
        Self {
            pool,
            supervisor,
            scheduler,
            progress_tx,
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
        downloads::list_downloads,
        downloads::get_download_progress,
        downloads::retry_download,
    ),
    components(schemas(
        profiles::ProfileResponse,
        profiles::CreateProfileRequest,
        profiles::UpdateProfileRequest,
        sources::SourceResponse,
        sources::CreateSourceRequest,
        sources::UpdateSourceRequest,
        sources::IndexTriggerResponse,
        downloads::VideoResponse,
        downloads::RetryResponse,
        hof_core::domain::profile::Quality,
        hof_core::domain::source::SourceType,
        hof_core::domain::video::VideoStatus,
    )),
    tags(
        (name = "profiles", description = "Profile management endpoints"),
        (name = "sources", description = "Source management endpoints"),
        (name = "downloads", description = "Download management endpoints")
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
        .nest("/v1/profiles", profiles::router())
        .nest("/v1/sources", sources::router())
        .nest("/v1/downloads", downloads::router())
        .with_state(state)
}

/// Build the Scalar `OpenAPI` documentation router.
///
/// Mount at `/docs` in the top-level application.
pub fn scalar_router() -> Router {
    Router::new().merge(Scalar::with_url("/", ApiDoc::openapi()))
}
