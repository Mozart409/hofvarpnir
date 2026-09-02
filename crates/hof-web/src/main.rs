use std::sync::Arc;

use axum::routing::get;
use color_eyre::Result;
use http::header::HeaderName;
use tokio::sync::broadcast;
use tower_http::propagate_header::PropagateHeaderLayer;
use tower_http::request_id::SetRequestIdLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use hof_core::{
    Config, HttpResponseRecorder, RequestSpan, UlidRequestId, db, init_tracing, initialize,
    oidc::{OidcClient, OidcConfig},
    shutdown,
};

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Load .env before tracing so OTEL_EXPORTER_OTLP_ENDPOINT / LOKI_URL are visible
    dotenvy::dotenv().ok();

    // Initialize tracing (supports LOG_FORMAT=json, OTEL_EXPORTER_OTLP_ENDPOINT)
    let _telemetry_guard = init_tracing();

    // Initialize Prometheus metrics
    let metrics_handle = Arc::new(hof_core::metrics::init_metrics());

    // Load configuration
    let config = Config::load()?;

    // Initialize database
    let pool = db::create_pool().await?;

    db::run_migrations(&pool).await?;

    // Initialize actor system
    let mut actor_system = initialize(pool.clone(), &config).await?;

    // `shutdown()` below takes `actor_system` by value, so clone the drain
    // handle now for the select arm and for `AppState`.
    let drain = actor_system.drain.clone();

    // Create broadcast channel for SSE progress updates
    // The actor system uses mpsc, so we bridge it to broadcast
    let (progress_tx, _) = broadcast::channel(1000);

    // Take the progress receiver from actor system (replacing with a dummy)
    let (_, dummy_rx) = tokio::sync::mpsc::channel(1);
    let progress_rx = std::mem::replace(&mut actor_system.progress_rx, dummy_rx);

    // Spawn task to forward progress from mpsc to broadcast
    let progress_tx_clone = progress_tx.clone();
    tokio::spawn(async move {
        let mut rx = progress_rx;
        while let Some(progress) = rx.recv().await {
            // Ignore send errors (no subscribers)
            let _ = progress_tx_clone.send(progress);
        }
    });

    // Create API state
    let api_state = hof_api::AppState::new(
        pool.clone(),
        actor_system.supervisor.clone(),
        actor_system.scheduler.clone(),
        actor_system.jellyfin_metadata.clone(),
        actor_system.cleanup.clone(),
        progress_tx,
        std::mem::take(&mut actor_system.startup_issues),
        actor_system.broadcaster.clone(),
        config
            .storage
            .retention_days
            .map(|d| i32::try_from(d).unwrap_or(i32::MAX)),
        drain.clone(),
    );

    // Initialize OIDC client if configured
    let oidc_client: Option<Arc<OidcClient>> = if OidcConfig::is_configured() {
        match init_oidc().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!(error = %e, "Failed to initialize OIDC client, continuing without OIDC");
                None
            }
        }
    } else {
        info!("OIDC not configured (OIDC_ISSUER not set)");
        None
    };

    // Create session layer
    let session_layer = hof_web::session_layer(pool).await?;

    // Build the application router
    let x_request_id = HeaderName::from_static("x-request-id");
    let (api_router, openapi) = hof_api::router(api_state.clone());
    let app = axum::Router::new()
        .merge(axum::Router::new().route(
            "/metrics",
            get({
                let handle = Arc::clone(&metrics_handle);
                move || async move { handle.render() }
            }),
        ))
        .merge(api_router)
        .merge(hof_api::scalar_router(openapi))
        .merge(hof_web::router(api_state, oidc_client.as_ref()))
        .layer(session_layer)
        .layer(axum::middleware::from_fn(hof_web::middleware::http_metrics))
        // Layer order (axum applies bottom-up, so outermost layer is last):
        // 1. PropagateHeader: copy x-request-id from request to response
        .layer(PropagateHeaderLayer::new(x_request_id.clone()))
        // 2. Trace: create span (sees x-request-id set by SetRequestId)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(RequestSpan)
                .on_response(HttpResponseRecorder),
        )
        // 3. SetRequestId: generate ULID and set x-request-id header
        .layer(SetRequestIdLayer::new(x_request_id, UlidRequestId));

    let bind_addr = config.server.bind_addr();
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;

    tracing::info!("listening on {}", listener.local_addr()?);

    // Start the server
    let server = axum::serve(listener, app);

    // Handle graceful shutdown
    tokio::select! {
        result = server => {
            if let Err(e) = result {
                tracing::error!(error = %e, "Server error");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received shutdown signal");
        }
        () = drain.wait_complete() => {
            tracing::info!("Drain complete, shutting down");
        }
    }

    // Shutdown actor system gracefully
    if let Err(e) = shutdown(actor_system).await {
        tracing::error!(error = %e, "Error during shutdown");
    }

    tracing::info!("Server shutdown complete");

    Ok(())
}

/// Initialize OIDC client if configured.
///
/// Discovers the OIDC provider metadata and creates a client.
///
/// # Errors
///
/// Returns an error if OIDC is configured but discovery fails.
async fn init_oidc() -> Result<Option<Arc<OidcClient>>> {
    let Some(config) = OidcConfig::from_env() else {
        return Ok(None);
    };

    info!(issuer = %config.issuer_url, "Discovering OIDC provider");
    let client = OidcClient::discover(config).await?;
    info!("OIDC client initialized successfully");

    Ok(Some(Arc::new(client)))
}
