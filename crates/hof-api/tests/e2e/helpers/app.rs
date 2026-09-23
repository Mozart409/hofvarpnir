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
    db,
    domain::video::DownloadProgress,
    liveness::LivenessFlag,
    runtime_config::{DrainToken, RuntimeConfig},
    ytdlp::YtdlpClient,
};
use kameo::actor::{ActorRef, Spawn};
use sqlx::PgPool;
use tokio::sync::{broadcast, mpsc};
use ulid::Ulid;

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
        Self::build(pool, false, false, false).await
    }

    /// Create a test application with optional download verification.
    ///
    /// Use this to test the full download pipeline including verification.
    pub async fn with_verification(pool: PgPool, verify_downloads: bool) -> Self {
        Self::build(pool, verify_downloads, false, false).await
    }

    /// Create a test application whose runtime-settings listener is running.
    ///
    /// Opt-in because the listener holds a second connection out of the pool
    /// `#[sqlx::test]` hands each test, and that pool is capped at 5 and draws
    /// from a single process-wide master pool capped at 20 (see
    /// `sqlx_postgres::testing`). At four tests in flight the two caps meet
    /// exactly, and the next acquire waits out sqlx's 30s `acquire_timeout` --
    /// which is 30 seconds added to whichever tests happen to be running.
    /// Only a test that asserts a settings change reaching the actors needs
    /// it; see [`Self::wait_for_settings`].
    pub async fn with_settings_listener(pool: PgPool) -> Self {
        Self::build(pool, false, true, false).await
    }

    /// Create a test application whose scheduler/cleanup/metadata loops run.
    ///
    /// Opt-in: those loops write to the same tables the assertions read, so a
    /// test that seeds a row in a state a loop acts on (a pending video, a
    /// video past its retention) otherwise races it. Only a test asserting
    /// the loops' own behaviour wants this.
    pub async fn with_running_actors(pool: PgPool) -> Self {
        Self::build(pool, false, false, true).await
    }

    async fn build(
        pool: PgPool,
        verify_downloads: bool,
        settings_listener: bool,
        autostart_actors: bool,
    ) -> Self {
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
            verify_downloads,
        };

        let broadcaster = ActivityBroadcaster::new();

        let runtime_config = RuntimeConfig::new(pool.clone(), EnvOverrides::default())
            .await
            .expect("Failed to load runtime settings");

        // Settings written through the API reach the actors only over
        // LISTEN/NOTIFY (`startup.rs` does the same). Without this listener a
        // test can PATCH settings or pause a module and the scheduler keeps
        // serving its stale snapshot, so the pause gate silently never fires.
        if settings_listener {
            let _listener = runtime_config.clone().spawn_listener();
        }

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
            autostart: autostart_actors,
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

    /// Wait until a settings change made through the API has propagated to
    /// the in-process actors.
    ///
    /// `PATCH /system/settings` and `POST /system/pause` write the row and
    /// `NOTIFY`; the listener picks that up asynchronously and republishes to
    /// the watch channel the actors read. `GET /system/settings` is served
    /// from that same channel (`runtime_config.current()`), so polling it
    /// until `predicate` holds is what makes "pause, then assert the gate
    /// fires" deterministic instead of racy.
    pub async fn wait_for_settings<F>(&self, bearer: &str, predicate: F)
    where
        F: Fn(&serde_json::Value) -> bool,
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);

        loop {
            let response = self
                .server
                .get("/api/v1/system/settings")
                .add_header("Authorization", bearer)
                .await;
            let body: serde_json::Value = response.json();

            if predicate(&body) {
                return;
            }

            assert!(
                std::time::Instant::now() < deadline,
                "settings did not propagate within 5s; last body: {body}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Enqueue a video for download using the download supervisor.
    ///
    /// This allows tests to directly trigger downloads without going through the scheduler.
    pub async fn enqueue_download(&self, video_id: Ulid, source_id: Ulid) {
        use hof_core::actors::download_supervisor::EnqueueDownload;

        let video = db::get_video(&self.pool, video_id)
            .await
            .expect("video should exist");

        let source = db::get_source(&self.pool, source_id)
            .await
            .expect("source should exist");

        let profile = db::get_profile(&self.pool, source.profile_id)
            .await
            .expect("profile should exist");

        self.supervisor
            .ask(EnqueueDownload {
                video,
                profile,
                source,
            })
            .await
            .expect("enqueue download should succeed");
    }
}
