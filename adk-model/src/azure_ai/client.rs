//! Azure AI Inference client implementation.

use super::config::AzureAIConfig;
use super::convert;
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{AdkError, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;

/// Azure AI Inference client for models hosted on Azure AI endpoints.
///
/// Supports models like Cohere, Llama, and Mistral deployed via Azure AI
/// Inference. Uses `api-key` header authentication and the Azure AI chat
/// completions REST API.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
///
/// let config = AzureAIConfig::new(
///     "https://my-endpoint.eastus.inference.ai.azure.com",
///     "my-api-key",
///     "meta-llama-3.1-8b-instruct",
/// );
/// let client = AzureAIClient::new(config)?;
/// ```
pub struct AzureAIClient {
    client: Client,
    endpoint: String,
    api_key: String,
    model: String,
    retry_config: RetryConfig,
}

impl AzureAIClient {
    /// Create a new Azure AI Inference client from the given config.
    pub fn new(config: AzureAIConfig) -> Result<Self, AdkError> {
        let client = Client::builder()
            .build()
            .map_err(|e| AdkError::Model(format!("Failed to create HTTP client: {e}")))?;

        Ok(Self {
            client,
            endpoint: config.endpoint,
            api_key: config.api_key,
            model: config.model,
            retry_config: RetryConfig::default(),
        })
    }

    /// Set the retry configuration, consuming and returning `self` for builder chaining.
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    /// Set the retry configuration by mutable reference.
    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.retry_config = retry_config;
    }

    /// Return a reference to the current retry configuration.
    pub fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }

    /// Build the chat completions URL for this endpoint.
    fn api_url(&self) -> String {
        format!(
            "{}/chat/completions?api-version=2024-05-01-preview",
            self.endpoint.trim_end_matches('/')
        )
    }
}

#[async_trait]
impl Llm for AzureAIClient {
    fn name(&self) -> &str {
        &self.model
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        let api_url = self.api_url();
        let api_key = self.api_key.clone();
        let model = self.model.clone();
        let endpoint = self.endpoint.clone();
        let client = self.client.clone();
        let retry_config = self.retry_config.clone();

        let body = convert::build_request_body(
            &model,
            &request.contents,
            &request.tools,
            request.config.as_ref(),
            stream,
        );

        let response_stream = try_stream! {
            let response = execute_with_retry(&retry_config, is_retryable_model_error, || {
                let client = client.clone();
                let api_url = api_url.clone();
                let api_key = api_key.clone();
                let body = body.clone();
                let endpoint = endpoint.clone();
                async move {
                    let resp = client
                        .post(&api_url)
                        .header("api-key", &api_key)
                        .header("Content-Type", "application/json")
                        .json(&body)
                        .send()
                        .await
                        .map_err(|e| AdkError::Model(format!(
                            "Azure AI error for endpoint={endpoint}: {e}"
                        )))?;

                    if !resp.status().is_success() {
                        let status = resp.status();
                        let error_text = resp.text().await.unwrap_or_default();
                        return Err(AdkError::Model(format!(
                            "Azure AI error for endpoint={endpoint}, status={status}: {error_text}"
                        )));
                    }

                    Ok(resp)
                }
            })
            .await?;

            if stream {
                let mut byte_stream = response.bytes_stream();
                let mut buffer = String::new();

                // Accumulate tool calls across SSE chunks
                let mut tool_call_accumulators: std::collections::HashMap<u32, (String, String, String)> =
                    std::collections::HashMap::new();

                while let Some(chunk_result) = byte_stream.next().await {
                    let chunk = chunk_result
                        .map_err(|e| AdkError::Model(format!("Azure AI stream error: {e}")))?;

                    buffer.push_str(&String::from_utf8_lossy(&chunk));

                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim().to_string();
                        buffer = buffer[line_end + 1..].to_string();

                        if line.is_empty() || line == "data: [DONE]" {
                            continue;
                        }

                        if let Some(data) = line.strip_prefix("data: ") {
                            match serde_json::from_str::<Value>(data) {
                                Ok(chunk_json) => {
                                    // Accumulate tool call deltas
                                    accumulate_tool_calls(&chunk_json, &mut tool_call_accumulators);

                                    let llm_resp = convert::parse_sse_chunk(&chunk_json);

                                    if llm_resp.turn_complete {
                                        // Emit accumulated tool calls if any
                                        if !tool_call_accumulators.is_empty() {
                                            let mut sorted: Vec<_> =
                                                tool_call_accumulators.drain().collect();
                                            sorted.sort_by_key(|(idx, _)| *idx);

                                            let parts: Vec<Part> = sorted
                                                .into_iter()
                                                .map(|(_, (id, name, args_str))| {
                                                    let args: Value =
                                                        serde_json::from_str(&args_str)
                                                            .unwrap_or(serde_json::json!({}));
                                                    Part::FunctionCall {
                                                        name,
                                                        args,
                                                        id: Some(id),
                                                    }
                                                })
                                                .collect();

                                            yield LlmResponse {
                                                content: Some(adk_core::Content {
                                                    role: "model".to_string(),
                                                    parts,
                                                }),
                                                finish_reason: llm_resp.finish_reason,
                                                partial: false,
                                                turn_complete: true,
                                                ..Default::default()
                                            };
                                            continue;
                                        }

                                        yield llm_resp;
                                    } else if llm_resp.content.is_some() {
                                        yield llm_resp;
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("failed to parse Azure AI chunk: {e} - {data}");
                                }
                            }
                        }
                    }
                }
            } else {
                let response_text = response.text().await
                    .map_err(|e| AdkError::Model(format!(
                        "Azure AI response parse failed: {e}"
                    )))?;

                let response_json: Value = serde_json::from_str(&response_text)
                    .map_err(|e| AdkError::Model(format!(
                        "Azure AI response parse failed: {e}"
                    )))?;

                yield convert::parse_response(&response_json);
            }
        };

        Ok(Box::pin(response_stream))
    }
}

/// Accumulate tool call argument deltas from an SSE chunk into the accumulator map.
///
/// Each tool call is identified by its index in the `tool_calls` array. The
/// accumulator stores `(id, name, arguments_string)` per index, appending
/// argument fragments as they arrive across multiple chunks.
fn accumulate_tool_calls(
    chunk: &Value,
    accumulators: &mut std::collections::HashMap<u32, (String, String, String)>,
) {
    let Some(tool_calls) = chunk
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|choice| choice.get("delta"))
        .and_then(|delta| delta.get("tool_calls"))
        .and_then(|tc| tc.as_array())
    else {
        return;
    };

    for tc in tool_calls {
        let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
        let entry = accumulators.entry(index).or_insert_with(|| {
            let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            (id, String::new(), String::new())
        });

        if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
            if !id.is_empty() {
                entry.0 = id.to_string();
            }
        }

        if let Some(func) = tc.get("function") {
            if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                if !name.is_empty() {
                    entry.1 = name.to_string();
                }
            }
            if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                entry.2.push_str(args);
            }
        }
    }
}
