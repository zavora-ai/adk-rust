use adk_core::{
    Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result,
    UsageMetadata,
};
use adk_gemini::Gemini;
use async_trait::async_trait;

pub struct GeminiModel {
    client: Gemini,
    model_name: String,
}

impl GeminiModel {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        let client =
            Gemini::new(api_key.into()).map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

        Ok(Self { client, model_name: model.into() })
    }

    pub fn new_google_cloud(
        api_key: impl Into<String>,
        project_id: impl AsRef<str>,
        location: impl AsRef<str>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let model_name = model.into();
        let client = Gemini::with_google_cloud_model(
            api_key.into(),
            project_id,
            location,
            model_name.clone(),
        )
        .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

        Ok(Self { client, model_name })
    }

    pub fn new_google_cloud_service_account(
        service_account_json: &str,
        project_id: impl AsRef<str>,
        location: impl AsRef<str>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let model_name = model.into();
        let client = Gemini::with_google_cloud_service_account_json(
            service_account_json,
            project_id.as_ref(),
            location.as_ref(),
            model_name.clone(),
        )
        .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

        Ok(Self { client, model_name })
    }

    pub fn new_google_cloud_adc(
        project_id: impl AsRef<str>,
        location: impl AsRef<str>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let model_name = model.into();
        let client = Gemini::with_google_cloud_adc_model(
            project_id.as_ref(),
            location.as_ref(),
            model_name.clone(),
        )
        .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

        Ok(Self { client, model_name })
    }

    pub fn new_google_cloud_wif(
        wif_json: &str,
        project_id: impl AsRef<str>,
        location: impl AsRef<str>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let model_name = model.into();
        let client = Gemini::with_google_cloud_wif_json(
            wif_json,
            project_id.as_ref(),
            location.as_ref(),
            model_name.clone(),
        )
        .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

        Ok(Self { client, model_name })
    }

    fn convert_response(resp: &adk_gemini::GenerationResponse) -> Result<LlmResponse> {
        let mut converted_parts: Vec<Part> = Vec::new();

        // Convert content parts
        if let Some(parts) = resp.candidates.first().and_then(|c| c.content.parts.as_ref()) {
            for p in parts {
                match p {
                    adk_gemini::Part::Text { text, .. } => {
                        converted_parts.push(Part::Text { text: text.clone() });
                    }
                    adk_gemini::Part::FunctionCall { function_call, .. } => {
                        converted_parts.push(Part::FunctionCall {
                            name: function_call.name.clone(),
                            args: function_call.args.clone(),
                            id: None,
                        });
                    }
                    adk_gemini::Part::FunctionResponse { function_response } => {
                        converted_parts.push(Part::FunctionResponse {
                            function_response: adk_core::FunctionResponseData {
                                name: function_response.name.clone(),
                                response: function_response
                                    .response
                                    .clone()
                                    .unwrap_or(serde_json::Value::Null),
                            },
                            id: None,
                        });
                    }
                    _ => {}
                }
            }
        }

        // Add grounding metadata as text if present (required for Google Search grounding compliance)
        if let Some(grounding) = resp.candidates.first().and_then(|c| c.grounding_metadata.as_ref())
        {
            if let Some(queries) = &grounding.web_search_queries {
                if !queries.is_empty() {
                    let search_info = format!("\n\n🔍 **Searched:** {}", queries.join(", "));
                    converted_parts.push(Part::Text { text: search_info });
                }
            }
            if let Some(chunks) = &grounding.grounding_chunks {
                let sources: Vec<String> = chunks
                    .iter()
                    .filter_map(|c| {
                        c.web.as_ref().and_then(|w| match (&w.title, &w.uri) {
                            (Some(title), Some(uri)) => Some(format!("[{}]({})", title, uri)),
                            (Some(title), None) => Some(title.clone()),
                            (None, Some(uri)) => Some(uri.to_string()),
                            (None, None) => None,
                        })
                    })
                    .collect();
                if !sources.is_empty() {
                    let sources_info = format!("\n📚 **Sources:** {}", sources.join(" | "));
                    converted_parts.push(Part::Text { text: sources_info });
                }
            }
        }

        let content = if converted_parts.is_empty() {
            None
        } else {
            Some(Content { role: "model".to_string(), parts: converted_parts })
        };

        let usage_metadata = resp.usage_metadata.as_ref().map(|u| UsageMetadata {
            prompt_token_count: u.prompt_token_count.unwrap_or(0),
            candidates_token_count: u.candidates_token_count.unwrap_or(0),
            total_token_count: u.total_token_count.unwrap_or(0),
        });

        let finish_reason =
            resp.candidates.first().and_then(|c| c.finish_reason.as_ref()).map(|fr| match fr {
                adk_gemini::FinishReason::Stop => FinishReason::Stop,
                adk_gemini::FinishReason::MaxTokens => FinishReason::MaxTokens,
                adk_gemini::FinishReason::Safety => FinishReason::Safety,
                adk_gemini::FinishReason::Recitation => FinishReason::Recitation,
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

    #[adk_telemetry::instrument(
        name = "call_llm",
        skip(self, req),
        fields(
            model.name = %self.model_name,
            stream = %stream,
            request.contents_count = %req.contents.len(),
            request.tools_count = %req.tools.len()
        )
    )]
    async fn generate_content(&self, req: LlmRequest, stream: bool) -> Result<LlmResponseStream> {
        adk_telemetry::info!("Generating content");

        let mut builder = self.client.generate_content();

        // Add contents using proper builder methods
        for content in &req.contents {
            match content.role.as_str() {
                "user" => {
                    // For user messages, build gemini Content with potentially multiple parts
                    let mut gemini_parts = Vec::new();
                    for part in &content.parts {
                        match part {
                            Part::Text { text } => {
                                gemini_parts.push(adk_gemini::Part::Text {
                                    text: text.clone(),
                                    thought: None,
                                    thought_signature: None,
                                });
                            }
                            Part::InlineData { data, mime_type } => {
                                use base64::{Engine as _, engine::general_purpose::STANDARD};
                                let encoded = STANDARD.encode(data);
                                gemini_parts.push(adk_gemini::Part::InlineData {
                                    inline_data: adk_gemini::Blob {
                                        mime_type: mime_type.clone(),
                                        data: encoded,
                                    },
                                });
                            }
                            _ => {}
                        }
                    }
                    if !gemini_parts.is_empty() {
                        let user_content = adk_gemini::Content {
                            role: Some(adk_gemini::Role::User),
                            parts: Some(gemini_parts),
                        };
                        builder = builder.with_message(adk_gemini::Message {
                            content: user_content,
                            role: adk_gemini::Role::User,
                        });
                    }
                }
                "model" => {
                    // For model messages, build gemini Content
                    let mut gemini_parts = Vec::new();
                    for part in &content.parts {
                        match part {
                            Part::Text { text } => {
                                gemini_parts.push(adk_gemini::Part::Text {
                                    text: text.clone(),
                                    thought: None,
                                    thought_signature: None,
                                });
                            }
                            Part::FunctionCall { name, args, .. } => {
                                gemini_parts.push(adk_gemini::Part::FunctionCall {
                                    function_call: adk_gemini::FunctionCall {
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
                        let model_content = adk_gemini::Content {
                            role: Some(adk_gemini::Role::Model),
                            parts: Some(gemini_parts),
                        };
                        builder = builder.with_message(adk_gemini::Message {
                            content: model_content,
                            role: adk_gemini::Role::Model,
                        });
                    }
                }
                "function" => {
                    // For function responses
                    for part in &content.parts {
                        if let Part::FunctionResponse { function_response, .. } = part {
                            builder = builder
                                .with_function_response(
                                    &function_response.name,
                                    function_response.response.clone(),
                                )
                                .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;
                        }
                    }
                }
                _ => {}
            }
        }

        // Add generation config
        if let Some(config) = req.config {
            let has_schema = config.response_schema.is_some();
            let gen_config = adk_gemini::GenerationConfig {
                temperature: config.temperature,
                top_p: config.top_p,
                top_k: config.top_k,
                max_output_tokens: config.max_output_tokens,
                response_schema: config.response_schema,
                response_mime_type: if has_schema {
                    Some("application/json".to_string())
                } else {
                    None
                },
                ..Default::default()
            };
            builder = builder.with_generation_config(gen_config);
        }

        // Add tools
        if !req.tools.is_empty() {
            let mut function_declarations = Vec::new();
            let mut has_google_search = false;

            for (name, tool_decl) in &req.tools {
                if name == "google_search" {
                    has_google_search = true;
                    continue;
                }

                // Deserialize our tool declaration into adk_gemini::FunctionDeclaration
                if let Ok(func_decl) =
                    serde_json::from_value::<adk_gemini::FunctionDeclaration>(tool_decl.clone())
                {
                    function_declarations.push(func_decl);
                }
            }

            if !function_declarations.is_empty() {
                let tool = adk_gemini::Tool::with_functions(function_declarations);
                builder = builder.with_tool(tool);
            }

            if has_google_search {
                // Enable built-in Google Search
                let tool = adk_gemini::Tool::google_search();
                builder = builder.with_tool(tool);
            }
        }

        if stream {
            adk_telemetry::debug!("Executing streaming request");
            let response_stream = builder.execute_stream().await.map_err(|e| {
                adk_telemetry::error!(error = %e, "Model request failed");
                adk_core::AdkError::Model(e.to_string())
            })?;

            let mapped_stream = async_stream::stream! {
                use futures::TryStreamExt;
                let mut stream = response_stream;
                while let Some(result) = stream.try_next().await.transpose() {
                    match result {
                        Ok(resp) => {
                            match Self::convert_response(&resp) {
                                Ok(mut llm_resp) => {
                                    // Check if this is the final chunk (has finish_reason)
                                    let is_final = llm_resp.finish_reason.is_some();
                                    llm_resp.partial = !is_final;
                                    llm_resp.turn_complete = is_final;
                                    yield Ok(llm_resp);
                                }
                                Err(e) => {
                                    adk_telemetry::error!(error = %e, "Failed to convert response");
                                    yield Err(e);
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
            let response = builder.execute().await.map_err(|e| {
                adk_telemetry::error!(error = %e, "Model request failed");
                adk_core::AdkError::Model(e.to_string())
            })?;

            let llm_response = Self::convert_response(&response)?;

            let stream = async_stream::stream! {
                yield Ok(llm_response);
            };

            Ok(Box::pin(stream))
        }
    }
}
