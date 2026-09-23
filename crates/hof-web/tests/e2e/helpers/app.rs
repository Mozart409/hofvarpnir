//! Test application setup for web e2e tests.

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
use kameo::actor::{ActorRef, Spawn}; // Spawn trait must be imported for .spawn()
use sqlx::PgPool;
use tokio::sync::{broadcast, mpsc};

/// Test web application wrapper.
///
/// Provides a configured `TestServer` with database access and session support.
pub struct TestWebApp {
    pub server: TestServer,
    pub pool: PgPool,
    /// The liveness flag backing `/api/health/live`.
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

impl TestWebApp {
    /// Create a new test web application with the provided database pool.
    ///
    /// The pool is provided by `#[sqlx::test]` which manages database isolation.
    /// This initializes the full actor system and web router.
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
            verify_downloads: false,
        };

        let broadcaster = ActivityBroadcaster::new();

        let runtime_config = RuntimeConfig::new(pool.clone(), EnvOverrides::default())
            .await
            .expect("Failed to load runtime settings");

        // Matches `startup.rs`: settings changes reach the actors over
        // LISTEN/NOTIFY, so without this listener the actors serve a stale
        // snapshot for the whole test.
        // No test here asserts a settings change reaching the actors, and the
        // listener holds a second connection out of the pool `#[sqlx::test]`
        // caps at 5 -- which, against sqlx's process-wide 20-connection master
        // pool, stalls every test past the fourth in flight for the full 30s
        // `acquire_timeout`. See `hof-api`'s `TestApp::with_settings_listener`.

        let drain = DrainToken::new();

        // Spawn the four actors the way production does
        let root_supervisor = RootSupervisor::spawn(RootSupervisorArgs {
            pool: pool.clone(),
            ytdlp,
            download_config,
            progress_tx,
            runtime_config: runtime_config.clone(),
            broadcaster: broadcaster.clone(),
            drain: drain.clone(),
            global_retention_days: None,
            autostart: false,
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

        let (broadcast_tx, _) = broadcast::channel::<DownloadProgress>(100);

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

        // Initialize session layer and build the web router
        let session_layer = hof_web::session_layer(pool.clone())
            .await
            .expect("Failed to initialize session layer");

        // Build the web router (without API router for now)
        let web_router = hof_web::router(state, None);

        // Create the app
        let app = Router::new().merge(web_router).layer(session_layer);

        // `save_cookies` makes the server carry the `tower-sessions` cookie
        // from the login response into every later request, which is what
        // lets these tests exercise authenticated pages over real HTTP
        // instead of reaching around the session layer.
        let mut server = TestServer::new(app);
        server.save_cookies();

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

    /// Log in as `user` by posting the real login form, so every later
    /// request on this server carries a valid session cookie.
    ///
    /// The handler answers a *failed* login with 200 and a re-rendered form
    /// rather than an error status, so this asserts on the redirect to
    /// `/dashboard` — otherwise a broken password would surface much later as
    /// an unrelated "redirected to /login" failure in the test body.
    pub async fn login_as(&self, user: &hof_core::domain::user::User) {
        self.login_with(&user.email, super::builders::TEST_PASSWORD)
            .await;
    }

    /// Log in with explicit credentials, asserting the login succeeded.
    pub async fn login_with(&self, email: &str, password: &str) {
        let response = self
            .server
            .post("/login")
            .form(&[("email", email), ("password", password)])
            .await;

        assert_eq!(
            response.status_code(),
            axum_test::http::StatusCode::SEE_OTHER,
            "login for {email} should redirect; got {} with body:\n{}",
            response.status_code(),
            response.text()
        );
        assert_eq!(
            response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok()),
            Some("/dashboard"),
            "login should land on the dashboard"
        );
    }
}
