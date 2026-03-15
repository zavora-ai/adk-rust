use adk_spatial_os::{ServerConfig, run_server};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let host = std::env::var("ADK_SPATIAL_OS_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("ADK_SPATIAL_OS_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8199);

    println!("ADK Spatial OS shell running at http://{}:{}", host, port);
    run_server(ServerConfig { host, port }).await
}
