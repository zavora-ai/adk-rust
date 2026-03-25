//! Typed models for the OpenRouter chat-completions API.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

type JsonMap = BTreeMap<String, serde_json::Value>;

/// Native request body for `POST /chat/completions`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterChatRequest {
    pub model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<OpenRouterChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenRouterTool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<OpenRouterToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<Vec<OpenRouterPlugin>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<OpenRouterProviderPreferences>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<OpenRouterReasoningConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<OpenRouterResponseFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_config: Option<OpenRouterImageConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Request or response chat message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterChatMessage {
    #[serde(default)]
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenRouterChatMessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenRouterChatToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<serde_json::Value>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Chat message content, either a string or a list of structured content parts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OpenRouterChatMessageContent {
    Text(String),
    Parts(Vec<OpenRouterChatContentPart>),
}

/// One structured content part inside a chat message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterChatContentPart {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_url: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<serde_json::Value>>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Function tool definition passed through the chat API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterTool {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<OpenRouterFunctionDescription>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// One callable function exposed to the model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterFunctionDescription {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Tool-choice selector used by chat and responses requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OpenRouterToolChoice {
    Mode(String),
    Named {
        #[serde(rename = "type")]
        kind: String,
        function: OpenRouterNamedFunction,
    },
}

/// Named tool-choice target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterNamedFunction {
    pub name: String,
}

/// One OpenRouter plugin declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterPlugin {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Request-time provider preferences for routing and pricing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterProviderPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_parameters: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_collection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforce_distillable_text: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantizations: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_price: Option<OpenRouterProviderMaxPrice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_min_throughput: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_max_latency: Option<f64>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Maximum price constraints for OpenRouter provider routing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterProviderMaxPrice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<serde_json::Value>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Shared reasoning configuration used by chat and responses requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterReasoningConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Replayable reasoning state that can be attached to a later request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterReasoningReplay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<OpenRouterReasoningConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<serde_json::Value>,
}

/// Structured-output request format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterResponseFormat {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Image-generation configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterImageConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Native response body for `POST /chat/completions`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterChatResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<OpenRouterChatChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenRouterChatUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// One chat choice in a completion response or streamed delta.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterChatChoice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<OpenRouterChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<OpenRouterChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Chat response usage payload enriched by OpenRouter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterChatUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_byok: Option<bool>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// One tool call attached to an assistant chat message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterChatToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    #[serde(default)]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<OpenRouterChatToolFunction>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Tool-function payload embedded in a tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterChatToolFunction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}
