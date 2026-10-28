//! Responses API client (`POST /v1/responses`): the stateful surface.
//!
//! Unlike Chat Completions (stateless, no server memory), Responses is the only
//! endpoint carrying reasoning across turns: `previous_response_id` chains turns,
//! `store` persists them server-side, and `background` runs long generations
//! asynchronously. This is the loop the legal-agent-workshop pattern is built on.
//!
//! The workshop loop, in ADK terms: one [`MuseResponsesClient::create`] per turn
//! with the previous response id, one tool at a time, progress saved between turns.

use super::config::MuseConfig;
use adk_core::{AdkError, ErrorComponent};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One Responses input item (`input_text`, `input_image`, `input_file`,
/// `function_call`, `function_call_output`, ...). Untagged JSON, API-shaped.
pub type ResponseItem = Value;

/// A Responses generation request.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ResponseRequest {
    /// Model (for example `muse-spark-1.3`).
    pub model: String,
    /// Text input, or structured input items via [`ResponseRequest::with_items`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// Per-request instructions (not persisted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Chain onto a stored response (carries reasoning across turns).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    /// Persist the response server-side (default true server-side).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    /// Run asynchronously (poll with [`MuseResponsesClient::get`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Nucleus sampling threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Output cap (minimum 16 server-side).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Function/custom tools (caller-executed) or server-side built-ins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    /// Tool choice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    /// Whether tools may run in parallel in one turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// Output format, for example strict JSON Schema via `text.format`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
    /// Include values, for example `reasoning.encrypted_content`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
}

impl ResponseRequest {
    /// A request with plain-text input.
    pub fn text(model: impl Into<String>, input: impl Into<String>) -> Self {
        Self { model: model.into(), input: Some(Value::String(input.into())), ..Default::default() }
    }

    /// Attach structured input items (images, files, function outputs).
    pub fn with_items(mut self, items: Vec<ResponseItem>) -> Self {
        self.input = Some(Value::Array(items));
        self
    }

    /// Chain onto a stored response.
    pub fn with_previous(mut self, previous_response_id: impl Into<String>) -> Self {
        self.previous_response_id = Some(previous_response_id.into());
        self
    }

    /// Constrain output to a strict JSON Schema (Responses `text.format`).
    pub fn with_json_schema(mut self, name: &str, schema: Value) -> Self {
        self.text = Some(serde_json::json!({
            "format": {"type": "json_schema", "name": name, "schema": schema, "strict": true},
        }));
        self
    }
}

/// One function call the model wants executed.
#[derive(Debug, Clone)]
pub struct FunctionCallItem {
    /// Call id to answer with `function_call_output`.
    pub call_id: String,
    /// Function name.
    pub name: String,
    /// Raw JSON arguments.
    pub arguments: String,
}

/// A Responses generation.
#[derive(Debug, Clone, Deserialize)]
pub struct ResponseObject {
    /// Response id (`resp-..`), chainable via `previous_response_id`.
    pub id: String,
    /// Lifecycle status (`completed`, `failed`, `in_progress`, ...).
    #[serde(default)]
    pub status: Option<String>,
    /// Error object when the generation failed.
    #[serde(default)]
    pub error: Option<Value>,
    /// Output items (messages, function calls, reasoning).
    #[serde(default)]
    pub output: Vec<Value>,
    /// Usage object as received.
    #[serde(default)]
    pub usage: Option<Value>,
}

impl ResponseObject {
    /// All `output_text` content, joined.
    pub fn text(&self) -> String {
        let mut texts = Vec::new();
        for item in &self.output {
            if item.get("type").and_then(Value::as_str) == Some("message")
                && let Some(content) = item.get("content").and_then(Value::as_array)
            {
                for part in content {
                    if part.get("type").and_then(Value::as_str) == Some("output_text")
                        && let Some(text) = part.get("text").and_then(Value::as_str)
                    {
                        texts.push(text);
                    }
                }
            }
        }
        texts.join("\n")
    }

    /// Function calls the caller must execute and answer.
    pub fn function_calls(&self) -> Vec<FunctionCallItem> {
        self.output
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .filter_map(|item| {
                Some(FunctionCallItem {
                    call_id: item.get("call_id")?.as_str()?.to_string(),
                    name: item.get("name")?.as_str()?.to_string(),
                    arguments: item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
            })
            .collect()
    }

    /// Normalized usage, when present.
    pub fn usage_metadata(&self) -> Option<adk_core::UsageMetadata> {
        use crate::usage_json::count;

        // Responses counts `input_tokens`/`output_tokens`, not Chat Completions'
        // `prompt_tokens`/`completion_tokens`.
        let usage = self.usage.as_ref().filter(|usage| usage.is_object())?;
        let input = count(usage, "/input_tokens").unwrap_or_default();
        let output = count(usage, "/output_tokens").unwrap_or_default();
        Some(adk_core::UsageMetadata {
            prompt_token_count: input,
            candidates_token_count: output,
            total_token_count: count(usage, "/total_tokens")
                .unwrap_or_else(|| input.saturating_add(output)),
            cache_read_input_token_count: count(usage, "/input_tokens_details/cached_tokens"),
            thinking_token_count: count(usage, "/output_tokens_details/reasoning_tokens"),
            provider_usage: Some(usage.clone()),
            ..Default::default()
        })
    }

    /// Whether the generation finished successfully.
    pub fn is_completed(&self) -> bool {
        self.status.as_deref() == Some("completed")
    }
}

/// Responses API client.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::muse::{MuseConfig, MuseResponsesClient, ResponseRequest};
///
/// let responses = MuseResponsesClient::new(MuseConfig::spark_1_3(api_key))?;
/// let first = responses.create(ResponseRequest::text("muse-spark-1.3", "Plan the memo.")).await?;
/// let next = responses
///     .create(ResponseRequest::text("muse-spark-1.3", "Now execute step one.").with_previous(&first.id))
///     .await?;
/// ```
pub struct MuseResponsesClient {
    client: Client,
    config: MuseConfig,
}

impl MuseResponsesClient {
    /// Create a Responses client.
    pub fn new(config: MuseConfig) -> Result<Self, AdkError> {
        let client = crate::HttpTimeouts::default()
            .apply(Client::builder())
            .build()
            .map_err(|e| AdkError::model(format!("Failed to create HTTP client: {e}")))?;
        Ok(Self { client, config })
    }

    /// Create a response (one turn).
    ///
    /// # Errors
    ///
    /// Returns the API error (HTTP status, code `model.muse.api_error`) on rejection.
    pub async fn create(&self, request: ResponseRequest) -> Result<ResponseObject, AdkError> {
        let response = self
            .client
            .post(format!("{}/responses", self.base()))
            .header("Authorization", self.auth())
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        self.read_json(response).await
    }

    /// Retrieve a stored response (poll background generations here).
    ///
    /// # Errors
    ///
    /// Returns the API error on rejection.
    pub async fn get(&self, response_id: &str) -> Result<ResponseObject, AdkError> {
        let response = self
            .client
            .get(format!("{}/responses/{response_id}", self.base()))
            .header("Authorization", self.auth())
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        self.read_json(response).await
    }

    /// Delete a stored response.
    ///
    /// # Errors
    ///
    /// Returns the API error on rejection.
    pub async fn delete(&self, response_id: &str) -> Result<(), AdkError> {
        let response = self
            .client
            .delete(format!("{}/responses/{response_id}", self.base()))
            .header("Authorization", self.auth())
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        if !response.status().is_success() {
            return Err(self.api_error(response).await);
        }
        Ok(())
    }

    /// Cancel a background response.
    ///
    /// # Errors
    ///
    /// Returns the API error on rejection.
    pub async fn cancel(&self, response_id: &str) -> Result<ResponseObject, AdkError> {
        let response = self
            .client
            .post(format!("{}/responses/{response_id}/cancel", self.base()))
            .header("Authorization", self.auth())
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        self.read_json(response).await
    }

    /// Count input tokens for a request without generating.
    ///
    /// # Errors
    ///
    /// Returns the API error on rejection.
    pub async fn input_tokens(&self, request: &ResponseRequest) -> Result<u64, AdkError> {
        let response = self
            .client
            .post(format!("{}/responses/input_tokens", self.base()))
            .header("Authorization", self.auth())
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        let body: Value = self.read_json(response).await?;
        body.get("input_tokens")
            .and_then(Value::as_u64)
            .ok_or_else(|| AdkError::model("input_tokens response carried no count"))
    }

    fn base(&self) -> String {
        self.config.effective_base_url().trim_end_matches('/').to_string()
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.config.api_key)
    }

    fn transport(&self, error: reqwest::Error) -> AdkError {
        AdkError::new(
            ErrorComponent::Model,
            crate::http_timeouts::transport_category(&error),
            "model.muse.request",
            format!("Muse responses request failed: {error}"),
        )
        .with_provider("muse")
    }

    async fn api_error(&self, response: reqwest::Response) -> AdkError {
        let status = response.status();
        let status_code = status.as_u16();
        let error_text = response.text().await.unwrap_or_default();
        AdkError::new(
            ErrorComponent::Model,
            crate::retry::category_for_status_code(status_code),
            "model.muse.api_error",
            format!(
                "Muse responses API error (HTTP {status}): {}",
                adk_core::upstream_excerpt(&error_text)
            ),
        )
        .with_upstream_status(status_code)
        .with_provider("muse")
    }

    async fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, AdkError> {
        if !response.status().is_success() {
            return Err(self.api_error(response).await);
        }
        let text = response
            .text()
            .await
            .map_err(|e| AdkError::model(format!("Failed to read responses body: {e}")))?;
        serde_json::from_str(&text).map_err(|e| {
            AdkError::model(format!(
                "Failed to parse responses body: {}",
                adk_core::json_error_summary(&e)
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_text_and_calls_are_extracted() {
        let response: ResponseObject = serde_json::from_value(serde_json::json!({
            "id": "resp-1",
            "status": "completed",
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "Memo plan ready."}]},
                {"type": "function_call", "call_id": "call-1", "name": "analyze_document", "arguments": "{\"id\":\"D1\"}"},
            ],
            "usage": {
                "input_tokens": 10,
                "input_tokens_details": {"cached_tokens": 4},
                "output_tokens": 5,
                "output_tokens_details": {"reasoning_tokens": 3},
                "total_tokens": 15,
            },
        }))
        .unwrap();
        assert!(response.is_completed());
        assert_eq!(response.text(), "Memo plan ready.");
        let usage = response.usage_metadata().unwrap();
        assert_eq!(
            (
                usage.prompt_token_count,
                usage.candidates_token_count,
                usage.total_token_count,
                usage.cache_read_input_token_count,
                usage.thinking_token_count,
            ),
            (10, 5, 15, Some(4), Some(3))
        );
        let calls = response.function_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "analyze_document");
    }

    #[test]
    fn request_chains_and_constrains() {
        let request = ResponseRequest::text("muse-spark-1.3", "next step")
            .with_previous("resp-1")
            .with_json_schema("memo", serde_json::json!({"type": "object"}));
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire["previous_response_id"], "resp-1");
        assert_eq!(wire["text"]["format"]["type"], "json_schema");
    }
}
