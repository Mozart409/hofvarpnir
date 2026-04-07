use color_eyre::Result;
use http::header::HeaderName;
use tokio::sync::broadcast;
use tower_http::propagate_header::PropagateHeaderLayer;
use tower_http::request_id::SetRequestIdLayer;
use tower_http::trace::TraceLayer;

use hof_core::{Config, RequestSpan, UlidRequestId, db, init_tracing, initialize, shutdown};

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Initialize tracing (supports LOG_FORMAT=json)
    init_tracing();

    // Load configuration
    let config = Config::load()?;

    // Initialize database
    let pool = db::create_pool().await?;

    db::run_migrations(&pool).await?;

    // Initialize actor system
    let mut actor_system = initialize(pool.clone(), &config).await?;

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
    );

    // Create session layer
    let session_layer = hof_web::session_layer(pool).await?;

    // Build the application router
    let x_request_id = HeaderName::from_static("x-request-id");
    let app = axum::Router::new()
        .nest("/api", hof_api::router(api_state.clone()))
        .nest("/docs", hof_api::scalar_router())
        .merge(hof_web::router(api_state))
        .layer(session_layer)
        // Layer order (axum applies bottom-up, so outermost layer is last):
        // 1. PropagateHeader: copy x-request-id from request to response
        .layer(PropagateHeaderLayer::new(x_request_id.clone()))
        // 2. Trace: create span (sees x-request-id set by SetRequestId)
        .layer(TraceLayer::new_for_http().make_span_with(RequestSpan))
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
    }

    // Shutdown actor system gracefully
    if let Err(e) = shutdown(actor_system).await {
        tracing::error!(error = %e, "Error during shutdown");
    }

    tracing::info!("Server shutdown complete");

    Ok(())
}
