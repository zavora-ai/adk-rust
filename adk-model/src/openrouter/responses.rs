//! Typed models for the OpenRouter Responses API.

use crate::openrouter::chat::{
    OpenRouterImageConfig, OpenRouterPlugin, OpenRouterProviderPreferences,
    OpenRouterReasoningConfig, OpenRouterToolChoice,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

type JsonMap = BTreeMap<String, serde_json::Value>;

/// Native request body for `POST /responses`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterResponsesRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<OpenRouterResponseInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenRouterResponseTool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<OpenRouterToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<OpenRouterResponseTextConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<OpenRouterReasoningConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_config: Option<OpenRouterImageConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<OpenRouterProviderPreferences>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<Vec<OpenRouterPlugin>>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Responses input, either a plain string or a structured list of input items.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OpenRouterResponseInput {
    Text(String),
    Items(Vec<OpenRouterResponseInputItem>),
}

/// One structured input item in a Responses API request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterResponseInputItem {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenRouterResponseInputContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// One input item's content, either plain text or structured multimodal parts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OpenRouterResponseInputContent {
    Text(String),
    Parts(Vec<OpenRouterResponseInputContentPart>),
}

/// One multimodal content part in a Responses API item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterResponseInputContentPart {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio: Option<serde_json::Value>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Native Responses API tool declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterResponseTool {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Text-output configuration for Responses requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterResponseTextConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<String>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Native response body for `POST /responses`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<OpenRouterResponseOutputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenRouterResponsesUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// One output item returned by the Responses API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterResponseOutputItem {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<serde_json::Value>>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Usage metadata returned by the Responses API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterResponsesUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_byok: Option<bool>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}
