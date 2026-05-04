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

pub mod auth;
pub mod routes;

use std::sync::Arc;

use axum::{Json, Router, routing::get};
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
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_scalar::{Scalar, Servable};

use routes::{activity, downloads, health, profiles, sources, system};

/// Security scheme modifier for `OpenAPI` documentation.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "api_key",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("API Key")
                        .description(Some(
                            "API key authentication. Use format: Bearer hof_sk_...",
                        ))
                        .build(),
                ),
            );
        }
    }
}

/// Server URL modifier for `OpenAPI` documentation.
///
/// Reads `API_BASE_URL` env var to set the server URL in the `OpenAPI` spec.
/// Defaults to `http://localhost:8080` if not set.
struct ServerAddon;

impl Modify for ServerAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let base_url =
            std::env::var("API_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
        openapi.servers = Some(vec![
            utoipa::openapi::ServerBuilder::new()
                .url(&base_url)
                .description(Some("API Server"))
                .build(),
        ]);
    }
}

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

/// `OpenAPI` base documentation for the API.
///
/// Paths are auto-registered via `utoipa-axum` in `router()`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Hofvarpnir API",
        version = "1.0.0",
        description = "Video archival system API for managing profiles, sources, and downloads."
    ),
    components(schemas(
        health::HealthResponse,
        health::HealthStatus,
        health::ComponentHealth,
        health::ActorsHealth,
        profiles::ProfileResponse,
        profiles::CreateProfileRequest,
        profiles::UpdateProfileRequest,
        profiles::ErrorResponse,
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
        downloads::ErrorResponse,
        downloads::ProgressEvent,
        activity::ActivityEventResponse,
        activity::ActivityListResponse,
        system::SystemStatusResponse,
        system::SchedulerStatusResponse,
        system::DownloadsStatusResponse,
        system::CleanupStatusResponse,
        system::StatisticsResponse,
        system::CleanupTriggerResponse,
        system::CleanupResultResponse,
        auth::ApiErrorResponse,
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
    ),
    modifiers(&SecurityAddon, &ServerAddon)
)]
struct ApiDoc;

/// Build the API router with all JSON + SSE endpoints.
///
/// Returns both the Axum router and the `OpenAPI` spec (with paths auto-registered).
/// Mount the router at `/api` in the top-level application.
///
/// # Arguments
///
/// * `state` - Shared application state
pub fn router(state: AppState) -> (Router, utoipa::openapi::OpenApi) {
    let (router, mut api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .nest("/api/health", health::router())
        .nest("/api/v1/profiles", profiles::router())
        .nest("/api/v1/sources", sources::router())
        .nest("/api/v1/downloads", downloads::router())
        .nest("/api/v1/activity", activity::router())
        .nest("/api/v1/system", system::router())
        .split_for_parts();

    // Apply modifiers (security scheme, server URL)
    SecurityAddon.modify(&mut api);
    ServerAddon.modify(&mut api);

    let router = router.with_state(state);

    (router, api)
}

/// Build the Scalar `OpenAPI` documentation router.
///
/// Mount at `/docs` in the top-level application.
///
/// Provides:
/// - `/docs/` - Scalar UI for interactive API documentation
/// - `/docs/openapi.json` - Raw `OpenAPI` specification in JSON format
///
/// # Arguments
///
/// * `openapi` - The `OpenAPI` spec from `router()`
pub fn scalar_router(openapi: utoipa::openapi::OpenApi) -> Router {
    Router::new()
        .merge(Scalar::with_url("/", openapi.clone()))
        .route(
            "/openapi.json",
            get(move || async move { Json(openapi.clone()) }),
        )
}
