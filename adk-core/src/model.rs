use crate::schema_adapter::{GenericSchemaAdapter, SchemaAdapter};
use crate::{Result, types::Content};
use async_trait::async_trait;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;

/// A pinned, boxed stream of [`LlmResponse`] results from a model.
pub type LlmResponseStream = Pin<Box<dyn Stream<Item = Result<LlmResponse>> + Send>>;

/// The core trait for all LLM providers.
///
/// Implementations wrap a specific model API (Gemini, OpenAI, Anthropic, etc.)
/// and produce a stream of responses for a given request.
#[async_trait]
pub trait Llm: Send + Sync {
    /// Returns the model identifier (e.g., "gemini-2.5-flash").
    fn name(&self) -> &str;
    /// Generates content from the given request, optionally streaming.
    async fn generate_content(&self, req: LlmRequest, stream: bool) -> Result<LlmResponseStream>;

    /// Returns the schema adapter for this provider.
    ///
    /// The schema adapter normalizes raw JSON Schema from MCP tools into the
    /// format accepted by this provider's function-calling API.
    ///
    /// Default implementation returns [`GenericSchemaAdapter`], which applies
    /// safe transforms suitable for most providers. Override this method to
    /// return a provider-specific adapter (e.g., `GeminiSchemaAdapter`,
    /// `OpenAiStrictSchemaAdapter`).
    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        &GenericSchemaAdapter
    }

    /// Returns `true` if this model is configured to use a server-managed
    /// environment (e.g., Gemini Interactions API) where the provider owns
    /// the tool-calling loop and filesystem.
    ///
    /// This is used at agent build time to detect conflicts with client-side
    /// sandbox tools that would produce competing filesystems.
    ///
    /// Default implementation returns `false`. Override in providers that
    /// support server-managed environments.
    fn uses_interactions_api(&self) -> bool {
        false
    }
}

/// A request to an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    /// The model identifier to use for generation.
    pub model: String,
    /// The conversation contents (system, user, model messages).
    pub contents: Vec<Content>,
    /// Optional generation configuration (temperature, tokens, etc.).
    pub config: Option<GenerateContentConfig>,
    /// Tool declarations keyed by tool name.
    #[serde(skip)]
    pub tools: HashMap<String, serde_json::Value>,
    /// Provider-neutral identifier of the previous response to continue from.
    /// The agent populates this from the most recent event's `interaction_id`.
    /// Maps to `previous_interaction_id` for the Gemini Interactions transport;
    /// ignored by transports that do not support response chaining.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub previous_response_id: Option<String>,
}

/// Configuration for LLM content generation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerateContentConfig {
    /// Sampling temperature (0.0 = deterministic, higher = more random).
    pub temperature: Option<f32>,
    /// Nucleus sampling threshold.
    pub top_p: Option<f32>,
    /// Top-k sampling parameter.
    pub top_k: Option<i32>,
    /// Frequency penalty to reduce repetition.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub frequency_penalty: Option<f32>,
    /// Presence penalty to encourage topic diversity.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub presence_penalty: Option<f32>,
    /// Maximum number of output tokens to generate.
    pub max_output_tokens: Option<i32>,
    /// Random seed for reproducible generation.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub seed: Option<i64>,
    /// Number of top log probabilities to return per token.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub top_logprobs: Option<u8>,
    /// Sequences that stop generation when encountered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    /// JSON Schema for structured output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,

    /// Optional cached content name for Gemini provider.
    /// When set, the Gemini provider attaches this to the generation request.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cached_content: Option<String>,

    /// Provider-specific request options keyed by provider namespace.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extensions: serde_json::Map<String, serde_json::Value>,
}

/// A response from an LLM provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmResponse {
    /// The generated content (text, function calls, etc.).
    pub content: Option<Content>,
    /// Token usage statistics.
    pub usage_metadata: Option<UsageMetadata>,
    /// Reason the model stopped generating.
    pub finish_reason: Option<FinishReason>,
    /// Citation sources referenced in the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citation_metadata: Option<CitationMetadata>,
    /// Whether this is a partial streaming chunk.
    pub partial: bool,
    /// Whether the model has finished its turn.
    pub turn_complete: bool,
    /// Whether the response was interrupted.
    pub interrupted: bool,
    /// Error code from the provider, if any.
    pub error_code: Option<String>,
    /// Error message from the provider, if any.
    pub error_message: Option<String>,
    /// Provider-specific metadata (e.g., response IDs, routing info).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub provider_metadata: Option<serde_json::Value>,
    /// Interactions API: the server-assigned interaction id for this turn, used
    /// to continue the conversation via `previous_interaction_id`. `None` for
    /// the generateContent transport and non-Gemini providers.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub interaction_id: Option<String>,
}

/// Trait for LLM providers that support prompt caching.
///
/// Providers implementing this trait can create and delete cached content
/// resources, enabling automatic prompt caching lifecycle management by the
/// runner. The runner stores an `Option<Arc<dyn CacheCapable>>` alongside the
/// primary `Arc<dyn Llm>` and calls these methods when [`ContextCacheConfig`]
/// is active.
///
/// # Example
///
/// ```rust,ignore
/// use adk_core::CacheCapable;
///
/// let cache_name = model
///     .create_cache("You are a helpful assistant.", &tools, 600)
///     .await?;
/// // ... use cache_name in generation requests ...
/// model.delete_cache(&cache_name).await?;
/// ```
#[async_trait]
pub trait CacheCapable: Send + Sync {
    /// Create a cached content resource from the given system instruction,
    /// tool definitions, and TTL.
    ///
    /// Returns the provider-specific cache name (e.g. `"cachedContents/abc123"`
    /// for Gemini) that can be attached to subsequent generation requests via
    /// [`GenerateContentConfig::cached_content`].
    async fn create_cache(
        &self,
        system_instruction: &str,
        tools: &HashMap<String, serde_json::Value>,
        ttl_seconds: u32,
    ) -> Result<String>;

    /// Delete a previously created cached content resource by name.
    async fn delete_cache(&self, name: &str) -> Result<()>;
}

/// Configuration for automatic prompt caching lifecycle management.
///
/// When set on runner configuration, the runner will automatically create and manage
/// cached content resources for supported providers (currently Gemini).
///
/// # Example
///
/// ```rust
/// use adk_core::ContextCacheConfig;
///
/// let config = ContextCacheConfig {
///     min_tokens: 4096,
///     ttl_seconds: 600,
///     cache_intervals: 3,
/// };
/// assert_eq!(config.min_tokens, 4096);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCacheConfig {
    /// Minimum system instruction + tool token count to trigger caching.
    /// Set to 0 to disable caching.
    pub min_tokens: u32,

    /// Cache time-to-live in seconds.
    /// Set to 0 to disable caching.
    pub ttl_seconds: u32,

    /// Maximum number of LLM invocations before cache refresh.
    /// After this many invocations, the runner creates a new cache
    /// and deletes the old one.
    pub cache_intervals: u32,
}

impl Default for ContextCacheConfig {
    fn default() -> Self {
        Self { min_tokens: 4096, ttl_seconds: 600, cache_intervals: 3 }
    }
}

/// Token usage statistics from an LLM response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageMetadata {
    /// Number of tokens in the prompt.
    pub prompt_token_count: i32,
    /// Number of tokens in the generated response.
    pub candidates_token_count: i32,
    /// Total tokens (prompt + response).
    pub total_token_count: i32,

    /// Tokens read from cache (Gemini/Anthropic).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cache_read_input_token_count: Option<i32>,

    /// Tokens written to cache during this request.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cache_creation_input_token_count: Option<i32>,

    /// Tokens used for thinking/reasoning (thinking models).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub thinking_token_count: Option<i32>,

    /// Audio input tokens (multimodal models).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub audio_input_token_count: Option<i32>,

    /// Audio output tokens (multimodal models).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub audio_output_token_count: Option<i32>,

    /// Estimated cost in USD for this request.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cost: Option<f64>,

    /// Whether this request used a bring-your-own-key provider.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub is_byok: Option<bool>,

    /// Provider-native usage data retained when available, including fields
    /// that are not yet projected into the normalized counters above.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub provider_usage: Option<serde_json::Value>,
}

/// Citation metadata emitted by model providers for source attribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CitationMetadata {
    /// The list of citation sources in this response.
    #[serde(default)]
    pub citation_sources: Vec<CitationSource>,
}

/// One citation source with optional offsets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CitationSource {
    /// URI of the cited source.
    pub uri: Option<String>,
    /// Title of the cited source.
    pub title: Option<String>,
    /// Start character index in the response text.
    pub start_index: Option<i32>,
    /// End character index in the response text.
    pub end_index: Option<i32>,
    /// License of the cited source.
    pub license: Option<String>,
    /// Publication date of the cited source.
    pub publication_date: Option<String>,
}

/// Reason the model stopped generating content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    /// Natural stop (end of response).
    Stop,
    /// Hit the maximum token limit.
    MaxTokens,
    /// Content filtered for safety.
    Safety,
    /// Content blocked due to recitation/copyright.
    Recitation,
    /// Other/unknown reason.
    Other,
}

impl LlmRequest {
    /// Creates a new request with the given model and contents.
    pub fn new(model: impl Into<String>, contents: Vec<Content>) -> Self {
        Self {
            model: model.into(),
            contents,
            config: None,
            tools: HashMap::new(),
            previous_response_id: None,
        }
    }

    /// Set the response schema for structured output.
    pub fn with_response_schema(mut self, schema: serde_json::Value) -> Self {
        let config = self.config.get_or_insert(GenerateContentConfig::default());
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
    /// Creates a complete (non-streaming) response with the given content.
    pub fn new(content: Content) -> Self {
        Self {
            content: Some(content),
            usage_metadata: None,
            finish_reason: Some(FinishReason::Stop),
            citation_metadata: None,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
            provider_metadata: None,
            interaction_id: None,
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
            frequency_penalty: Some(0.2),
            presence_penalty: Some(-0.3),
            max_output_tokens: Some(1024),
            seed: Some(42),
            top_logprobs: Some(5),
            stop_sequences: vec!["END".to_string()],
            ..Default::default()
        };
        let req = LlmRequest::new("test-model", vec![]).with_config(config);

        assert!(req.config.is_some());
        let config = req.config.unwrap();
        assert_eq!(config.temperature, Some(0.7));
        assert_eq!(config.max_output_tokens, Some(1024));
        assert_eq!(config.frequency_penalty, Some(0.2));
        assert_eq!(config.presence_penalty, Some(-0.3));
        assert_eq!(config.seed, Some(42));
        assert_eq!(config.top_logprobs, Some(5));
        assert_eq!(config.stop_sequences, vec!["END"]);
    }

    #[test]
    fn test_llm_response_creation() {
        let content = Content::new("assistant");
        let resp = LlmResponse::new(content);
        assert!(resp.content.is_some());
        assert!(resp.turn_complete);
        assert!(!resp.partial);
        assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
        assert!(resp.citation_metadata.is_none());
        assert!(resp.provider_metadata.is_none());
    }

    #[test]
    fn test_llm_response_deserialize_without_citations() {
        let json = serde_json::json!({
            "content": {
                "role": "model",
                "parts": [{"text": "hello"}]
            },
            "partial": false,
            "turn_complete": true,
            "interrupted": false
        });

        let response: LlmResponse = serde_json::from_value(json).expect("should deserialize");
        assert!(response.citation_metadata.is_none());
    }

    #[test]
    fn test_llm_response_roundtrip_with_citations() {
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("hello")),
            usage_metadata: None,
            finish_reason: Some(FinishReason::Stop),
            citation_metadata: Some(CitationMetadata {
                citation_sources: vec![CitationSource {
                    uri: Some("https://example.com".to_string()),
                    title: Some("Example".to_string()),
                    start_index: Some(0),
                    end_index: Some(5),
                    license: None,
                    publication_date: Some("2026-01-01T00:00:00Z".to_string()),
                }],
            }),
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
            provider_metadata: None,
            interaction_id: None,
        };

        let encoded = serde_json::to_string(&response).expect("serialize");
        let decoded: LlmResponse = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded.citation_metadata, response.citation_metadata);
    }

    #[test]
    fn test_generate_content_config_roundtrip_with_extensions() {
        let mut extensions = serde_json::Map::new();
        extensions.insert(
            "openrouter".to_string(),
            serde_json::json!({
                "provider": {
                    "zdr": true,
                    "order": ["openai", "anthropic"]
                },
                "plugins": [
                    { "id": "web", "enabled": true }
                ]
            }),
        );

        let config = GenerateContentConfig {
            temperature: Some(0.4),
            top_p: Some(0.8),
            top_k: Some(12),
            frequency_penalty: Some(0.1),
            presence_penalty: Some(0.2),
            max_output_tokens: Some(512),
            seed: Some(7),
            top_logprobs: Some(3),
            stop_sequences: vec!["STOP".to_string(), "DONE".to_string()],
            response_schema: Some(serde_json::json!({
                "type": "object",
                "properties": { "answer": { "type": "string" } },
                "required": ["answer"]
            })),
            cached_content: Some("cachedContents/abc123".to_string()),
            extensions,
        };

        let encoded = serde_json::to_string(&config).expect("serialize");
        let decoded: GenerateContentConfig = serde_json::from_str(&encoded).expect("deserialize");

        assert_eq!(decoded.temperature, config.temperature);
        assert_eq!(decoded.top_p, config.top_p);
        assert_eq!(decoded.top_k, config.top_k);
        assert_eq!(decoded.frequency_penalty, config.frequency_penalty);
        assert_eq!(decoded.presence_penalty, config.presence_penalty);
        assert_eq!(decoded.max_output_tokens, config.max_output_tokens);
        assert_eq!(decoded.seed, config.seed);
        assert_eq!(decoded.top_logprobs, config.top_logprobs);
        assert_eq!(decoded.stop_sequences, config.stop_sequences);
        assert_eq!(decoded.response_schema, config.response_schema);
        assert_eq!(decoded.cached_content, config.cached_content);
        assert_eq!(decoded.extensions, config.extensions);
    }

    #[test]
    fn test_llm_response_and_usage_roundtrip_with_provider_metadata() {
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("hello")),
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 20,
                total_token_count: 30,
                cache_read_input_token_count: Some(5),
                cache_creation_input_token_count: Some(2),
                thinking_token_count: Some(3),
                audio_input_token_count: Some(4),
                audio_output_token_count: Some(6),
                cost: Some(0.0125),
                is_byok: Some(true),
                provider_usage: Some(serde_json::json!({
                    "server_tool_use": {
                        "web_search_requests": 1
                    },
                    "prompt_tokens_details": {
                        "video_tokens": 8
                    }
                })),
            }),
            finish_reason: Some(FinishReason::Stop),
            citation_metadata: None,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
            provider_metadata: Some(serde_json::json!({
                "openrouter": {
                    "responseId": "resp_123",
                    "outputItems": 2
                }
            })),
            interaction_id: None,
        };

        let encoded = serde_json::to_string(&response).expect("serialize");
        let decoded: LlmResponse = serde_json::from_str(&encoded).expect("deserialize");

        assert_eq!(decoded.provider_metadata, response.provider_metadata);
        assert_eq!(
            decoded.usage_metadata.as_ref().and_then(|u| u.cost),
            response.usage_metadata.as_ref().and_then(|u| u.cost),
        );
        assert_eq!(
            decoded.usage_metadata.as_ref().and_then(|u| u.is_byok),
            response.usage_metadata.as_ref().and_then(|u| u.is_byok),
        );
        assert_eq!(
            decoded.usage_metadata.as_ref().and_then(|u| u.provider_usage.clone()),
            response.usage_metadata.as_ref().and_then(|u| u.provider_usage.clone()),
        );
    }

    #[test]
    fn test_finish_reason() {
        assert_eq!(FinishReason::Stop, FinishReason::Stop);
        assert_ne!(FinishReason::Stop, FinishReason::MaxTokens);
    }
}
