//! Test application setup.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum_test::TestServer;
use hof_api::AppState;
use hof_core::{
    ActivityBroadcaster,
    actors::{
        cleanup::CleanupActor,
        download_supervisor::DownloadSupervisor,
        jellyfin_metadata::JellyfinMetadataActor,
        root_supervisor::{ChildRefs, GetChildRefs, RootSupervisor, RootSupervisorArgs},
        scheduler::SchedulerActor,
    },
    config::{DownloadConfig, EnvOverrides},
    domain::video::DownloadProgress,
    liveness::LivenessFlag,
    runtime_config::{DrainToken, RuntimeConfig},
    ytdlp::YtdlpClient,
};
use kameo::actor::{ActorRef, Spawn};
use sqlx::PgPool;
use tokio::sync::{broadcast, mpsc};

/// Test application wrapper.
///
/// Provides a configured `TestServer` with database access and API key management.
pub struct TestApp {
    pub server: TestServer,
    pub pool: PgPool,
    /// The liveness flag backing `/api/health/live`, so a test can assert
    /// both the healthy and the unrecoverable response.
    pub liveness: LivenessFlag,
    /// Kept alive for the lifetime of the test: dropping the root supervisor
    /// would tear down the four supervised children with it.
    #[allow(dead_code)]
    root_supervisor: ActorRef<RootSupervisor>,
    #[allow(dead_code)]
    supervisor: ActorRef<DownloadSupervisor>,
    #[allow(dead_code)]
    scheduler: ActorRef<SchedulerActor>,
    #[allow(dead_code)]
    cleanup: ActorRef<CleanupActor>,
    #[allow(dead_code)]
    jellyfin_metadata: ActorRef<JellyfinMetadataActor>,
}

impl TestApp {
    /// Create a new test application with the provided database pool.
    ///
    /// The pool is provided by `#[sqlx::test]` which manages database isolation.
    pub async fn new(pool: PgPool) -> Self {
        // Create minimal actors for testing
        let (progress_tx, _progress_rx) = mpsc::channel::<DownloadProgress>(100);

        // Create a minimal yt-dlp client (won't actually be used in most tests)
        let ytdlp = Arc::new(
            YtdlpClient::new("yt-dlp", None, std::path::Path::new("/tmp"))
                .await
                .expect("Failed to create yt-dlp client"),
        );

        #[allow(clippy::duration_suboptimal_units)]
        let download_config = DownloadConfig {
            max_concurrent: 2,
            timeout: Duration::from_secs(60 * 60), // 1 hour
            max_attempts: 3,
            rate_limit_delay: Duration::from_millis(100),
            ytdlp_path: std::path::PathBuf::from("yt-dlp"),
        };

        let broadcaster = ActivityBroadcaster::new();

        let runtime_config = RuntimeConfig::new(pool.clone(), EnvOverrides::default())
            .await
            .expect("Failed to load runtime settings");

        // Process-local drain signal (see ADR-0004). Triggerable through
        // this test app via `POST /api/v1/system/shutdown`, which calls
        // `drain.begin(..)`; actor gates and the shutdown poller read
        // `drain.is_draining()` to stop taking new work and detect
        // quiescence.
        let drain = DrainToken::new();

        // Spawn the four actors the way production does — as supervised
        // children of a `RootSupervisor` — rather than individually. The
        // health and restart endpoints under test resolve actors *through*
        // the root supervisor, so spawning a second, unsupervised set here
        // would leave those tests asserting against actors the API never
        // touches.
        let root_supervisor = RootSupervisor::spawn(RootSupervisorArgs {
            pool: pool.clone(),
            ytdlp,
            download_config,
            progress_tx,
            runtime_config: runtime_config.clone(),
            broadcaster: broadcaster.clone(),
            drain: drain.clone(),
            global_retention_days: None,
        });

        let ChildRefs {
            supervisor,
            scheduler,
            cleanup,
            jellyfin_metadata,
        } = root_supervisor
            .ask(GetChildRefs)
            .await
            .expect("root supervisor should report its child refs");

        // Create broadcast channel for SSE (not used in most tests)
        let (broadcast_tx, _) = broadcast::channel::<DownloadProgress>(100);

        // No watchdog runs under test, so nothing ever trips this on its own.
        // Exposed on `TestApp` so a test can drive `/api/health/live` through
        // both of its outcomes.
        let liveness = LivenessFlag::new();

        let state = AppState::new(
            pool.clone(),
            root_supervisor.clone(),
            supervisor.clone(),
            scheduler.clone(),
            jellyfin_metadata.clone(),
            cleanup.clone(),
            broadcast_tx,
            vec![],
            broadcaster,
            None,
            std::time::Duration::from_hours(2),
            runtime_config,
            drain,
            liveness.clone(),
        );

        // Build the API router with docs
        let (api_router, openapi) = hof_api::router(state);
        let app = Router::new()
            .merge(api_router)
            .merge(hof_api::scalar_router(openapi));

        let server = TestServer::new(app);

        Self {
            server,
            pool,
            liveness,
            root_supervisor,
            supervisor,
            scheduler,
            cleanup,
            jellyfin_metadata,
        }
    }
}
