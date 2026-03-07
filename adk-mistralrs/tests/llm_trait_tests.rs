//! Property tests for Llm trait implementation.
//!
//! **Property 1: Llm Trait Implementation Completeness**
//! *For any* valid `MistralRsConfig`, creating a `MistralRsModel` and calling
//! `generate_content` with a valid `LlmRequest` SHALL return a non-empty `LlmResponseStream`.
//!
//! **Validates: Requirements 1.1, 1.2, 1.3**
//!
//! Note: Full integration tests require actual model files and are marked as ignored.
//! These tests validate the trait implementation structure and type compatibility.

use adk_core::{Content, Llm, LlmRequest};
use adk_mistralrs::{MistralRsConfig, MistralRsModel, ModelSource};

/// Verify MistralRsModel implements Llm trait (compile-time check)
fn _assert_llm_impl<T: Llm>() {}

#[test]
fn test_mistralrs_model_implements_llm_trait() {
    // This is a compile-time check that MistralRsModel implements Llm
    _assert_llm_impl::<MistralRsModel>();
}

#[test]
fn test_llm_request_creation() {
    // Test that LlmRequest can be created with Content
    let content = Content::new("user").with_text("Hello, world!");
    let request = LlmRequest::new("test-model", vec![content]);

    assert_eq!(request.model, "test-model");
    assert_eq!(request.contents.len(), 1);
    assert_eq!(request.contents[0].role, "user");
}

#[test]
fn test_config_for_llm_creation() {
    // Test that config can be built for model creation
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("mistralai/Magistral-Small-2509"))
        .temperature(0.7)
        .max_tokens(1024)
        .build();

    assert!(matches!(config.model_source, ModelSource::HuggingFace(_)));
    assert_eq!(config.temperature, Some(0.7));
    assert_eq!(config.max_tokens, Some(1024));
}

/// Integration test that requires actual model - marked as ignored
/// Run with: cargo test --test llm_trait_tests -- --ignored
#[tokio::test]
#[ignore = "Requires actual model download - run manually"]
async fn test_llm_generate_content_integration() {
    // This test requires an actual model to be downloaded
    // It validates Property 1: Llm Trait Implementation Completeness
    let model = MistralRsModel::from_hf("mistralai/Magistral-Small-2509")
        .await
        .expect("Failed to load model");

    let content = Content::new("user").with_text("Say hello in one word.");
    let request = LlmRequest::new("test", vec![content]);

    let stream = model.generate_content(request, false).await.expect("Failed to generate content");

    // Collect stream results
    use futures::StreamExt;
    let responses: Vec<_> = stream.collect().await;

    assert!(!responses.is_empty(), "Response stream should not be empty");

    // Check that at least one response has content
    let has_content =
        responses.iter().any(|r| r.as_ref().map(|resp| resp.content.is_some()).unwrap_or(false));
    assert!(has_content, "At least one response should have content");
}
