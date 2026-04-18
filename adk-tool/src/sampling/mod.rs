//! MCP sampling callback support for ADK agents.
//!
//! Handles `sampling/createMessage` requests from MCP servers by routing
//! them through the agent's LLM provider.
//!
//! Enable with the `mcp-sampling` feature flag.
//!
//! # Overview
//!
//! The MCP protocol allows servers to request that the client generate a message
//! via `sampling/createMessage`. This module defines the [`SamplingHandler`] trait
//! for handling those requests, along with the wire types [`SamplingRequest`] and
//! [`SamplingResponse`].
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_tool::sampling::{SamplingHandler, SamplingRequest, SamplingResponse};
//! use adk_core::Result;
//!
//! struct MySamplingHandler;
//!
//! #[async_trait::async_trait]
//! impl SamplingHandler for MySamplingHandler {
//!     async fn handle_create_message(
//!         &self,
//!         request: SamplingRequest,
//!     ) -> Result<SamplingResponse> {
//!         // Route to your LLM provider or custom logic
//!         todo!()
//!     }
//! }
//! ```

use adk_core::model::{FinishReason, GenerateContentConfig, LlmRequest};
use adk_core::types::{Content, Part};
use adk_core::{AdkError, Llm, Result};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::debug;

/// Trait for handling MCP `sampling/createMessage` requests.
///
/// Implementors receive the MCP sampling request and return a response.
/// The default implementation (provided by `LlmSamplingHandler` in task 8.2)
/// routes through the agent's LLM provider.
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::sampling::{SamplingHandler, SamplingRequest, SamplingResponse};
/// use adk_core::Result;
///
/// struct MyHandler;
///
/// #[async_trait::async_trait]
/// impl SamplingHandler for MyHandler {
///     async fn handle_create_message(
///         &self,
///         request: SamplingRequest,
///     ) -> Result<SamplingResponse> {
///         // Custom sampling logic
///         todo!()
///     }
/// }
/// ```
#[async_trait::async_trait]
pub trait SamplingHandler: Send + Sync {
    /// Handle a `sampling/createMessage` request from an MCP server.
    ///
    /// Receives the sampling parameters and returns the generated message.
    async fn handle_create_message(&self, request: SamplingRequest) -> Result<SamplingResponse>;
}

// ---------------------------------------------------------------------------
// Wire types — camelCase serialization for MCP protocol compatibility
// ---------------------------------------------------------------------------

/// MCP sampling request parameters.
///
/// Represents the payload of a `sampling/createMessage` request from an MCP
/// server. All optional fields use `skip_serializing_if` to produce minimal
/// JSON on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingRequest {
    /// Conversation messages to include in the sampling context.
    pub messages: Vec<SamplingMessage>,

    /// Optional system prompt to prepend to the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// Optional model preferences for provider selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_preferences: Option<ModelPreferences>,

    /// Maximum number of tokens to generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Sampling temperature (0.0 = deterministic, higher = more random).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

/// MCP sampling response.
///
/// Represents the result of a `sampling/createMessage` request, returned
/// to the MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingResponse {
    /// The generated content.
    pub content: SamplingContent,

    /// Identifier of the model that produced the response.
    pub model: String,

    /// Reason the model stopped generating (e.g. "endTurn", "maxTokens").
    pub stop_reason: String,
}

/// A message in the MCP sampling conversation context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingMessage {
    /// The role of the message author (e.g. "user", "assistant").
    pub role: String,

    /// The content of the message.
    pub content: SamplingContent,
}

/// Content within a sampling message or response.
///
/// Follows the MCP content format with a `type` discriminator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SamplingContent {
    /// Text content.
    #[serde(rename = "text")]
    Text {
        /// The text value.
        text: String,
    },

    /// Image content (base64-encoded).
    #[serde(rename = "image", rename_all = "camelCase")]
    Image {
        /// Base64-encoded image data.
        data: String,
        /// MIME type of the image (e.g. "image/png").
        mime_type: String,
    },
}

/// Model preferences for sampling requests.
///
/// Allows the MCP server to express preferences about which model
/// should handle the sampling request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPreferences {
    /// Hints about preferred models (e.g. model IDs or families).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<ModelHint>,

    /// Preference for cost optimization (0.0 = cheapest, 1.0 = best quality).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_priority: Option<f64>,

    /// Preference for speed optimization (0.0 = fastest, 1.0 = best quality).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_priority: Option<f64>,

    /// Preference for intelligence/quality (0.0 = fastest, 1.0 = smartest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intelligence_priority: Option<f64>,
}

/// A hint about a preferred model for sampling.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelHint {
    /// Optional model name or identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

impl SamplingContent {
    /// Create text content.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_tool::sampling::SamplingContent;
    ///
    /// let content = SamplingContent::text("Hello, world!");
    /// ```
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Create image content from base64-encoded data.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_tool::sampling::SamplingContent;
    ///
    /// let content = SamplingContent::image("iVBOR...", "image/png");
    /// ```
    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::Image { data: data.into(), mime_type: mime_type.into() }
    }
}

impl SamplingMessage {
    /// Create a new sampling message.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_tool::sampling::{SamplingContent, SamplingMessage};
    ///
    /// let msg = SamplingMessage::new("user", SamplingContent::text("What is 2+2?"));
    /// ```
    pub fn new(role: impl Into<String>, content: SamplingContent) -> Self {
        Self { role: role.into(), content }
    }
}

impl SamplingResponse {
    /// Create a new sampling response.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_tool::sampling::{SamplingContent, SamplingResponse};
    ///
    /// let response = SamplingResponse::new(
    ///     SamplingContent::text("4"),
    ///     "gemini-2.0-flash",
    ///     "endTurn",
    /// );
    /// ```
    pub fn new(
        content: SamplingContent,
        model: impl Into<String>,
        stop_reason: impl Into<String>,
    ) -> Self {
        Self { content, model: model.into(), stop_reason: stop_reason.into() }
    }
}

// ---------------------------------------------------------------------------
// LlmSamplingHandler — default handler routing through the agent's LLM
// ---------------------------------------------------------------------------

/// Default sampling handler that routes requests through an LLM provider.
///
/// Converts [`SamplingRequest`] to an [`LlmRequest`], calls
/// [`Llm::generate_content`], and converts the response back to a
/// [`SamplingResponse`].
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::sampling::LlmSamplingHandler;
/// use std::sync::Arc;
///
/// let handler = LlmSamplingHandler::new(my_llm.clone());
/// let response = handler.handle_create_message(request).await?;
/// ```
pub struct LlmSamplingHandler {
    llm: Arc<dyn Llm>,
}

impl LlmSamplingHandler {
    /// Create a new handler that routes sampling requests to the given LLM.
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self { llm }
    }
}

#[async_trait::async_trait]
impl SamplingHandler for LlmSamplingHandler {
    async fn handle_create_message(&self, request: SamplingRequest) -> Result<SamplingResponse> {
        let llm_request = sampling_request_to_llm_request(&request, self.llm.name());
        debug!(
            model = self.llm.name(),
            message_count = request.messages.len(),
            "routing sampling/createMessage to LLM"
        );

        let mut stream = self
            .llm
            .generate_content(llm_request, false)
            .await
            .map_err(|e| AdkError::tool(format!("LLM sampling failed: {e}")))?;

        // Collect the final (non-partial) response from the stream.
        let mut last_response = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(resp) => last_response = Some(resp),
                Err(e) => return Err(AdkError::tool(format!("LLM sampling stream error: {e}"))),
            }
        }

        let llm_response = last_response
            .ok_or_else(|| AdkError::tool("LLM returned empty response for sampling request"))?;

        // Check for LLM-level errors
        if let Some(ref error_message) = llm_response.error_message {
            return Err(AdkError::tool(format!("LLM sampling error: {error_message}")));
        }

        Ok(llm_response_to_sampling_response(llm_response, self.llm.name()))
    }
}

// ---------------------------------------------------------------------------
// Conversion functions
// ---------------------------------------------------------------------------

/// Convert a [`SamplingRequest`] into an [`LlmRequest`].
///
/// Preserves message count/content, system prompt, max tokens, and temperature.
pub fn sampling_request_to_llm_request(request: &SamplingRequest, model_name: &str) -> LlmRequest {
    let mut contents = Vec::with_capacity(request.messages.len() + 1);

    // Add system prompt as the first content entry with role "system"
    if let Some(ref system_prompt) = request.system_prompt {
        contents.push(Content::new("system").with_text(system_prompt.clone()));
    }

    // Convert each SamplingMessage to a Content
    for msg in &request.messages {
        let role = match msg.role.as_str() {
            "assistant" => "model",
            other => other,
        };
        let content = match &msg.content {
            SamplingContent::Text { text } => Content::new(role).with_text(text.clone()),
            SamplingContent::Image { data, mime_type } => Content {
                role: role.to_string(),
                parts: vec![Part::InlineData {
                    mime_type: mime_type.clone(),
                    data: base64_decode_lossy(data),
                }],
            },
        };
        contents.push(content);
    }

    let config = GenerateContentConfig {
        temperature: request.temperature.map(|t| t as f32),
        max_output_tokens: request.max_tokens.map(|t| t as i32),
        ..Default::default()
    };

    LlmRequest {
        model: model_name.to_string(),
        contents,
        config: Some(config),
        tools: Default::default(),
    }
}

/// Convert an [`LlmResponse`] into a [`SamplingResponse`].
///
/// Preserves content text, model identifier, and maps finish reason to MCP
/// stop reason strings.
pub fn llm_response_to_sampling_response(
    response: adk_core::model::LlmResponse,
    model_name: &str,
) -> SamplingResponse {
    let content = response
        .content
        .map(|c| content_to_sampling_content(&c))
        .unwrap_or_else(|| SamplingContent::text(""));

    let stop_reason = match response.finish_reason {
        Some(FinishReason::Stop) => "endTurn".to_string(),
        Some(FinishReason::MaxTokens) => "maxTokens".to_string(),
        Some(FinishReason::Safety) => "safety".to_string(),
        Some(FinishReason::Recitation) => "recitation".to_string(),
        Some(FinishReason::Other) => "other".to_string(),
        None => "endTurn".to_string(),
    };

    SamplingResponse { content, model: model_name.to_string(), stop_reason }
}

/// Extract the first text or image part from a [`Content`] into [`SamplingContent`].
fn content_to_sampling_content(content: &Content) -> SamplingContent {
    for part in &content.parts {
        match part {
            Part::Text { text } => return SamplingContent::text(text.clone()),
            Part::InlineData { mime_type, data } => {
                return SamplingContent::image(base64_encode(data), mime_type.clone());
            }
            _ => continue,
        }
    }
    // Fallback: empty text if no text/image parts found
    SamplingContent::text("")
}

/// Decode base64 data, returning empty vec on failure.
fn base64_decode_lossy(input: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(input).unwrap_or_default()
}

/// Encode bytes to base64.
fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_handler_is_send_sync() {
        fn require_send_sync<T: Send + Sync>() {}
        // Trait objects must be Send + Sync
        require_send_sync::<Box<dyn SamplingHandler>>();
    }

    #[test]
    fn sampling_request_json_round_trip() {
        let request = SamplingRequest {
            messages: vec![SamplingMessage::new("user", SamplingContent::text("What is 2+2?"))],
            system_prompt: Some("You are a math tutor.".to_string()),
            model_preferences: None,
            max_tokens: Some(100),
            temperature: Some(0.0),
        };

        let json = serde_json::to_string(&request).unwrap();
        let parsed: SamplingRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.system_prompt.as_deref(), Some("You are a math tutor."));
        assert_eq!(parsed.max_tokens, Some(100));
        assert_eq!(parsed.temperature, Some(0.0));
    }

    #[test]
    fn sampling_response_json_round_trip() {
        let response =
            SamplingResponse::new(SamplingContent::text("4"), "gemini-2.0-flash", "endTurn");

        let json = serde_json::to_string(&response).unwrap();
        let parsed: SamplingResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.model, "gemini-2.0-flash");
        assert_eq!(parsed.stop_reason, "endTurn");
        match &parsed.content {
            SamplingContent::Text { text } => assert_eq!(text, "4"),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn sampling_request_camel_case_serialization() {
        let request = SamplingRequest {
            messages: vec![],
            system_prompt: Some("test".to_string()),
            model_preferences: Some(ModelPreferences {
                hints: vec![ModelHint { name: Some("gpt-4".to_string()) }],
                cost_priority: Some(0.5),
                speed_priority: None,
                intelligence_priority: Some(0.8),
            }),
            max_tokens: Some(200),
            temperature: Some(0.7),
        };

        let json = serde_json::to_string_pretty(&request).unwrap();

        // Verify camelCase field names
        assert!(json.contains("systemPrompt"));
        assert!(json.contains("modelPreferences"));
        assert!(json.contains("maxTokens"));
        assert!(json.contains("costPriority"));
        assert!(json.contains("intelligencePriority"));

        // Verify snake_case is NOT present
        assert!(!json.contains("system_prompt"));
        assert!(!json.contains("model_preferences"));
        assert!(!json.contains("max_tokens"));
        assert!(!json.contains("cost_priority"));
        assert!(!json.contains("intelligence_priority"));
    }

    #[test]
    fn sampling_response_camel_case_serialization() {
        let response = SamplingResponse::new(SamplingContent::text("hello"), "model-1", "endTurn");

        let json = serde_json::to_string(&response).unwrap();

        assert!(json.contains("stopReason"));
        assert!(!json.contains("stop_reason"));
    }

    #[test]
    fn sampling_content_text_variant() {
        let content = SamplingContent::text("hello");
        let json = serde_json::to_string(&content).unwrap();

        assert!(json.contains(r#""type":"text"#));
        assert!(json.contains(r#""text":"hello"#));

        let parsed: SamplingContent = serde_json::from_str(&json).unwrap();
        match parsed {
            SamplingContent::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("expected text variant"),
        }
    }

    #[test]
    fn sampling_content_image_variant() {
        let content = SamplingContent::image("base64data", "image/png");
        let json = serde_json::to_string(&content).unwrap();

        assert!(json.contains(r#""type":"image"#));
        assert!(json.contains(r#""data":"base64data"#));
        assert!(json.contains(r#""mimeType":"image/png"#));

        let parsed: SamplingContent = serde_json::from_str(&json).unwrap();
        match parsed {
            SamplingContent::Image { data, mime_type } => {
                assert_eq!(data, "base64data");
                assert_eq!(mime_type, "image/png");
            }
            _ => panic!("expected image variant"),
        }
    }

    #[test]
    fn sampling_request_optional_fields_omitted() {
        let request = SamplingRequest {
            messages: vec![],
            system_prompt: None,
            model_preferences: None,
            max_tokens: None,
            temperature: None,
        };

        let json = serde_json::to_string(&request).unwrap();

        // Optional None fields should be omitted
        assert!(!json.contains("systemPrompt"));
        assert!(!json.contains("modelPreferences"));
        assert!(!json.contains("maxTokens"));
        assert!(!json.contains("temperature"));
    }

    #[test]
    fn sampling_message_deserialization_from_mcp_format() {
        // Verify we can parse the MCP wire format from the design doc
        let json = r#"{
            "role": "user",
            "content": { "type": "text", "text": "What is 2+2?" }
        }"#;

        let msg: SamplingMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, "user");
        match &msg.content {
            SamplingContent::Text { text } => assert_eq!(text, "What is 2+2?"),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn model_preferences_empty_hints_omitted() {
        let prefs = ModelPreferences {
            hints: vec![],
            cost_priority: Some(0.5),
            speed_priority: None,
            intelligence_priority: None,
        };

        let json = serde_json::to_string(&prefs).unwrap();
        assert!(!json.contains("hints"));
        assert!(json.contains("costPriority"));
    }

    #[test]
    fn llm_sampling_handler_is_send_sync() {
        fn require_send_sync<T: Send + Sync>() {}
        require_send_sync::<LlmSamplingHandler>();
    }

    #[test]
    fn sampling_request_to_llm_request_preserves_messages() {
        let request = SamplingRequest {
            messages: vec![
                SamplingMessage::new("user", SamplingContent::text("Hello")),
                SamplingMessage::new("assistant", SamplingContent::text("Hi there")),
                SamplingMessage::new("user", SamplingContent::text("How are you?")),
            ],
            system_prompt: None,
            model_preferences: None,
            max_tokens: None,
            temperature: None,
        };

        let llm_req = sampling_request_to_llm_request(&request, "test-model");

        // 3 messages, no system prompt
        assert_eq!(llm_req.contents.len(), 3);
        assert_eq!(llm_req.contents[0].role, "user");
        assert_eq!(llm_req.contents[1].role, "model"); // assistant → model
        assert_eq!(llm_req.contents[2].role, "user");

        // Verify text content preserved
        match &llm_req.contents[0].parts[0] {
            Part::Text { text } => assert_eq!(text, "Hello"),
            _ => panic!("expected text part"),
        }
    }

    #[test]
    fn sampling_request_to_llm_request_preserves_system_prompt() {
        let request = SamplingRequest {
            messages: vec![SamplingMessage::new("user", SamplingContent::text("Hi"))],
            system_prompt: Some("You are a helpful assistant.".to_string()),
            model_preferences: None,
            max_tokens: None,
            temperature: None,
        };

        let llm_req = sampling_request_to_llm_request(&request, "test-model");

        // system prompt + 1 message
        assert_eq!(llm_req.contents.len(), 2);
        assert_eq!(llm_req.contents[0].role, "system");
        match &llm_req.contents[0].parts[0] {
            Part::Text { text } => assert_eq!(text, "You are a helpful assistant."),
            _ => panic!("expected text part"),
        }
    }

    #[test]
    fn sampling_request_to_llm_request_preserves_config() {
        let request = SamplingRequest {
            messages: vec![],
            system_prompt: None,
            model_preferences: None,
            max_tokens: Some(500),
            temperature: Some(0.7),
        };

        let llm_req = sampling_request_to_llm_request(&request, "test-model");

        let config = llm_req.config.unwrap();
        assert_eq!(config.max_output_tokens, Some(500));
        // f64 0.7 → f32 0.7 (approximate)
        assert!((config.temperature.unwrap() - 0.7f32).abs() < 0.001);
    }

    #[test]
    fn llm_response_to_sampling_response_preserves_text() {
        use adk_core::model::{FinishReason, LlmResponse};

        let llm_resp = LlmResponse {
            content: Some(Content::new("model").with_text("The answer is 42.")),
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        };

        let sampling_resp = llm_response_to_sampling_response(llm_resp, "gemini-2.0-flash");

        assert_eq!(sampling_resp.model, "gemini-2.0-flash");
        assert_eq!(sampling_resp.stop_reason, "endTurn");
        match &sampling_resp.content {
            SamplingContent::Text { text } => assert_eq!(text, "The answer is 42."),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn llm_response_to_sampling_response_maps_finish_reasons() {
        use adk_core::model::{FinishReason, LlmResponse};

        let cases = vec![
            (Some(FinishReason::Stop), "endTurn"),
            (Some(FinishReason::MaxTokens), "maxTokens"),
            (Some(FinishReason::Safety), "safety"),
            (Some(FinishReason::Recitation), "recitation"),
            (Some(FinishReason::Other), "other"),
            (None, "endTurn"),
        ];

        for (finish_reason, expected_stop) in cases {
            let llm_resp = LlmResponse {
                content: Some(Content::new("model").with_text("test")),
                finish_reason,
                ..Default::default()
            };
            let sampling_resp = llm_response_to_sampling_response(llm_resp, "model");
            assert_eq!(sampling_resp.stop_reason, expected_stop);
        }
    }

    #[test]
    fn llm_response_to_sampling_response_empty_content() {
        use adk_core::model::LlmResponse;

        let llm_resp = LlmResponse { content: None, ..Default::default() };
        let sampling_resp = llm_response_to_sampling_response(llm_resp, "model");

        match &sampling_resp.content {
            SamplingContent::Text { text } => assert_eq!(text, ""),
            _ => panic!("expected empty text content"),
        }
    }
}
