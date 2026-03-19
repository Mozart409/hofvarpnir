use tokio::sync::broadcast;
use tracing_subscriber::EnvFilter;

use hof_core::{Config, db, initialize, shutdown};

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Load configuration
    let config = Config::load().expect("Failed to load configuration");

    // Initialize database
    let pool = db::create_pool()
        .await
        .expect("Failed to create database pool");

    db::run_migrations(&pool)
        .await
        .expect("Failed to run migrations");

    // Initialize actor system
    let mut actor_system = initialize(pool.clone(), &config)
        .await
        .expect("Failed to initialize actor system");

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
        progress_tx,
    );

    // Create session layer
    let session_layer = hof_web::session_layer(pool).await;

    // Build the application router
    let app = axum::Router::new()
        .nest("/api", hof_api::router(api_state.clone()))
        .nest("/docs", hof_api::scalar_router())
        .merge(hof_web::router(api_state))
        .layer(session_layer);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("failed to bind to port 3000");

    tracing::info!("listening on {}", listener.local_addr().unwrap());

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
}
