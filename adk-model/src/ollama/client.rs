//! Ollama client implementation.

use super::config::OllamaConfig;
use super::convert;
use adk_core::{AdkError, Llm, LlmRequest, LlmResponseStream, Result};
use async_stream::try_stream;
use async_trait::async_trait;
use ollama_rs::Ollama;
use ollama_rs::generation::chat::ChatMessage;
use ollama_rs::generation::chat::request::ChatMessageRequest;
use ollama_rs::generation::tools::{ToolFunctionInfo, ToolInfo, ToolType};
use ollama_rs::models::ModelOptions;
use schemars::Schema;

/// Ollama client for local LLM inference.
pub struct OllamaModel {
    client: Ollama,
    model_name: String,
    config: OllamaConfig,
}

impl OllamaModel {
    /// Create a new Ollama client with the given configuration.
    pub fn new(config: OllamaConfig) -> Result<Self> {
        // Parse host URL to extract host and port
        let host = config.host.trim_end_matches('/');
        let client = Ollama::try_new(host)
            .map_err(|e| AdkError::Model(format!("Failed to create Ollama client: {}", e)))?;

        Ok(Self { client, model_name: config.model.clone(), config })
    }

    /// Create a client with default settings for a given model.
    pub fn from_model(model: impl Into<String>) -> Result<Self> {
        Self::new(OllamaConfig::new(model))
    }

    /// Build ModelOptions from config and request.
    fn build_options(&self, request: &LlmRequest) -> ModelOptions {
        let mut options = ModelOptions::default();

        // Apply config options
        if let Some(temp) = self.config.temperature {
            options = options.temperature(temp);
        }
        if let Some(top_p) = self.config.top_p {
            options = options.top_p(top_p);
        }
        if let Some(top_k) = self.config.top_k {
            options = options.top_k(top_k as u32);
        }
        if let Some(num_ctx) = self.config.num_ctx {
            options = options.num_ctx(num_ctx as u64);
        }

        // Apply request config overrides
        if let Some(ref cfg) = request.config {
            if let Some(temp) = cfg.temperature {
                options = options.temperature(temp);
            }
            if let Some(top_p) = cfg.top_p {
                options = options.top_p(top_p);
            }
            if let Some(top_k) = cfg.top_k {
                options = options.top_k(top_k as u32);
            }
        }

        options
    }

    /// Convert ADK tool declarations to Ollama tools.
    fn convert_tools(
        &self,
        tools: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Vec<ToolInfo> {
        tools
            .iter()
            .map(|(name, decl)| {
                let description =
                    decl.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let parameters_json =
                    decl.get("parameters").cloned().unwrap_or(serde_json::json!({}));
                let parameters: Schema =
                    serde_json::from_value(parameters_json).unwrap_or_else(|_| Schema::from(false));

                ToolInfo {
                    tool_type: ToolType::Function,
                    function: ToolFunctionInfo { name: name.clone(), description, parameters },
                }
            })
            .collect()
    }
}

#[async_trait]
impl Llm for OllamaModel {
    fn name(&self) -> &str {
        &self.model_name
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream> {
        let model = self.model_name.clone();
        let client = self.client.clone();
        let options = self.build_options(&request);

        // Convert ADK contents to Ollama messages
        let mut messages: Vec<ChatMessage> = Vec::new();
        for content in &request.contents {
            if let Some(msg) = convert::content_to_chat_message(content) {
                messages.push(msg);
            }
        }

        // Build chat request
        let mut chat_request = ChatMessageRequest::new(model, messages).options(options);

        // Add tools if present
        if !request.tools.is_empty() {
            let tools = self.convert_tools(&request.tools);
            chat_request = chat_request.tools(tools);
        }

        let response_stream = try_stream! {
            // When tools are present, use non-streaming mode because ollama-rs
            // doesn't parse tool_calls in streaming responses
            let use_streaming = stream && request.tools.is_empty();

            if use_streaming {
                // Streaming mode (only when no tools)
                use futures::StreamExt;

                let stream_result = client
                    .send_chat_messages_stream(chat_request)
                    .await
                    .map_err(|e| AdkError::Model(format!("Ollama stream error: {}", e)))?;

                let mut pinned_stream = std::pin::pin!(stream_result);

                while let Some(chunk_result) = pinned_stream.next().await {
                    match chunk_result {
                        Ok(response) => {
                            // Yield thinking delta for each chunk
                            if let Some(thinking) = &response.message.thinking {
                                if !thinking.is_empty() {
                                    yield convert::thinking_delta_response(thinking);
                                }
                            }

                            // Yield text delta for each chunk
                            if !response.message.content.is_empty() {
                                yield convert::text_delta_response(&response.message.content);
                            }

                            // If done, yield final response with metadata
                            if response.done {
                                yield convert::chat_response_to_llm_response(&response, false);
                            }
                        }
                        Err(e) => {
                            Err(AdkError::Model(format!("Stream chunk error: {:?}", e)))?;
                        }
                    }
                }
            } else {
                // Non-streaming mode (required when tools are present)
                let response = client
                    .send_chat_messages(chat_request)
                    .await
                    .map_err(|e| AdkError::Model(format!("Ollama error: {}", e)))?;

                yield convert::chat_response_to_llm_response(&response, false);
            }
        };

        Ok(Box::pin(response_stream))
    }
}
