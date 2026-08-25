//! Test application setup.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum_test::TestServer;
use hof_api::AppState;
use hof_core::{
    ActivityBroadcaster,
    actors::{
        cleanup::{CleanupActor, CleanupActorArgs},
        download_supervisor::{DownloadSupervisor, DownloadSupervisorArgs},
        jellyfin_metadata::{JellyfinMetadataActor, JellyfinMetadataActorArgs},
        scheduler::{SchedulerActor, SchedulerArgs},
    },
    config::DownloadConfig,
    domain::video::DownloadProgress,
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

        let supervisor = DownloadSupervisor::spawn(DownloadSupervisorArgs {
            pool: pool.clone(),
            ytdlp: ytdlp.clone(),
            config: download_config,
            progress_tx,
            broadcaster: broadcaster.clone(),
        });

        let scheduler = SchedulerActor::spawn(SchedulerArgs {
            pool: pool.clone(),
            ytdlp,
            supervisor: supervisor.clone(),
            check_interval: None,
            max_indexers_per_tick: None,
            broadcaster: broadcaster.clone(),
        });

        let cleanup = CleanupActor::spawn(CleanupActorArgs {
            pool: pool.clone(),
            global_retention_days: None,
            cleanup_interval: None,
            broadcaster: broadcaster.clone(),
        });

        let jellyfin_metadata = JellyfinMetadataActor::spawn(JellyfinMetadataActorArgs {
            pool: pool.clone(),
            check_interval: None,
            broadcaster: broadcaster.clone(),
        });

        // Create broadcast channel for SSE (not used in most tests)
        let (broadcast_tx, _) = broadcast::channel::<DownloadProgress>(100);

        let state = AppState::new(
            pool.clone(),
            supervisor.clone(),
            scheduler.clone(),
            jellyfin_metadata.clone(),
            cleanup.clone(),
            broadcast_tx,
            vec![],
            broadcaster,
            None,
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
            supervisor,
            scheduler,
            cleanup,
            jellyfin_metadata,
        }
    }
}
