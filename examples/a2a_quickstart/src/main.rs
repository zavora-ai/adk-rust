use adk_rust::prelude::*;
use adk_rust::server::A2aServer;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY must be set in .env or environment");

    let model = GeminiModel::new(api_key, "gemini-2.5-flash")?;

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("a2a-quickstart-agent")
            .description("A minimal A2A-capable AI assistant built with ADK-Rust")
            .instruction("You are a helpful assistant exposed via the A2A protocol. Answer questions clearly and concisely.")
            .model(Arc::new(model))
            .build()?,
    );

    let server = A2aServer::builder()
        .agent(agent)
        .bind_addr("0.0.0.0:8003")
        .build()?;

    println!("🚀 ADK-Rust A2A agent running on http://localhost:8003");
    println!();
    println!("  Endpoints:");
    println!("    GET  http://localhost:8003/.well-known/agent-card.json  — Agent card");
    println!("    POST http://localhost:8003/a2a                          — JSON-RPC (message/send)");
    println!();
    println!("  Test with curl:");
    println!("    curl http://localhost:8003/.well-known/agent-card.json | jq .");
    println!();
    println!("    curl -X POST http://localhost:8003/a2a \\");
    println!("      -H 'Content-Type: application/json' \\");
    println!("      -d '{{\"jsonrpc\":\"2.0\",\"method\":\"message/send\",\"params\":{{\"message\":{{\"role\":\"user\",\"parts\":[{{\"kind\":\"text\",\"text\":\"Hello, what can you do?\"}}],\"messageId\":\"test-1\"}}}},\"id\":\"req-1\"}}'");

    server.serve().await?;
    Ok(())
}
