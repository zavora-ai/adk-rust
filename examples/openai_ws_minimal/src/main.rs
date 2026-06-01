//! Minimal WebSocket transport example for the OpenAI Responses API.
//!
//! Demonstrates the lowest-level WebSocket transport usage without the
//! Runner/Agent abstraction — establishes a persistent WebSocket connection,
//! sends a single prompt, and streams the response to stdout.
//!
//! # Running
//!
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --manifest-path examples/openai_ws_minimal/Cargo.toml
//! ```

use adk_model::openai::{OpenAIResponsesConfig, WsTransport};
use adk_rust::Part;
use async_openai::types::responses::{CreateResponseArgs, InputParam};
use futures::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    println!("═══════════════════════════════════════════════════");
    println!("  OpenAI WebSocket Minimal — Low-Latency Transport");
    println!("═══════════════════════════════════════════════════");
    println!();

    let api_key = std::env::var("OPENAI_API_KEY")
        .expect("OPENAI_API_KEY must be set — see .env.example");

    // Configure the Responses API client for WebSocket transport
    let config = OpenAIResponsesConfig::new(&api_key, "gpt-4.1-nano");

    // Establish a persistent WebSocket connection
    println!("Connecting to OpenAI Responses API via WebSocket...");
    let transport = WsTransport::connect(&config).await?;
    println!("Connected!\n");

    // Build a simple request with a single user prompt
    let request = CreateResponseArgs::default()
        .model("gpt-4.1-nano")
        .input(InputParam::Text(
            "Explain in one sentence why WebSockets are useful for AI APIs.".to_string(),
        ))
        .build()?;

    // Send the request and stream the response
    print!("Response: ");
    let mut stream = transport.send_request(request).await?;
    while let Some(response) = stream.next().await {
        let response = response?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                }
            }
        }
    }
    println!();

    println!();
    println!("✅ Example completed successfully.");
    Ok(())
}
