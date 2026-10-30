use adk_rust::prelude::*;
use adk_rust::server::A2aServer;
use std::sync::Arc;
use tracing::warn;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let api_key =
        std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set in .env or environment");

    let model = GeminiModel::new(api_key, "gemini-3.7-flash")?;

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("a2a-quickstart-agent")
            .description("A minimal A2A-capable AI assistant built with ADK-Rust")
            .instruction("You are a helpful assistant exposed via the A2A protocol. Answer questions clearly and concisely.")
            .model(Arc::new(model))
            .build()?,
    );

    // Loopback unless BIND_HOST widens it: the server has no authentication.
    let host = std::env::var("BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let listener = tokio::net::TcpListener::bind((host.as_str(), 8003)).await?;
    let addr = listener.local_addr()?;
    if !addr.ip().is_loopback() {
        warn!(
            %addr,
            "listening beyond loopback without authentication: anyone who can reach this \
             address can call the agent"
        );
    }

    // The agent card advertises the address the listener bound; the listener serves the router.
    let url = format!("http://{addr}");
    let server = A2aServer::builder().agent(agent).agent_card_url(&url).build()?;

    println!("🚀 ADK-Rust A2A agent running on {url}");
    println!();
    println!("  Endpoints:");
    println!("    GET  {url}/.well-known/agent-card.json  — Agent card");
    println!("    POST {url}/a2a                          — JSON-RPC (message/send)");
    println!();
    println!("  Test with curl:");
    println!("    curl {url}/.well-known/agent-card.json | jq .");
    println!();
    println!("    curl -X POST {url}/a2a \\");
    println!("      -H 'Content-Type: application/json' \\");
    println!(
        "      -d '{{\"jsonrpc\":\"2.0\",\"method\":\"message/send\",\"params\":{{\"message\":{{\"role\":\"user\",\"parts\":[{{\"kind\":\"text\",\"text\":\"Hello, what can you do?\"}}],\"messageId\":\"test-1\"}}}},\"id\":\"req-1\"}}'"
    );

    axum::serve(listener, server.into_router()).await?;
    Ok(())
}
