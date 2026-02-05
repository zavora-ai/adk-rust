use adk_core::{Content, LlmRequest};
use adk_model::gemini::{GeminiModel, RetryConfig};
use std::time::Duration;

#[tokio::test]
async fn test_gemini_model_creation() {
    fn accepts_sync_constructor<F>(_f: F)
    where
        F: Fn(&str, &str) -> adk_core::Result<GeminiModel>,
    {
    }

    accepts_sync_constructor(|api_key, model| GeminiModel::new(api_key, model));
}

#[tokio::test]
async fn test_llm_request_creation() {
    let content = Content::new("user").with_text("Hello");
    let request = LlmRequest::new("gemini-2.5-flash", vec![content]);

    assert_eq!(request.model, "gemini-2.5-flash");
    assert_eq!(request.contents.len(), 1);
}

#[tokio::test]
async fn test_retry_config_is_additive() {
    let retry_config = RetryConfig::default()
        .with_max_retries(5)
        .with_initial_delay(Duration::from_millis(50))
        .with_max_delay(Duration::from_secs(1));

    assert!(retry_config.enabled);
    assert_eq!(retry_config.max_retries, 5);
}
