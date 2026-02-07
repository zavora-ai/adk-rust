use adk_studio::{
    AppState, FileStorage, api_routes, cleanup_stale_sessions, embedded, start_scheduler,
};
use axum::{Router, extract::Path as AxumPath, routing::get};
use std::net::SocketAddr;
use std::path::PathBuf;
use tower_http::{
    cors::{Any, CorsLayer},
    services::{ServeDir, ServeFile},
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Handler for serving embedded static files
async fn serve_static(AxumPath(path): AxumPath<String>) -> axum::response::Response {
    embedded::serve_embedded(path)
}

/// Handler for serving index.html at root
async fn serve_root() -> axum::response::Response {
    embedded::serve_index()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .iter()
        .position(|a| a == "--port" || a == "-p")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let host: [u8; 4] = args
        .iter()
        .position(|a| a == "--host" || a == "-h")
        .and_then(|i| args.get(i + 1))
        .map(|h| if h == "0.0.0.0" { [0, 0, 0, 0] } else { [127, 0, 0, 1] })
        .unwrap_or([127, 0, 0, 1]);

    let projects_dir = args
        .iter()
        .position(|a| a == "--dir" || a == "-d")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::data_local_dir().unwrap_or_default().join("adk-studio/projects"));

    let static_dir = args
        .iter()
        .position(|a| a == "--static" || a == "-s")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    let storage = FileStorage::new(projects_dir.clone()).await?;
    let state = AppState::new(storage);

    // Start the schedule trigger service in the background
    let scheduler_state = state.clone();
    tokio::spawn(async move {
        start_scheduler(scheduler_state).await;
    });

    // Start periodic session cleanup (every 10 minutes, remove sessions older than 1 hour)
    tokio::spawn(async {
        let cleanup_interval = std::time::Duration::from_secs(600);
        let max_session_age = std::time::Duration::from_secs(3600);
        loop {
            tokio::time::sleep(cleanup_interval).await;
            cleanup_stale_sessions(max_session_age).await;
        }
    });

    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    let mut app = Router::new().nest("/api", api_routes()).layer(cors).with_state(state);

    // Serve static files - external directory takes priority, otherwise use embedded
    if let Some(dir) = static_dir {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)));
        tracing::info!("📂 Serving static files from: {}", dir.display());
    } else {
        // Serve embedded static files (default)
        // Use a nested router for static files to avoid route conflicts
        let static_router =
            Router::new().route("/", get(serve_root)).route("/*path", get(serve_static));
        app = app.merge(static_router);
        tracing::info!("📦 Serving embedded static files");
    }

    let addr = SocketAddr::from((host, port));
    tracing::info!("🚀 ADK Studio starting on http://{}", addr);
    tracing::info!("📁 Projects directory: {}", projects_dir.display());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
