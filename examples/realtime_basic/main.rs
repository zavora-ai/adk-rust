//! Basic Realtime API example - Text-only mode.
//!
//! This example demonstrates using the OpenAI Realtime API with text-only
//! output (no audio). This is useful for understanding the basics before
//! adding audio streaming.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example realtime_basic --features realtime-openai
//! ```

use adk_realtime::{RealtimeConfig, RealtimeModel, ServerEvent, openai::OpenAIRealtimeModel};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load environment variables
    dotenvy::dotenv().ok();

    // Get API key from environment
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    println!("=== ADK-Rust Realtime Basic Example ===\n");

    // Create the OpenAI Realtime model
    let model = Arc::new(OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17"));

    // Configure for text-only output (no audio)
    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful assistant. Keep your responses concise and friendly.")
        .with_modalities(vec!["text".to_string()]); // Text only, no audio output

    println!("Connecting to OpenAI Realtime API...");

    // Connect to the realtime session
    let session = model.connect(config).await?;

    println!("Connected! Sending message...\n");

    // Send a text message
    session.send_text("Hello! What can you help me with today?").await?;

    // Request a response
    session.create_response().await?;

    println!("User: Hello! What can you help me with today?\n");
    print!("Assistant: ");

    // Process events from the server
    while let Some(event_result) = session.next_event().await {
        match event_result {
            Ok(event) => match event {
                ServerEvent::TextDelta { delta, .. } => {
                    // Print text as it streams in
                    print!("{}", delta);
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                }
                ServerEvent::ResponseDone { .. } => {
                    // Response is complete
                    println!("\n");
                    break;
                }
                ServerEvent::Error { error, .. } => {
                    eprintln!("\nError: {} - {}", error.error_type, error.message);
                    break;
                }
                ServerEvent::SessionCreated { session, .. } => {
                    if let Some(id) = session.get("id").and_then(|v| v.as_str()) {
                        println!("[Session created: {}]", id);
                    }
                }
                _ => {
                    // Ignore other events for this basic example
                }
            },
            Err(e) => {
                eprintln!("Error receiving event: {}", e);
                break;
            }
        }
    }

    // Send a follow-up message
    session.send_text("Can you tell me a short joke?").await?;
    session.create_response().await?;

    println!("User: Can you tell me a short joke?\n");
    print!("Assistant: ");

    // Process the second response
    while let Some(event_result) = session.next_event().await {
        match event_result {
            Ok(event) => match event {
                ServerEvent::TextDelta { delta, .. } => {
                    print!("{}", delta);
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                }
                ServerEvent::ResponseDone { .. } => {
                    println!("\n");
                    break;
                }
                ServerEvent::Error { error, .. } => {
                    eprintln!("\nError: {} - {}", error.error_type, error.message);
                    break;
                }
                _ => {}
            },
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
        }
    }

    println!("=== Session Complete ===");

    Ok(())
}
