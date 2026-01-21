use crate::{Result, types::Content};
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
        Self { model: model.into(), contents, config: None, tools: HashMap::new() }
    }

    /// Set the response schema for structured output.
    pub fn with_response_schema(mut self, schema: serde_json::Value) -> Self {
        let config = self.config.get_or_insert(GenerateContentConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            max_output_tokens: None,
            response_schema: None,
        });
        config.response_schema = Some(schema);
        self
    }

    /// Set the generation config.
    pub fn with_config(mut self, config: GenerateContentConfig) -> Self {
        self.config = Some(config);
        self
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
    fn test_llm_request_with_response_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            }
        });
        let req = LlmRequest::new("test-model", vec![]).with_response_schema(schema.clone());

        assert!(req.config.is_some());
        let config = req.config.unwrap();
        assert!(config.response_schema.is_some());
        assert_eq!(config.response_schema.unwrap(), schema);
    }

    #[test]
    fn test_llm_request_with_config() {
        let config = GenerateContentConfig {
            temperature: Some(0.7),
            top_p: Some(0.9),
            top_k: Some(40),
            max_output_tokens: Some(1024),
            response_schema: None,
        };
        let req = LlmRequest::new("test-model", vec![]).with_config(config);

        assert!(req.config.is_some());
        let config = req.config.unwrap();
        assert_eq!(config.temperature, Some(0.7));
        assert_eq!(config.max_output_tokens, Some(1024));
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
