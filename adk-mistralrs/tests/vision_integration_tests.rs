//! Integration tests for vision model support.
//!
//! These tests validate the MistralRsVisionModel implementation with actual models.
//! Vision models interpret/describe images, not generate them.
//!
//! **Validates: Requirements 5.1, 5.2, 5.3**
//!
//! Run with: cargo test -p adk-mistralrs --test vision_integration_tests -- --ignored --nocapture

use adk_core::Llm;
use adk_mistralrs::{MistralRsVisionModel, QuantizationLevel};
use std::path::PathBuf;

// Test model - Qwen3-VL-4B (user requested)
const VISION_MODEL: &str = "Qwen/Qwen2-VL-2B-Instruct";

/// Get the path to the test image in the models folder
fn get_test_image_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/image.jpg")
}

/// Load the test image from the models folder
fn load_test_image() -> image::DynamicImage {
    let path = get_test_image_path();
    image::open(&path).unwrap_or_else(|_| panic!("Failed to load test image from {:?}", path))
}

// =============================================================================
// Integration Tests
// =============================================================================

#[tokio::test]
#[ignore = "Requires HuggingFace auth and model download - run manually"]
async fn test_vision_model_load() {
    println!("Testing vision model loading: {}", VISION_MODEL);

    let model =
        MistralRsVisionModel::from_hf(VISION_MODEL).await.expect("Failed to load vision model");

    assert_eq!(model.name(), VISION_MODEL);
    println!("✓ Vision model loaded successfully: {}", model.name());
}

/// Test vision model describing an image.
///
/// This test loads a local image and asks the model to describe it.
/// The image should be placed at `adk-mistralrs/models/image.jpg`.
#[tokio::test]
#[ignore = "Requires HuggingFace auth and model download - run manually"]
async fn test_vision_describe_image() {
    println!("Testing vision model describing an image...");
    println!("Model: {}", VISION_MODEL);
    println!("Image path: {:?}", get_test_image_path());

    // Load model with ISQ quantization to reduce memory
    println!("Loading model with Q4K quantization...");
    let model = MistralRsVisionModel::from_hf_with_isq(VISION_MODEL, QuantizationLevel::Q4K)
        .await
        .expect("Failed to load vision model");

    // Load the test image
    let test_image = load_test_image();
    println!("Image loaded: {}x{} pixels", test_image.width(), test_image.height());

    // Ask the model to describe the image
    println!("Generating description...");
    let response = model
        .generate_with_image(
            "Describe what you see in this image. Be specific about objects, colors, and any text visible.",
            vec![test_image],
        )
        .await
        .expect("Failed to generate response");

    println!("\n=== Model Response ===");
    println!("{}", response);
    println!("======================\n");

    assert!(!response.is_empty(), "Response should not be empty");
    println!("✓ Vision description test passed");
}

/// Test vision model with streaming response.
#[tokio::test]
#[ignore = "Requires HuggingFace auth and model download - run manually"]
async fn test_vision_streaming() {
    use adk_core::{Content, LlmRequest, Part};
    use futures::StreamExt;

    println!("Testing vision model with streaming...");
    println!("Model: {}", VISION_MODEL);

    let model = MistralRsVisionModel::from_hf_with_isq(VISION_MODEL, QuantizationLevel::Q4K)
        .await
        .expect("Failed to load vision model");

    // Load image and encode as inline data
    let image_path = get_test_image_path();
    let image_bytes = std::fs::read(&image_path).expect("Failed to read image file");

    // Create request with image as inline data
    let content = Content {
        role: adk_core::types::Role::User,
        parts: vec![
            Part::InlineData { mime_type: "image/jpeg".parse().unwrap(), data: image_bytes.into() },
            Part::text("What is in this image? Answer briefly.".to_string()),
        ],
    };

    let request = LlmRequest::new("test", vec![content]);

    println!("Streaming response...");
    let mut stream =
        model.generate_content(request, true).await.expect("Failed to start streaming");

    let mut full_response = String::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(response) => {
                if let Some(content) = &response.content {
                    for part in &content.parts {
                        if let Some(text) = part.as_text() {
                            print!("{}", text);
                            full_response.push_str(text);
                        }
                    }
                }
                if response.turn_complete {
                    println!("\n[Stream complete]");
                }
            }
            Err(e) => {
                eprintln!("Stream error: {}", e);
                break;
            }
        }
    }

    assert!(!full_response.is_empty(), "Should have received some response");
    println!("✓ Vision streaming test passed");
}

#[tokio::test]
#[ignore = "Requires HuggingFace auth and model download - run manually"]
async fn test_vision_llm_trait_text_only() {
    use adk_core::{Content, LlmRequest};
    use futures::StreamExt;

    println!("Testing vision model via Llm trait (text-only)...");

    let model =
        MistralRsVisionModel::from_hf(VISION_MODEL).await.expect("Failed to load vision model");

    // Create a text-only request to verify basic Llm trait works
    let content = Content::new("user").with_text("Say hello in one word.");
    let request = LlmRequest::new("test", vec![content]);

    let stream = model.generate_content(request, false).await.expect("Failed to generate content");

    let responses: Vec<_> = stream.collect().await;
    assert!(!responses.is_empty(), "Should have at least one response");

    let has_content =
        responses.iter().any(|r| r.as_ref().map(|resp| resp.content.is_some()).unwrap_or(false));
    assert!(has_content, "At least one response should have content");

    println!("✓ Vision Llm trait test passed");
}
