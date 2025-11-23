use adk_core::{Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result, UsageMetadata};
use async_trait::async_trait;
use gemini::Gemini;

pub struct GeminiModel {
    client: Gemini,
    model_name: String,
}

impl GeminiModel {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        let client = Gemini::new(api_key.into())
            .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

        Ok(Self {
            client,
            model_name: model.into(),
        })
    }

    fn convert_response(resp: &gemini::GenerationResponse) -> Result<LlmResponse> {
        let content = resp.candidates.first()
            .and_then(|c| c.content.parts.as_ref())
            .map(|parts| {
                let converted_parts: Vec<Part> = parts.iter().filter_map(|p| {
                    match p {
                        gemini::Part::Text { text, .. } => Some(Part::Text { text: text.clone() }),
                        gemini::Part::FunctionCall { function_call, .. } => Some(Part::FunctionCall {
                            name: function_call.name.clone(),
                            args: function_call.args.clone(),
                        }),
                        gemini::Part::FunctionResponse { function_response } => Some(Part::FunctionResponse {
                            name: function_response.name.clone(),
                            response: function_response.response.clone().unwrap_or(serde_json::Value::Null),
                        }),
                        _ => None,
                    }
                }).collect();

                Content {
                    role: "model".to_string(),
                    parts: converted_parts,
                }
            });

        let usage_metadata = resp.usage_metadata.as_ref().map(|u| UsageMetadata {
            prompt_token_count: u.prompt_token_count.unwrap_or(0),
            candidates_token_count: u.candidates_token_count.unwrap_or(0),
            total_token_count: u.total_token_count.unwrap_or(0),
        });

        let finish_reason = resp.candidates.first()
            .and_then(|c| c.finish_reason.as_ref())
            .map(|fr| match fr {
                gemini::FinishReason::Stop => FinishReason::Stop,
                gemini::FinishReason::MaxTokens => FinishReason::MaxTokens,
                gemini::FinishReason::Safety => FinishReason::Safety,
                gemini::FinishReason::Recitation => FinishReason::Recitation,
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
}

#[async_trait]
impl Llm for GeminiModel {
    fn name(&self) -> &str {
        &self.model_name
    }

    async fn generate_content(&self, req: LlmRequest, stream: bool) -> Result<LlmResponseStream> {
        let mut builder = self.client.generate_content();

        // Add contents using proper builder methods
        for content in &req.contents {
            match content.role.as_str() {
                "user" => {
                    // For user messages, extract text parts
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            builder = builder.with_user_message(text);
                        }
                    }
                }
                "model" => {
                    // For model messages, build gemini Content
                    let mut gemini_parts = Vec::new();
                    for part in &content.parts {
                        match part {
                            Part::Text { text } => {
                                gemini_parts.push(gemini::Part::Text {
                                    text: text.clone(),
                                    thought: None,
                                    thought_signature: None,
                                });
                            }
                            Part::FunctionCall { name, args } => {
                                gemini_parts.push(gemini::Part::FunctionCall {
                                    function_call: gemini::FunctionCall {
                                        name: name.clone(),
                                        args: args.clone(),
                                        thought_signature: None,
                                    },
                                    thought_signature: None,
                                });
                            }
                            _ => {}
                        }
                    }
                    if !gemini_parts.is_empty() {
                        let model_content = gemini::Content {
                            role: Some(gemini::Role::Model),
                            parts: Some(gemini_parts),
                        };
                        builder = builder.with_message(gemini::Message {
                            content: model_content,
                            role: gemini::Role::Model,
                        });
                    }
                }
                "function" => {
                    // For function responses
                    for part in &content.parts {
                        if let Part::FunctionResponse { name, response } = part {
                            builder = builder.with_function_response(name, response.clone())
                                .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;
                        }
                    }
                }
                _ => {}
            }
        }

        // Add generation config
        if let Some(config) = req.config {
            let gen_config = gemini::GenerationConfig {
                temperature: config.temperature,
                top_p: config.top_p,
                top_k: config.top_k,
                max_output_tokens: config.max_output_tokens,
                ..Default::default()
            };
            builder = builder.with_generation_config(gen_config);
        }

        // Add tools
        if !req.tools.is_empty() {
            let mut function_declarations = Vec::new();
            
            for (_name, tool_decl) in &req.tools {
                // Deserialize our tool declaration into gemini::FunctionDeclaration
                if let Ok(func_decl) = serde_json::from_value::<gemini::FunctionDeclaration>(tool_decl.clone()) {
                    function_declarations.push(func_decl);
                }
            }
            
            if !function_declarations.is_empty() {
                let tool = gemini::Tool::with_functions(function_declarations);
                builder = builder.with_tool(tool);
            }
        }

        if stream {
            let response_stream = builder
                .execute_stream()
                .await
                .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

            let mapped_stream = async_stream::stream! {
                use futures::TryStreamExt;
                let mut stream = response_stream;
                while let Some(result) = stream.try_next().await.transpose() {
                    match result {
                        Ok(resp) => {
                            match Self::convert_response(&resp) {
                                Ok(mut llm_resp) => {
                                    llm_resp.partial = true;
                                    llm_resp.turn_complete = false;
                                    yield Ok(llm_resp);
                                }
                                Err(e) => yield Err(e),
                            }
                        }
                        Err(e) => yield Err(adk_core::AdkError::Model(e.to_string())),
                    }
                }
            };

            Ok(Box::pin(mapped_stream))
        } else {
            let response = builder
                .execute()
                .await
                .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

            let llm_response = Self::convert_response(&response)?;

            let stream = async_stream::stream! {
                yield Ok(llm_response);
            };

            Ok(Box::pin(stream))
        }
    }
}
