use adk_core::{Content, Llm, LlmRequest};
use adk_model::gemini::GeminiModel;

#[tokio::test]
async fn test_gemini_model_creation() {
    let result = GeminiModel::new("test-api-key", "gemini-2.0-flash-exp");
    assert!(result.is_ok());
    
    let model = result.unwrap();
    assert_eq!(model.name(), "gemini-2.0-flash-exp");
}

#[tokio::test]
async fn test_llm_request_creation() {
    let content = Content::new("user").with_text("Hello");
    let request = LlmRequest::new("gemini-2.0-flash-exp", vec![content]);
    
    assert_eq!(request.model, "gemini-2.0-flash-exp");
    assert_eq!(request.contents.len(), 1);
}
