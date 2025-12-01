use adk_core::{
    Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result,
    UsageMetadata,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct OpenaiModel {
    client: Client,
    base_url: String,
    api_key: String,
    model_name: String,
}

impl OpenaiModel {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let client = Client::new();
        let mut base_url = base_url.into();
        if base_url.ends_with('/') {
            base_url.pop();
        }

        Ok(Self { client, base_url, api_key: api_key.into(), model_name: model.into() })
    }

    fn convert_to_openai_messages(contents: &[Content]) -> Vec<OpenaiMessage> {
        let mut messages = Vec::new();

        for content in contents {
            let role = match content.role.as_str() {
                "model" => "assistant",
                "function" => "tool",
                other => other,
            };

            let mut text_parts = Vec::new();
            let mut tool_calls = Vec::new();
            let mut tool_call_id = None;

            for part in &content.parts {
                match part {
                    Part::Text { text } => {
                        text_parts.push(text.clone());
                    }
                    Part::FunctionCall { name, args } => {
                        tool_calls.push(OpenaiToolCall {
                            id: format!("call_{}", uuid::Uuid::new_v4()),
                            r#type: "function".to_string(),
                            function: OpenaiFunction {
                                name: name.clone(),
                                arguments: serde_json::to_string(args).unwrap_or_default(),
                            },
                        });
                    }
                    Part::FunctionResponse { name: _, response } => {
                        tool_call_id = Some(format!("call_{}", uuid::Uuid::new_v4()));
                        text_parts.push(serde_json::to_string(response).unwrap_or_default());
                    }
                    _ => {}
                }
            }

            let content_str =
                if text_parts.is_empty() { None } else { Some(text_parts.join("\n")) };

            let msg = OpenaiMessage {
                role: role.to_string(),
                content: content_str,
                tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                tool_call_id,
            };

            messages.push(msg);
        }

        messages
    }

    fn convert_tools(
        tools: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Vec<OpenaiTool> {
        tools
            .iter()
            .map(|(name, decl)| {
                let description = decl.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let parameters = decl.get("parameters").cloned().unwrap_or(serde_json::json!({}));

                OpenaiTool {
                    r#type: "function".to_string(),
                    function: OpenaiToolFunction {
                        name: name.clone(),
                        description: description.to_string(),
                        parameters,
                    },
                }
            })
            .collect()
    }

    fn convert_response(resp: &OpenaiChatResponse) -> Result<LlmResponse> {
        let choice = resp.choices.first();

        let content = choice.map(|c| {
            let mut parts = Vec::new();

            if let Some(text) = &c.message.content {
                parts.push(Part::Text { text: text.clone() });
            }

            if let Some(tool_calls) = &c.message.tool_calls {
                for tc in tool_calls {
                    let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::Value::Null);
                    parts.push(Part::FunctionCall { name: tc.function.name.clone(), args });
                }
            }

            Content { role: "model".to_string(), parts }
        });

        let usage_metadata = resp.usage.as_ref().map(|u| UsageMetadata {
            prompt_token_count: u.prompt_tokens as i32,
            candidates_token_count: u.completion_tokens as i32,
            total_token_count: u.total_tokens as i32,
        });

        let finish_reason =
            choice.and_then(|c| c.finish_reason.as_ref()).map(|fr| match fr.as_str() {
                "stop" => FinishReason::Stop,
                "length" => FinishReason::MaxTokens,
                "content_filter" => FinishReason::Safety,
                "tool_calls" => FinishReason::Stop,
                _ => FinishReason::Other,
            });

        Ok(LlmResponse {
            content,
            usage_metadata,
            finish_reason,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
        })
    }

    fn convert_stream_chunk(chunk: &OpenaiStreamChunk) -> Result<Option<LlmResponse>> {
        let choice = match chunk.choices.first() {
            Some(c) => c,
            None => return Ok(None),
        };

        let delta = &choice.delta;
        let mut parts = Vec::new();

        if let Some(text) = &delta.content {
            if !text.is_empty() {
                parts.push(Part::Text { text: text.clone() });
            }
        }

        if let Some(tool_calls) = &delta.tool_calls {
            for tc in tool_calls {
                if let Some(func) = &tc.function {
                    let name = func.name.clone().unwrap_or_default();
                    let args_str = func.arguments.clone().unwrap_or_default();
                    let args: serde_json::Value =
                        serde_json::from_str(&args_str).unwrap_or(serde_json::Value::Null);
                    if !name.is_empty() || args != serde_json::Value::Null {
                        parts.push(Part::FunctionCall { name, args });
                    }
                }
            }
        }

        if parts.is_empty() {
            return Ok(None);
        }

        let content = Some(Content { role: "model".to_string(), parts });

        let finish_reason = choice.finish_reason.as_ref().map(|fr| match fr.as_str() {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::MaxTokens,
            "content_filter" => FinishReason::Safety,
            "tool_calls" => FinishReason::Stop,
            _ => FinishReason::Other,
        });

        Ok(Some(LlmResponse {
            content,
            usage_metadata: None,
            finish_reason,
            partial: true,
            turn_complete: choice.finish_reason.is_some(),
            interrupted: false,
            error_code: None,
            error_message: None,
        }))
    }
}

#[async_trait]
impl Llm for OpenaiModel {
    fn name(&self) -> &str {
        &self.model_name
    }

    #[adk_telemetry::instrument(
        skip(self, req),
        fields(
            model.name = %self.model_name,
            stream = %stream,
            request.contents_count = %req.contents.len(),
            request.tools_count = %req.tools.len()
        )
    )]
    async fn generate_content(&self, req: LlmRequest, stream: bool) -> Result<LlmResponseStream> {
        adk_telemetry::info!("Generating content via OpenAI-compatible API");

        let messages = Self::convert_to_openai_messages(&req.contents);
        let tools = if req.tools.is_empty() { None } else { Some(Self::convert_tools(&req.tools)) };

        let mut request_body = OpenaiChatRequest {
            model: self.model_name.clone(),
            messages,
            tools,
            stream: Some(stream),
            temperature: None,
            top_p: None,
            max_tokens: None,
        };

        if let Some(config) = req.config {
            request_body.temperature = config.temperature;
            request_body.top_p = config.top_p;
            request_body.max_tokens = config.max_output_tokens.map(|v| v as u32);
        }

        let url = format!("{}/chat/completions", self.base_url);

        if stream {
            adk_telemetry::debug!("Executing streaming request");

            let response = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&request_body)
                .send()
                .await
                .map_err(|e| {
                    adk_telemetry::error!(error = %e, "Request failed");
                    adk_core::AdkError::Model(e.to_string())
                })?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                adk_telemetry::error!(status = %status, body = %body, "API error");
                return Err(adk_core::AdkError::Model(format!("API error {}: {}", status, body)));
            }

            let byte_stream = response.bytes_stream();

            let mapped_stream = async_stream::stream! {
                use futures::StreamExt;

                let mut stream = byte_stream;
                let mut buffer = String::new();

                while let Some(chunk_result) = stream.next().await {
                    match chunk_result {
                        Ok(bytes) => {
                            buffer.push_str(&String::from_utf8_lossy(&bytes));

                            while let Some(line_end) = buffer.find('\n') {
                                let line = buffer[..line_end].trim().to_string();
                                buffer = buffer[line_end + 1..].to_string();

                                if line.is_empty() || line == "data: [DONE]" {
                                    continue;
                                }

                                if let Some(data) = line.strip_prefix("data: ") {
                                    match serde_json::from_str::<OpenaiStreamChunk>(data) {
                                        Ok(chunk) => {
                                            match Self::convert_stream_chunk(&chunk) {
                                                Ok(Some(resp)) => yield Ok(resp),
                                                Ok(None) => {}
                                                Err(e) => {
                                                    adk_telemetry::error!(error = %e, "Failed to convert chunk");
                                                    yield Err(e);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            adk_telemetry::warn!(error = %e, data = %data, "Failed to parse chunk");
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            adk_telemetry::error!(error = %e, "Stream error");
                            yield Err(adk_core::AdkError::Model(e.to_string()));
                        }
                    }
                }
            };

            Ok(Box::pin(mapped_stream))
        } else {
            adk_telemetry::debug!("Executing blocking request");

            let response = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&request_body)
                .send()
                .await
                .map_err(|e| {
                    adk_telemetry::error!(error = %e, "Request failed");
                    adk_core::AdkError::Model(e.to_string())
                })?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                adk_telemetry::error!(status = %status, body = %body, "API error");
                return Err(adk_core::AdkError::Model(format!("API error {}: {}", status, body)));
            }

            let chat_response: OpenaiChatResponse = response.json().await.map_err(|e| {
                adk_telemetry::error!(error = %e, "Failed to parse response");
                adk_core::AdkError::Model(e.to_string())
            })?;

            let llm_response = Self::convert_response(&chat_response)?;

            let stream = async_stream::stream! {
                yield Ok(llm_response);
            };

            Ok(Box::pin(stream))
        }
    }
}

#[derive(Debug, Serialize)]
struct OpenaiChatRequest {
    model: String,
    messages: Vec<OpenaiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenaiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct OpenaiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenaiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct OpenaiToolCall {
    id: String,
    r#type: String,
    function: OpenaiFunction,
}

#[derive(Debug, Serialize)]
struct OpenaiFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct OpenaiTool {
    r#type: String,
    function: OpenaiToolFunction,
}

#[derive(Debug, Serialize)]
struct OpenaiToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OpenaiChatResponse {
    choices: Vec<OpenaiChoice>,
    usage: Option<OpenaiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenaiChoice {
    message: OpenaiResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenaiResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OpenaiResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenaiResponseToolCall {
    id: String,
    r#type: String,
    function: OpenaiResponseFunction,
}

#[derive(Debug, Deserialize)]
struct OpenaiResponseFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenaiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenaiStreamChunk {
    choices: Vec<OpenaiStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenaiStreamChoice {
    delta: OpenaiDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenaiDelta {
    content: Option<String>,
    tool_calls: Option<Vec<OpenaiDeltaToolCall>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenaiDeltaToolCall {
    index: Option<u32>,
    id: Option<String>,
    function: Option<OpenaiDeltaFunction>,
}

#[derive(Debug, Deserialize)]
struct OpenaiDeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}
