//! Direct image test - sends image directly to model without tools
//! This tests that the GeminiModel correctly handles InlineData parts.

use adk_rust::prelude::*;
use adk_rust::runner::{Runner, RunnerConfig};
use adk_rust::session::{InMemorySessionService, SessionService, CreateRequest};
use adk_rust_guide::init_env;
use futures::StreamExt;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();

    // Use a vision-capable model
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Read the test image
    let image_bytes = std::fs::read("examples/artifacts/test_image.png")?;
    println!("Loaded image: {} bytes", image_bytes.len());

    // Create a simple agent
    let agent = Arc::new(LlmAgentBuilder::new("image_viewer")
        .description("Views and describes images")
        .instruction("Describe what you see in the image provided by the user.")
        .model(model)
        .build()?);

    // Create runner with proper config
    let session_service = Arc::new(InMemorySessionService::new());

    // Create a session first
    session_service.create(CreateRequest {
        app_name: "direct_image_test".to_string(),
        user_id: "test_user".to_string(),
        session_id: Some("session_1".to_string()),
        state: std::collections::HashMap::new(),
    }).await?;

    let runner = Runner::new(RunnerConfig {
        app_name: "direct_image_test".to_string(),
        agent: agent,
        session_service: session_service,
        artifact_service: None,
        memory_service: None,
    })?;

    // Create user content with both text and image
    let user_content = Content {
        role: "user".to_string(),
        parts: vec![
            Part::Text { text: "What is in this image?".to_string() },
            Part::InlineData {
                mime_type: "image/png".to_string(),
                data: image_bytes,
            },
        ],
    };

    println!("\nSending image to model...\n");

    // Run the agent
    let mut stream = runner
        .run("test_user".to_string(), "session_1".to_string(), user_content)
        .await?;

    // Collect response
    print!("Model response: ");
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                if let Some(content) = &event.llm_response.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            print!("{}", text);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("\nError: {}", e);
                break;
            }
        }
    }
    println!("\n");

    Ok(())
}
