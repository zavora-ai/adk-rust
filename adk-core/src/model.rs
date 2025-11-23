use crate::{types::Content, Result};
use async_trait::async_trait;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;

pub type LlmResponseStream = Pin<Box<dyn Stream<Item = Result<LlmResponse>> + Send>>;

#[async_trait]
pub trait Llm: Send + Sync {
    fn name(&self) -> &str;
    async fn generate_content(&self, req: LlmRequest, stream: bool) -> Result<LlmResponseStream>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub contents: Vec<Content>,
    pub config: Option<GenerateContentConfig>,
    #[serde(skip)]
    pub tools: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateContentConfig {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<i32>,
    pub max_output_tokens: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: Option<Content>,
    pub usage_metadata: Option<UsageMetadata>,
    pub finish_reason: Option<FinishReason>,
    pub partial: bool,
    pub turn_complete: bool,
    pub interrupted: bool,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageMetadata {
    pub prompt_token_count: i32,
    pub candidates_token_count: i32,
    pub total_token_count: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    MaxTokens,
    Safety,
    Recitation,
    Other,
}

impl LlmRequest {
    pub fn new(model: impl Into<String>, contents: Vec<Content>) -> Self {
        Self {
            model: model.into(),
            contents,
            config: None,
            tools: HashMap::new(),
        }
    }
}

impl LlmResponse {
    pub fn new(content: Content) -> Self {
        Self {
            content: Some(content),
            usage_metadata: None,
            finish_reason: Some(FinishReason::Stop),
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
        }
    }
}

// Mock implementation for testing
pub struct MockLlm {
    name: String,
    responses: Vec<LlmResponse>,
}

impl MockLlm {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            responses: vec![],
        }
    }

    pub fn with_response(mut self, response: LlmResponse) -> Self {
        self.responses.push(response);
        self
    }
}

#[async_trait]
impl Llm for MockLlm {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let responses = self.responses.clone();
        let stream = async_stream::stream! {
            for response in responses {
                yield Ok(response);
            }
        };
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_request_creation() {
        let req = LlmRequest::new("test-model", vec![]);
        assert_eq!(req.model, "test-model");
        assert!(req.contents.is_empty());
    }

    #[test]
    fn test_llm_response_creation() {
        let content = Content::new("assistant");
        let resp = LlmResponse::new(content);
        assert!(resp.content.is_some());
        assert!(resp.turn_complete);
        assert!(!resp.partial);
        assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn test_finish_reason() {
        assert_eq!(FinishReason::Stop, FinishReason::Stop);
        assert_ne!(FinishReason::Stop, FinishReason::MaxTokens);
    }

    #[test]
    fn test_mock_llm() {
        let mock = MockLlm::new("test-llm")
            .with_response(LlmResponse::new(Content::new("assistant")));
        assert_eq!(mock.name(), "test-llm");
        assert_eq!(mock.responses.len(), 1);
    }

    #[tokio::test]
    async fn test_mock_llm_generate() {
        use futures::StreamExt;

        let mock = MockLlm::new("test")
            .with_response(LlmResponse::new(Content::new("assistant").with_text("Hello")));
        
        let req = LlmRequest::new("test", vec![]);
        let mut stream = mock.generate_content(req, false).await.unwrap();
        
        let response = stream.next().await.unwrap().unwrap();
        assert!(response.content.is_some());
    }
}
