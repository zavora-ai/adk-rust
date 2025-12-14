use adk_studio::{api_routes, AppState, FileStorage};
use axum::Router;
use std::net::SocketAddr;
use std::path::PathBuf;
use tower_http::{
    cors::{Any, CorsLayer},
    services::{ServeDir, ServeFile},
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .iter()
        .position(|a| a == "--port" || a == "-p")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

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

    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    let mut app = Router::new().nest("/api", api_routes()).layer(cors).with_state(state);

    // Serve static files if directory provided
    if let Some(dir) = static_dir {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)));
        tracing::info!("📂 Serving static files from: {}", dir.display());
    }

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("🚀 ADK Studio starting on http://{}", addr);
    tracing::info!("📁 Projects directory: {}", projects_dir.display());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
