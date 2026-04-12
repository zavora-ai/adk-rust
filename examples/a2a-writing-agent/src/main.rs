//! A2A v1.0.0 Writing Agent server entry point.

use a2a_writing_agent::{build_server, build_writing_agent, detect_model};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3002);

    let (model, provider_name) = detect_model()?;
    let agent = build_writing_agent(model)?;

    let base_url = format!("http://{host}:{port}");
    let app = build_server(agent, &base_url);

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    println!("Writing Agent listening on http://{addr}");
    println!("Agent card: http://{addr}/.well-known/agent-card.json");
    println!("JSON-RPC:   POST http://{addr}/jsonrpc");
    println!("LLM:        {provider_name}");

    axum::serve(listener, app).await?;
    Ok(())
}
