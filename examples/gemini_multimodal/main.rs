//! Gemini Multimodal Example
//!
//! Demonstrates sending images to Gemini via inline base64 data and through
//! an LlmAgent with multimodal content. Shows both direct LLM usage and the
//! agent pattern for vision tasks.
//!
//! ```bash
//! export GOOGLE_API_KEY=...
//! cargo run --example gemini_multimodal
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::gemini::GeminiModel;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

fn make_request(contents: Vec<Content>) -> LlmRequest {
    LlmRequest { model: String::new(), contents, config: None, tools: HashMap::new() }
}

/// Print text content from a non-streaming response.
async fn print_response(
    model: &GeminiModel,
    request: LlmRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = model.generate_content(request, false).await?;
    while let Some(Ok(response)) = stream.next().await {
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                }
            }
        }
    }
    println!();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // --- Example 1: Inline image (1x1 red PNG pixel) ---
    println!("=== Example 1: Describe an inline image ===\n");

    let red_pixel_png: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC, 0x33, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    let request = make_request(vec![Content {
        role: "user".to_string(),
        parts: vec![
            Part::InlineData { mime_type: "image/png".to_string(), data: red_pixel_png.clone() },
            Part::Text { text: "What do you see in this image? It's very small.".to_string() },
        ],
    }]);
    print_response(&model, request).await?;

    // --- Example 2: Multiple inline images ---
    println!("\n=== Example 2: Compare two inline images ===\n");

    // Minimal 1x1 blue PNG pixel
    let blue_pixel_png: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0x0F, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC, 0x33, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    let request = make_request(vec![Content {
        role: "user".to_string(),
        parts: vec![
            Part::InlineData { mime_type: "image/png".to_string(), data: red_pixel_png.clone() },
            Part::InlineData { mime_type: "image/png".to_string(), data: blue_pixel_png },
            Part::Text {
                text: "I sent you two tiny 1x1 pixel images. Can you tell what colors they are?"
                    .to_string(),
            },
        ],
    }]);
    print_response(&model, request).await?;

    // --- Example 3: Multimodal through an LlmAgent ---
    println!("\n=== Example 3: Vision agent with inline image ===\n");

    let agent_model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;
    let agent = LlmAgentBuilder::new("vision_agent")
        .description("An agent that analyzes images and answers questions about visual content.")
        .instruction("You are a helpful vision assistant. Describe images accurately and concisely. When analyzing images, mention colors, shapes, and any text you can see.")
        .model(Arc::new(agent_model))
        .build()?;

    // Send multimodal content through the agent's console
    let app_name = "gemini_multimodal".to_string();
    let user_id = "user1".to_string();

    println!("Starting vision agent console. Try pasting image descriptions or ask about images.");
    println!("(The agent accepts text input via console — for programmatic multimodal input,");
    println!(" use the LLM client directly as shown in Examples 1 and 2 above.)\n");

    adk_cli::console::run_console(Arc::new(agent), app_name, user_id).await?;

    Ok(())
}
