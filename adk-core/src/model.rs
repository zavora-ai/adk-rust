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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,
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
}
