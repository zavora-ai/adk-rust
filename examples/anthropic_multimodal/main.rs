//! Anthropic Multimodal Example
//!
//! Demonstrates sending images to Claude via URL and inline base64 data.
//! Claude's vision capabilities can describe images, read text in photos,
//! and answer questions about visual content.
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run --example anthropic_multimodal --features anthropic
//! ```

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use futures::StreamExt;
use std::collections::HashMap;

fn make_request(contents: Vec<Content>) -> LlmRequest {
    LlmRequest { model: String::new(), contents, config: None, tools: HashMap::new() }
}

/// Print the text content from a non-streaming response.
async fn print_response(
    client: &AnthropicClient,
    request: LlmRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = client.generate_content(request, false).await?;
    if let Some(Ok(response)) = stream.next().await
        && let Some(content) = &response.content
    {
        for part in &content.parts {
            if let Part::Text { text } = part {
                println!("{text}");
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");

    let client = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-4-20250514"))?;

    // --- Example 1: Image via URL ---
    println!("=== Example 1: Describe an image from URL ===\n");

    let request = make_request(vec![Content {
        role: "user".to_string(),
        parts: vec![
            Part::FileData {
                mime_type: "image/jpeg".to_string(),
                file_uri: "https://upload.wikimedia.org/wikipedia/commons/thumb/3/3a/Cat03.jpg/1200px-Cat03.jpg".to_string(),
            },
            Part::Text {
                text: "Describe this image in 2-3 sentences.".to_string(),
            },
        ],
    }]);
    print_response(&client, request).await?;

    // --- Example 2: Inline base64 image ---
    println!("\n=== Example 2: Analyze a tiny inline PNG ===\n");

    // A minimal 1x1 red PNG pixel (raw bytes)
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
            Part::InlineData { mime_type: "image/png".to_string(), data: red_pixel_png },
            Part::Text { text: "What do you see in this image? It's very small.".to_string() },
        ],
    }]);
    print_response(&client, request).await?;

    // --- Example 3: Multi-image comparison ---
    println!("\n=== Example 3: Compare two images via URL ===\n");

    let request = make_request(vec![Content {
        role: "user".to_string(),
        parts: vec![
            Part::FileData {
                mime_type: "image/jpeg".to_string(),
                file_uri: "https://upload.wikimedia.org/wikipedia/commons/thumb/3/3a/Cat03.jpg/1200px-Cat03.jpg".to_string(),
            },
            Part::FileData {
                mime_type: "image/jpeg".to_string(),
                file_uri: "https://upload.wikimedia.org/wikipedia/commons/thumb/2/26/YellowLabradorLooking_new.jpg/1200px-YellowLabradorLooking_new.jpg".to_string(),
            },
            Part::Text {
                text: "Compare these two animals in one sentence.".to_string(),
            },
        ],
    }]);
    print_response(&client, request).await?;

    // --- Example 4: Unsupported MIME type error handling ---
    println!("\n=== Example 4: Unsupported MIME type (graceful error) ===\n");

    let request = make_request(vec![Content {
        role: "user".to_string(),
        parts: vec![Part::InlineData { mime_type: "audio/wav".to_string(), data: vec![0; 10] }],
    }]);

    // The error surfaces when the stream is polled, not when generate_content is called
    match client.generate_content(request, false).await {
        Ok(mut stream) => match stream.next().await {
            Some(Err(e)) => println!("  Expected error: {e}"),
            Some(Ok(_)) => println!("  Unexpected success"),
            None => println!("  Empty stream"),
        },
        Err(e) => println!("  Expected error: {e}"),
    }

    println!("\nDone!");
    Ok(())
}
