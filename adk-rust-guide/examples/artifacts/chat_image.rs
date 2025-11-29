//! Image analysis example using BeforeModel callback pattern (adk-go style)
//!
//! This example demonstrates the correct way to load image artifacts for multimodal
//! analysis. Following the adk-go pattern, we use a BeforeModelCallback to inject
//! the image directly into the LLM request, rather than using a tool.
//!
//! Why? Tool responses are JSON text - the model can't "see" images in tool responses.
//! By injecting the image as a Part::InlineData in the request, the model receives
//! the actual image data.

use adk_rust::prelude::*;
use adk_rust::artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    // Use a model that supports vision/multimodal input
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create artifact service and save the image
    let artifact_service = Arc::new(InMemoryArtifactService::new());
    let image_content = std::fs::read("examples/artifacts/test_image.png")?;
    let image_size = image_content.len();

    // Save the image as a user-scoped artifact
    artifact_service.save(SaveRequest {
        app_name: "image_analyst".to_string(),
        user_id: "user".to_string(),
        session_id: "init".to_string(),
        file_name: "user:image.png".to_string(),
        part: Part::InlineData {
            data: image_content,
            mime_type: "image/png".to_string(),
        },
        version: None,
    }).await?;

    // Clone artifact service for use in callback
    let callback_artifact_service = artifact_service.clone();

    let agent = LlmAgentBuilder::new("image_analyst")
        .description("Analyzes images using BeforeModel callback pattern")
        .instruction(
            "You are an image analyst. An image has been provided to you. \
             Describe what you see in the image in detail."
        )
        .model(model)
        // Use BeforeModel callback to inject image into the request (adk-go pattern)
        .before_model_callback(Box::new(move |_ctx, mut request| {
            let artifact_service = callback_artifact_service.clone();
            Box::pin(async move {
                // Load the image artifact
                let load_result = artifact_service.load(adk_rust::artifact::LoadRequest {
                    app_name: "image_analyst".to_string(),
                    user_id: "user".to_string(),
                    session_id: "init".to_string(),
                    file_name: "user:image.png".to_string(),
                    version: None,
                }).await;

                if let Ok(response) = load_result {
                    // Inject the image part into the last user content
                    if let Some(last_content) = request.contents.last_mut() {
                        if last_content.role == "user" {
                            last_content.parts.push(response.part);
                            println!("[CALLBACK] Injected image into user content");
                        }
                    }
                }

                Ok(BeforeModelResult::Continue(request))
            })
        }))
        .build()?;

    if is_interactive_mode() {
        Launcher::new(Arc::new(agent))
            .with_artifact_service(artifact_service)
            .run()
            .await?;
    } else {
        print_validating("Image Analysis Agent (BeforeModel callback pattern)");
        println!("✓ Image file loaded into artifact service: {} bytes", image_size);
        println!("✓ Agent uses BeforeModel callback to inject image into LLM request");
        println!("✓ This follows the adk-go pattern for multimodal artifacts");
        print_success("chat_image");
        println!("\nTry: cargo run --example chat_image -- chat");
        println!("Ask: 'What is in the image?' or 'Describe the image'");
    }

    Ok(())
}
