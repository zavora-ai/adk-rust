#![allow(clippy::collapsible_if)]
//! Image Analysis Example
//!
//! Demonstrates using BeforeModelCallback to inject an image artifact
//! into the LLM request for multimodal analysis.
//!
//! Run:
//!   cd doc-test/artifacts/artifacts_test
//!   GOOGLE_API_KEY=your_key cargo run --bin image_analysis

use adk_artifact::{ArtifactService, InMemoryArtifactService, LoadRequest, SaveRequest};
use adk_core::{BeforeModelResult, Part};
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    println!("Image Analysis Example");
    println!("======================\n");

    // Create artifact service and load the image
    let artifact_service = Arc::new(InMemoryArtifactService::new());
    let image_bytes = std::fs::read("image.jpg")?;
    println!("Loaded image.jpg ({} bytes)", image_bytes.len());

    // Save image as user-scoped artifact
    artifact_service
        .save(SaveRequest {
            app_name: "image_app".to_string(),
            user_id: "user".to_string(),
            session_id: "init".to_string(),
            file_name: "user:photo.jpg".to_string(),
            part: Part::InlineData { data: image_bytes, mime_type: "image/jpeg".to_string() },
            version: None,
        })
        .await?;
    println!("Saved as artifact: user:photo.jpg\n");

    // Clone for callback
    let callback_service = artifact_service.clone();

    let agent = LlmAgentBuilder::new("image_analyst")
        .description("Analyzes images")
        .instruction("You are an image analyst. When the user asks about the image, describe what you see in detail.")
        .model(model)
        // BeforeModel callback injects the image into every request
        .before_model_callback(Box::new(move |_ctx, mut request| {
            let service = callback_service.clone();
            Box::pin(async move {
                // Load the image artifact
                if let Ok(response) = service
                    .load(LoadRequest {
                        app_name: "image_app".to_string(),
                        user_id: "user".to_string(),
                        session_id: "init".to_string(),
                        file_name: "user:photo.jpg".to_string(),
                        version: None,
                    })
                    .await
                {
                    // Inject image into the last user message
                    if let Some(last_content) = request.contents.last_mut() {
                        if last_content.role == "user" {
                            last_content.parts.push(response.part);
                        }
                    }
                }
                Ok(BeforeModelResult::Continue(request))
            })
        }))
        .build()?;

    println!("Ask questions about the image (e.g., 'What do you see?', 'Describe the colors')\n");

    adk_cli::console::run_console(Arc::new(agent), "image_demo".to_string(), "user".to_string())
        .await?;

    Ok(())
}
