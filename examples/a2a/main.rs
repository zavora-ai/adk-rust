// A2A (Agent-to-Agent) Protocol Example
//
// This example demonstrates the A2A protocol integration for agent-to-agent communication.
// It shows how to:
// 1. Create an A2A server that exposes an agent via the A2A protocol
// 2. Create a RemoteA2aAgent that can communicate with remote A2A agents
//
// To run this example:
//   GOOGLE_API_KEY=your_key cargo run --example a2a
//
// The server will start and expose:
// - GET  /.well-known/agent.json - Agent card discovery
// - POST /a2a                    - JSON-RPC endpoint for A2A protocol
// - POST /a2a/stream             - SSE streaming endpoint

use adk_agent::LlmAgentBuilder;
use adk_core::SingleAgentLoader;
use adk_model::gemini::GeminiModel;
use adk_server::{RemoteA2aAgent, ServerConfig, create_app_with_a2a};
use adk_session::InMemorySessionService;
use adk_tool::GoogleSearchTool;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing for better logging
    tracing_subscriber::fmt::init();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    // Create a weather agent that will be exposed via A2A
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let weather_agent = LlmAgentBuilder::new("weather_agent")
        .description("Agent to answer questions about weather")
        .instruction("Answer questions about weather in cities using Google Search.")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

    println!("A2A Protocol Example");
    println!("====================\n");

    // Configure the server
    let agent_loader = Arc::new(SingleAgentLoader::new(Arc::new(weather_agent)));
    let session_service = Arc::new(InMemorySessionService::new());

    let config = ServerConfig::new(agent_loader, session_service);

    // Start the A2A server
    let port = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8081);

    let base_url = format!("http://localhost:{}", port);

    // Create app with A2A support enabled
    let app = create_app_with_a2a(config, Some(&base_url));

    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    println!("A2A Server starting on http://localhost:{}", port);
    println!("\nEndpoints:");
    println!("  Agent Card:  GET  http://localhost:{}/.well-known/agent.json", port);
    println!("  A2A Invoke:  POST http://localhost:{}/a2a", port);
    println!("  A2A Stream:  POST http://localhost:{}/a2a/stream", port);
    println!("\nREST API:");
    println!("  Health:      GET  http://localhost:{}/api/health", port);
    println!("  Web UI:      http://localhost:{}/ui/", port);

    println!("\nExample: Fetch agent card with curl:");
    println!("  curl http://localhost:{}/.well-known/agent.json | jq", port);

    println!("\nExample: Send A2A message with curl:");
    println!(
        r#"  curl -X POST http://localhost:{}/a2a \
    -H "Content-Type: application/json" \
    -d '{{
      "jsonrpc": "2.0",
      "method": "message/send",
      "params": {{
        "message": {{
          "role": "user",
          "messageId": "msg-1",
          "parts": [{{"text": "What is the weather in Tokyo?"}}]
        }}
      }},
      "id": 1
    }}'"#,
        port
    );

    println!("\n\nRemoteA2aAgent Usage (for calling this server from another agent):");
    println!(
        r#"
    use adk_server::RemoteA2aAgent;

    let remote_agent = RemoteA2aAgent::builder("remote_weather")
        .description("Remote weather agent via A2A")
        .agent_url("http://localhost:{}")
        .build()?;
"#,
        port
    );

    println!("\nPress Ctrl+C to stop the server\n");

    axum::serve(listener, app).await?;

    Ok(())
}

/// Example of creating a RemoteA2aAgent (not used in main, but shown for documentation)
#[allow(dead_code)]
fn create_remote_agent_example() -> Result<RemoteA2aAgent> {
    // This is how you would create a RemoteA2aAgent to call another A2A server
    let remote_agent = RemoteA2aAgent::builder("remote_weather")
        .description("Remote weather agent via A2A protocol")
        .agent_url("http://localhost:8081")
        .build()?;

    Ok(remote_agent)
}
