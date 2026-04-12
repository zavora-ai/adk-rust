use crate::attachment;
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{
    CacheCapable, CitationMetadata, CitationSource, Content, ErrorCategory, ErrorComponent,
    FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result, UsageMetadata,
};
use adk_gemini::Gemini;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures::TryStreamExt;

pub struct GeminiModel {
    client: Gemini,
    model_name: String,
    retry_config: RetryConfig,
}

/// Convert a Gemini client error to a structured `AdkError` with proper category and retry hints.
fn gemini_error_to_adk(e: &adk_gemini::ClientError) -> adk_core::AdkError {
    fn format_error_chain(e: &dyn std::error::Error) -> String {
        let mut msg = e.to_string();
        let mut source = e.source();
        while let Some(s) = source {
            msg.push_str(": ");
            msg.push_str(&s.to_string());
            source = s.source();
        }
        msg
    }

    let message = format_error_chain(e);

    // Extract status code from BadResponse variant via Display output
    // BadResponse format: "bad response from server; code {code}; description: ..."
    let (category, code, status_code) = if message.contains("code 429")
        || message.contains("RESOURCE_EXHAUSTED")
        || message.contains("rate limit")
    {
        (ErrorCategory::RateLimited, "model.gemini.rate_limited", Some(429u16))
    } else if message.contains("code 503") || message.contains("UNAVAILABLE") {
        (ErrorCategory::Unavailable, "model.gemini.unavailable", Some(503))
    } else if message.contains("code 529") || message.contains("OVERLOADED") {
        (ErrorCategory::Unavailable, "model.gemini.overloaded", Some(529))
    } else if message.contains("code 408")
        || message.contains("DEADLINE_EXCEEDED")
        || message.contains("TIMEOUT")
    {
        (ErrorCategory::Timeout, "model.gemini.timeout", Some(408))
    } else if message.contains("code 401") || message.contains("Invalid API key") {
        (ErrorCategory::Unauthorized, "model.gemini.unauthorized", Some(401))
    } else if message.contains("code 400") {
        (ErrorCategory::InvalidInput, "model.gemini.bad_request", Some(400))
    } else if message.contains("code 404") {
        (ErrorCategory::NotFound, "model.gemini.not_found", Some(404))
    } else if message.contains("invalid generation config") {
        (ErrorCategory::InvalidInput, "model.gemini.invalid_config", None)
    } else {
        (ErrorCategory::Internal, "model.gemini.internal", None)
    };

    let mut err = adk_core::AdkError::new(ErrorComponent::Model, category, code, message)
        .with_provider("gemini");
    if let Some(sc) = status_code {
        err = err.with_upstream_status(sc);
    }
    err
}

impl GeminiModel {
    fn gemini_part_thought_signature(value: &serde_json::Value) -> Option<String> {
        value.get("thoughtSignature").and_then(serde_json::Value::as_str).map(str::to_string)
    }

    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        let model_name = model.into();
        let client = Gemini::with_model(api_key.into(), model_name.clone())
            .map_err(|e| adk_core::AdkError::model(e.to_string()))?;

        Ok(Self { client, model_name, retry_config: RetryConfig::default() })
    }

    /// Create a Gemini model via Vertex AI with API key auth.
    ///
    /// Requires `gemini-vertex` feature.
    #[cfg(feature = "gemini-vertex")]
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
        .map_err(|e| adk_core::AdkError::model(e.to_string()))?;

        Ok(Self { client, model_name, retry_config: RetryConfig::default() })
    }

    /// Create a Gemini model via Vertex AI with service account JSON.
    ///
    /// Requires `gemini-vertex` feature.
    #[cfg(feature = "gemini-vertex")]
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
        .map_err(|e| adk_core::AdkError::model(e.to_string()))?;

        Ok(Self { client, model_name, retry_config: RetryConfig::default() })
    }

    /// Create a Gemini model via Vertex AI with Application Default Credentials.
    ///
    /// Requires `gemini-vertex` feature.
    #[cfg(feature = "gemini-vertex")]
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
        .map_err(|e| adk_core::AdkError::model(e.to_string()))?;

        Ok(Self { client, model_name, retry_config: RetryConfig::default() })
    }

    /// Create a Gemini model via Vertex AI with Workload Identity Federation.
    ///
    /// Requires `gemini-vertex` feature.
    #[cfg(feature = "gemini-vertex")]
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
        .map_err(|e| adk_core::AdkError::model(e.to_string()))?;

        Ok(Self { client, model_name, retry_config: RetryConfig::default() })
    }

    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.retry_config = retry_config;
    }

    pub fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }

    fn convert_response(resp: &adk_gemini::GenerationResponse) -> Result<LlmResponse> {
        let mut converted_parts: Vec<Part> = Vec::new();

        // Convert content parts
        if let Some(parts) = resp.candidates.first().and_then(|c| c.content.parts.as_ref()) {
            for p in parts {
                match p {
                    adk_gemini::Part::Text { text, thought, thought_signature } => {
                        if thought == &Some(true) {
                            converted_parts.push(Part::Thinking {
                                thinking: text.clone(),
                                signature: thought_signature.clone(),
                            });
                        } else {
                            converted_parts.push(Part::Text { text: text.clone() });
                        }
                    }
                    adk_gemini::Part::InlineData { inline_data } => {
                        let decoded =
                            BASE64_STANDARD.decode(&inline_data.data).map_err(|error| {
                                adk_core::AdkError::model(format!(
                                    "failed to decode inline data from gemini response: {error}"
                                ))
                            })?;
                        converted_parts.push(Part::InlineData {
                            mime_type: inline_data.mime_type.clone(),
                            data: decoded,
                        });
                    }
                    adk_gemini::Part::FunctionCall { function_call, thought_signature } => {
                        converted_parts.push(Part::FunctionCall {
                            name: function_call.name.clone(),
                            args: function_call.args.clone(),
                            id: function_call.id.clone(),
                            thought_signature: thought_signature.clone(),
                        });
                    }
                    adk_gemini::Part::FunctionResponse { function_response, .. } => {
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
                    adk_gemini::Part::ToolCall { .. } | adk_gemini::Part::ExecutableCode { .. } => {
                        if let Ok(value) = serde_json::to_value(p) {
                            converted_parts.push(Part::ServerToolCall { server_tool_call: value });
                        }
                    }
                    adk_gemini::Part::ToolResponse { .. }
                    | adk_gemini::Part::CodeExecutionResult { .. } => {
                        let value = serde_json::to_value(p).unwrap_or(serde_json::Value::Null);
                        converted_parts
                            .push(Part::ServerToolResponse { server_tool_response: value });
                    }
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
            thinking_token_count: u.thoughts_token_count,
            cache_read_input_token_count: u.cached_content_token_count,
            ..Default::default()
        });

        let finish_reason =
            resp.candidates.first().and_then(|c| c.finish_reason.as_ref()).map(|fr| match fr {
                adk_gemini::FinishReason::Stop => FinishReason::Stop,
                adk_gemini::FinishReason::MaxTokens => FinishReason::MaxTokens,
                adk_gemini::FinishReason::Safety => FinishReason::Safety,
                adk_gemini::FinishReason::Recitation => FinishReason::Recitation,
                _ => FinishReason::Other,
            });

        let citation_metadata =
            resp.candidates.first().and_then(|c| c.citation_metadata.as_ref()).map(|meta| {
                CitationMetadata {
                    citation_sources: meta
                        .citation_sources
                        .iter()
                        .map(|source| CitationSource {
                            uri: source.uri.clone(),
                            title: source.title.clone(),
                            start_index: source.start_index,
                            end_index: source.end_index,
                            license: source.license.clone(),
                            publication_date: source.publication_date.map(|d| d.to_string()),
                        })
                        .collect(),
                }
            });

        // Serialize grounding metadata into provider_metadata so consumers
        // can access structured grounding data (search queries, sources, supports).
        let provider_metadata = resp
            .candidates
            .first()
            .and_then(|c| c.grounding_metadata.as_ref())
            .and_then(|g| serde_json::to_value(g).ok());

        Ok(LlmResponse {
            content,
            usage_metadata,
            finish_reason,
            citation_metadata,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
            provider_metadata,
        })
    }

    fn gemini_function_response_payload(response: serde_json::Value) -> serde_json::Value {
        match response {
            // Gemini functionResponse.response must be a JSON object.
            serde_json::Value::Object(_) => response,
            other => serde_json::json!({ "result": other }),
        }
    }

    fn merge_object_value(
        target: &mut serde_json::Map<String, serde_json::Value>,
        value: serde_json::Value,
    ) {
        if let serde_json::Value::Object(object) = value {
            for (key, value) in object {
                target.insert(key, value);
            }
        }
    }

    fn build_gemini_tools(
        tools: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<(Vec<adk_gemini::Tool>, adk_gemini::ToolConfig)> {
        let mut gemini_tools = Vec::new();
        let mut function_declarations = Vec::new();
        let mut has_provider_native_tools = false;
        let mut tool_config_json = serde_json::Map::new();

        for (name, tool_decl) in tools {
            if let Some(provider_tool) = tool_decl.get("x-adk-gemini-tool") {
                let tool = serde_json::from_value::<adk_gemini::Tool>(provider_tool.clone())
                    .map_err(|error| {
                        adk_core::AdkError::model(format!(
                            "failed to deserialize Gemini native tool '{name}': {error}"
                        ))
                    })?;
                has_provider_native_tools = true;
                gemini_tools.push(tool);
            } else if let Ok(func_decl) =
                serde_json::from_value::<adk_gemini::FunctionDeclaration>(tool_decl.clone())
            {
                function_declarations.push(func_decl);
            } else {
                return Err(adk_core::AdkError::model(format!(
                    "failed to deserialize Gemini tool '{name}' as a function declaration"
                )));
            }

            if let Some(tool_config) = tool_decl.get("x-adk-gemini-tool-config") {
                Self::merge_object_value(&mut tool_config_json, tool_config.clone());
            }
        }

        let has_function_declarations = !function_declarations.is_empty();
        if has_function_declarations {
            gemini_tools.push(adk_gemini::Tool::with_functions(function_declarations));
        }

        if has_provider_native_tools {
            tool_config_json.insert(
                "includeServerSideToolInvocations".to_string(),
                serde_json::Value::Bool(true),
            );
        }

        let tool_config = if tool_config_json.is_empty() {
            adk_gemini::ToolConfig::default()
        } else {
            serde_json::from_value::<adk_gemini::ToolConfig>(serde_json::Value::Object(
                tool_config_json,
            ))
            .map_err(|error| {
                adk_core::AdkError::model(format!(
                    "failed to deserialize Gemini tool configuration: {error}"
                ))
            })?
        };

        Ok((gemini_tools, tool_config))
    }

    fn stream_chunks_from_response(
        mut response: LlmResponse,
        saw_partial_chunk: bool,
    ) -> (Vec<LlmResponse>, bool) {
        let is_final = response.finish_reason.is_some();

        if !is_final {
            response.partial = true;
            response.turn_complete = false;
            return (vec![response], true);
        }

        response.partial = false;
        response.turn_complete = true;

        if saw_partial_chunk {
            return (vec![response], true);
        }

        let synthetic_partial = LlmResponse {
            content: None,
            usage_metadata: None,
            finish_reason: None,
            citation_metadata: None,
            partial: true,
            turn_complete: false,
            interrupted: false,
            error_code: None,
            error_message: None,
            provider_metadata: None,
        };

        (vec![synthetic_partial, response], true)
    }

    async fn generate_content_internal(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream> {
        let mut builder = self.client.generate_content();

        // Build a map of function_name → thought_signature from FunctionCall parts
        // in model content. Gemini 3.x requires thought_signature on FunctionResponse
        // parts when thinking is active, but adk_core::Part::FunctionResponse doesn't
        // carry it (it's Gemini-specific). We recover it here at the provider boundary.
        let mut fn_call_signatures: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for content in &req.contents {
            if content.role == "model" {
                for part in &content.parts {
                    if let Part::FunctionCall { name, thought_signature: Some(sig), .. } = part {
                        fn_call_signatures.insert(name.clone(), sig.clone());
                    }
                }
            }
        }

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
                            Part::Thinking { thinking, signature } => {
                                gemini_parts.push(adk_gemini::Part::Text {
                                    text: thinking.clone(),
                                    thought: Some(true),
                                    thought_signature: signature.clone(),
                                });
                            }
                            Part::InlineData { data, mime_type } => {
                                let encoded = attachment::encode_base64(data);
                                gemini_parts.push(adk_gemini::Part::InlineData {
                                    inline_data: adk_gemini::Blob {
                                        mime_type: mime_type.clone(),
                                        data: encoded,
                                    },
                                });
                            }
                            Part::FileData { mime_type, file_uri } => {
                                gemini_parts.push(adk_gemini::Part::Text {
                                    text: attachment::file_attachment_to_text(mime_type, file_uri),
                                    thought: None,
                                    thought_signature: None,
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
                            Part::Thinking { thinking, signature } => {
                                gemini_parts.push(adk_gemini::Part::Text {
                                    text: thinking.clone(),
                                    thought: Some(true),
                                    thought_signature: signature.clone(),
                                });
                            }
                            Part::FunctionCall { name, args, thought_signature, id } => {
                                gemini_parts.push(adk_gemini::Part::FunctionCall {
                                    function_call: adk_gemini::FunctionCall {
                                        name: name.clone(),
                                        args: args.clone(),
                                        id: id.clone(),
                                        thought_signature: None,
                                    },
                                    thought_signature: thought_signature.clone(),
                                });
                            }
                            Part::ServerToolCall { server_tool_call } => {
                                if let Ok(native_part) = serde_json::from_value::<adk_gemini::Part>(
                                    server_tool_call.clone(),
                                ) {
                                    match native_part {
                                        adk_gemini::Part::ToolCall { .. }
                                        | adk_gemini::Part::ExecutableCode { .. } => {
                                            gemini_parts.push(native_part);
                                            continue;
                                        }
                                        _ => {}
                                    }
                                }

                                gemini_parts.push(adk_gemini::Part::ToolCall {
                                    tool_call: server_tool_call.clone(),
                                    thought_signature: Self::gemini_part_thought_signature(
                                        server_tool_call,
                                    ),
                                });
                            }
                            Part::ServerToolResponse { server_tool_response } => {
                                if let Ok(native_part) = serde_json::from_value::<adk_gemini::Part>(
                                    server_tool_response.clone(),
                                ) {
                                    match native_part {
                                        adk_gemini::Part::ToolResponse { .. }
                                        | adk_gemini::Part::CodeExecutionResult { .. } => {
                                            gemini_parts.push(native_part);
                                            continue;
                                        }
                                        _ => {}
                                    }
                                }

                                gemini_parts.push(adk_gemini::Part::ToolResponse {
                                    tool_response: server_tool_response.clone(),
                                    thought_signature: Self::gemini_part_thought_signature(
                                        server_tool_response,
                                    ),
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
                    // For function responses, build content directly to attach thought_signature
                    // recovered from the preceding FunctionCall (Gemini 3.x requirement)
                    let mut gemini_parts = Vec::new();
                    for part in &content.parts {
                        if let Part::FunctionResponse { function_response, .. } = part {
                            let sig = fn_call_signatures.get(&function_response.name).cloned();
                            gemini_parts.push(adk_gemini::Part::FunctionResponse {
                                function_response: adk_gemini::tools::FunctionResponse::new(
                                    &function_response.name,
                                    Self::gemini_function_response_payload(
                                        function_response.response.clone(),
                                    ),
                                ),
                                thought_signature: sig,
                            });
                        }
                    }
                    if !gemini_parts.is_empty() {
                        let fn_content = adk_gemini::Content {
                            role: Some(adk_gemini::Role::User),
                            parts: Some(gemini_parts),
                        };
                        builder = builder.with_message(adk_gemini::Message {
                            content: fn_content,
                            role: adk_gemini::Role::User,
                        });
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

            // Attach cached content reference if provided
            if let Some(ref name) = config.cached_content {
                let handle = self.client.get_cached_content(name);
                builder = builder.with_cached_content(&handle);
            }
        }

        // Add tools
        if !req.tools.is_empty() {
            let (gemini_tools, tool_config) = Self::build_gemini_tools(&req.tools)?;
            for tool in gemini_tools {
                builder = builder.with_tool(tool);
            }
            if tool_config != adk_gemini::ToolConfig::default() {
                builder = builder.with_tool_config(tool_config);
            }
        }

        if stream {
            adk_telemetry::debug!("Executing streaming request");
            let response_stream = builder.execute_stream().await.map_err(|e| {
                adk_telemetry::error!(error = %e, "Model request failed");
                gemini_error_to_adk(&e)
            })?;

            let mapped_stream = async_stream::stream! {
                let mut stream = response_stream;
                let mut saw_partial_chunk = false;
                while let Some(result) = stream.try_next().await.transpose() {
                    match result {
                        Ok(resp) => {
                            match Self::convert_response(&resp) {
                                Ok(llm_resp) => {
                                    let (chunks, next_saw_partial) =
                                        Self::stream_chunks_from_response(llm_resp, saw_partial_chunk);
                                    saw_partial_chunk = next_saw_partial;
                                    for chunk in chunks {
                                        yield Ok(chunk);
                                    }
                                }
                                Err(e) => {
                                    adk_telemetry::error!(error = %e, "Failed to convert response");
                                    yield Err(e);
                                }
                            }
                        }
                        Err(e) => {
                            adk_telemetry::error!(error = %e, "Stream error");
                            yield Err(gemini_error_to_adk(&e));
                        }
                    }
                }
            };

            Ok(Box::pin(mapped_stream))
        } else {
            adk_telemetry::debug!("Executing blocking request");
            let response = builder.execute().await.map_err(|e| {
                adk_telemetry::error!(error = %e, "Model request failed");
                gemini_error_to_adk(&e)
            })?;

            let llm_response = Self::convert_response(&response)?;

            let stream = async_stream::stream! {
                yield Ok(llm_response);
            };

            Ok(Box::pin(stream))
        }
    }

    /// Create a cached content resource with the given system instruction, tools, and TTL.
    ///
    /// Returns the cache name (e.g., "cachedContents/abc123") on success.
    /// The cache is created using the model configured on this `GeminiModel` instance.
    pub async fn create_cached_content(
        &self,
        system_instruction: &str,
        tools: &std::collections::HashMap<String, serde_json::Value>,
        ttl_seconds: u32,
    ) -> Result<String> {
        let mut cache_builder = self
            .client
            .create_cache()
            .with_system_instruction(system_instruction)
            .with_ttl(std::time::Duration::from_secs(u64::from(ttl_seconds)));

        let (gemini_tools, tool_config) = Self::build_gemini_tools(tools)?;
        if !gemini_tools.is_empty() {
            cache_builder = cache_builder.with_tools(gemini_tools);
        }
        if tool_config != adk_gemini::ToolConfig::default() {
            cache_builder = cache_builder.with_tool_config(tool_config);
        }

        let handle = cache_builder
            .execute()
            .await
            .map_err(|e| adk_core::AdkError::model(format!("cache creation failed: {e}")))?;

        Ok(handle.name().to_string())
    }

    /// Delete a cached content resource by name.
    pub async fn delete_cached_content(&self, name: &str) -> Result<()> {
        let handle = self.client.get_cached_content(name);
        handle
            .delete()
            .await
            .map_err(|(_, e)| adk_core::AdkError::model(format!("cache deletion failed: {e}")))?;
        Ok(())
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
        let usage_span = adk_telemetry::llm_generate_span("gemini", &self.model_name, stream);
        // Retries only cover request setup/execution. Stream failures after the stream starts
        // are yielded to the caller and are not replayed automatically.
        let result = execute_with_retry(&self.retry_config, is_retryable_model_error, || {
            self.generate_content_internal(req.clone(), stream)
        })
        .await?;
        Ok(crate::usage_tracking::with_usage_tracking(result, usage_span))
    }
}

#[cfg(test)]
mod native_tool_tests {
    use super::*;

    #[test]
    fn test_build_gemini_tools_supports_native_tool_metadata() {
        let mut tools = std::collections::HashMap::new();
        tools.insert(
            "google_search".to_string(),
            serde_json::json!({
                "x-adk-gemini-tool": {
                    "google_search": {}
                }
            }),
        );
        tools.insert(
            "lookup_weather".to_string(),
            serde_json::json!({
                "name": "lookup_weather",
                "description": "lookup weather",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    }
                }
            }),
        );

        let (gemini_tools, tool_config) =
            GeminiModel::build_gemini_tools(&tools).expect("tool conversion should succeed");

        assert_eq!(gemini_tools.len(), 2);
        assert_eq!(tool_config.include_server_side_tool_invocations, Some(true));
    }

    #[test]
    fn test_build_gemini_tools_sets_flag_for_builtin_only() {
        let mut tools = std::collections::HashMap::new();
        tools.insert(
            "google_search".to_string(),
            serde_json::json!({
                "x-adk-gemini-tool": {
                    "google_search": {}
                }
            }),
        );

        let (_gemini_tools, tool_config) =
            GeminiModel::build_gemini_tools(&tools).expect("tool conversion should succeed");

        assert_eq!(
            tool_config.include_server_side_tool_invocations,
            Some(true),
            "includeServerSideToolInvocations should be set even with only built-in tools"
        );
    }

    #[test]
    fn test_build_gemini_tools_no_flag_for_function_only() {
        let mut tools = std::collections::HashMap::new();
        tools.insert(
            "lookup_weather".to_string(),
            serde_json::json!({
                "name": "lookup_weather",
                "description": "lookup weather",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    }
                }
            }),
        );

        let (_gemini_tools, tool_config) =
            GeminiModel::build_gemini_tools(&tools).expect("tool conversion should succeed");

        assert_eq!(
            tool_config.include_server_side_tool_invocations, None,
            "includeServerSideToolInvocations should NOT be set for function-only tools"
        );
    }

    #[test]
    fn test_build_gemini_tools_merges_native_tool_config() {
        let mut tools = std::collections::HashMap::new();
        tools.insert(
            "google_maps".to_string(),
            serde_json::json!({
                "x-adk-gemini-tool": {
                    "google_maps": {
                        "enable_widget": true
                    }
                },
                "x-adk-gemini-tool-config": {
                    "retrievalConfig": {
                        "latLng": {
                            "latitude": 1.23,
                            "longitude": 4.56
                        }
                    }
                }
            }),
        );

        let (_gemini_tools, tool_config) =
            GeminiModel::build_gemini_tools(&tools).expect("tool conversion should succeed");

        assert_eq!(
            tool_config.retrieval_config,
            Some(serde_json::json!({
                "latLng": {
                    "latitude": 1.23,
                    "longitude": 4.56
                }
            }))
        );
    }
}

#[async_trait]
impl CacheCapable for GeminiModel {
    async fn create_cache(
        &self,
        system_instruction: &str,
        tools: &std::collections::HashMap<String, serde_json::Value>,
        ttl_seconds: u32,
    ) -> Result<String> {
        self.create_cached_content(system_instruction, tools, ttl_seconds).await
    }

    async fn delete_cache(&self, name: &str) -> Result<()> {
        self.delete_cached_content(name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::AdkError;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        },
        time::Duration,
    };

    #[test]
    fn constructor_is_backward_compatible_and_sync() {
        fn accepts_sync_constructor<F>(_f: F)
        where
            F: Fn(&str, &str) -> Result<GeminiModel>,
        {
        }

        accepts_sync_constructor(|api_key, model| GeminiModel::new(api_key, model));
    }

    #[test]
    fn stream_chunks_from_response_injects_partial_before_lone_final_chunk() {
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("hello")),
            usage_metadata: None,
            finish_reason: Some(FinishReason::Stop),
            citation_metadata: None,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
            provider_metadata: None,
        };

        let (chunks, saw_partial) = GeminiModel::stream_chunks_from_response(response, false);
        assert!(saw_partial);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].partial);
        assert!(!chunks[0].turn_complete);
        assert!(chunks[0].content.is_none());
        assert!(!chunks[1].partial);
        assert!(chunks[1].turn_complete);
    }

    #[test]
    fn stream_chunks_from_response_keeps_final_only_when_partial_already_seen() {
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("done")),
            usage_metadata: None,
            finish_reason: Some(FinishReason::Stop),
            citation_metadata: None,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
            provider_metadata: None,
        };

        let (chunks, saw_partial) = GeminiModel::stream_chunks_from_response(response, true);
        assert!(saw_partial);
        assert_eq!(chunks.len(), 1);
        assert!(!chunks[0].partial);
        assert!(chunks[0].turn_complete);
    }

    #[tokio::test]
    async fn execute_with_retry_retries_retryable_errors() {
        let retry_config = RetryConfig::default()
            .with_max_retries(2)
            .with_initial_delay(Duration::from_millis(0))
            .with_max_delay(Duration::from_millis(0));
        let attempts = Arc::new(AtomicU32::new(0));

        let result = execute_with_retry(&retry_config, is_retryable_model_error, || {
            let attempts = Arc::clone(&attempts);
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    return Err(AdkError::model("code 429 RESOURCE_EXHAUSTED"));
                }
                Ok("ok")
            }
        })
        .await
        .expect("retry should eventually succeed");

        assert_eq!(result, "ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn execute_with_retry_does_not_retry_non_retryable_errors() {
        let retry_config = RetryConfig::default()
            .with_max_retries(3)
            .with_initial_delay(Duration::from_millis(0))
            .with_max_delay(Duration::from_millis(0));
        let attempts = Arc::new(AtomicU32::new(0));

        let error = execute_with_retry(&retry_config, is_retryable_model_error, || {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(AdkError::model("code 400 invalid request"))
            }
        })
        .await
        .expect_err("non-retryable error should return immediately");

        assert!(error.is_model());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn execute_with_retry_respects_disabled_config() {
        let retry_config = RetryConfig::disabled().with_max_retries(10);
        let attempts = Arc::new(AtomicU32::new(0));

        let error = execute_with_retry(&retry_config, is_retryable_model_error, || {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(AdkError::model("code 429 RESOURCE_EXHAUSTED"))
            }
        })
        .await
        .expect_err("disabled retries should return first error");

        assert!(error.is_model());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn convert_response_preserves_citation_metadata() {
        let response = adk_gemini::GenerationResponse {
            candidates: vec![adk_gemini::Candidate {
                content: adk_gemini::Content {
                    role: Some(adk_gemini::Role::Model),
                    parts: Some(vec![adk_gemini::Part::Text {
                        text: "hello world".to_string(),
                        thought: None,
                        thought_signature: None,
                    }]),
                },
                safety_ratings: None,
                citation_metadata: Some(adk_gemini::CitationMetadata {
                    citation_sources: vec![adk_gemini::CitationSource {
                        uri: Some("https://example.com".to_string()),
                        title: Some("Example".to_string()),
                        start_index: Some(0),
                        end_index: Some(5),
                        license: Some("CC-BY".to_string()),
                        publication_date: None,
                    }],
                }),
                grounding_metadata: None,
                finish_reason: Some(adk_gemini::FinishReason::Stop),
                index: Some(0),
            }],
            prompt_feedback: None,
            usage_metadata: None,
            model_version: None,
            response_id: None,
        };

        let converted =
            GeminiModel::convert_response(&response).expect("conversion should succeed");
        let metadata = converted.citation_metadata.expect("citation metadata should be mapped");
        assert_eq!(metadata.citation_sources.len(), 1);
        assert_eq!(metadata.citation_sources[0].uri.as_deref(), Some("https://example.com"));
        assert_eq!(metadata.citation_sources[0].start_index, Some(0));
        assert_eq!(metadata.citation_sources[0].end_index, Some(5));
    }

    #[test]
    fn convert_response_handles_inline_data_from_model() {
        let image_bytes = vec![0x89, 0x50, 0x4E, 0x47];
        let encoded = crate::attachment::encode_base64(&image_bytes);

        let response = adk_gemini::GenerationResponse {
            candidates: vec![adk_gemini::Candidate {
                content: adk_gemini::Content {
                    role: Some(adk_gemini::Role::Model),
                    parts: Some(vec![
                        adk_gemini::Part::Text {
                            text: "Here is the image".to_string(),
                            thought: None,
                            thought_signature: None,
                        },
                        adk_gemini::Part::InlineData {
                            inline_data: adk_gemini::Blob {
                                mime_type: "image/png".to_string(),
                                data: encoded,
                            },
                        },
                    ]),
                },
                safety_ratings: None,
                citation_metadata: None,
                grounding_metadata: None,
                finish_reason: Some(adk_gemini::FinishReason::Stop),
                index: Some(0),
            }],
            prompt_feedback: None,
            usage_metadata: None,
            model_version: None,
            response_id: None,
        };

        let converted =
            GeminiModel::convert_response(&response).expect("conversion should succeed");
        let content = converted.content.expect("should have content");
        assert!(
            content
                .parts
                .iter()
                .any(|part| matches!(part, Part::Text { text } if text == "Here is the image"))
        );
        assert!(content.parts.iter().any(|part| {
            matches!(
                part,
                Part::InlineData { mime_type, data }
                    if mime_type == "image/png" && data.as_slice() == image_bytes.as_slice()
            )
        }));
    }

    #[test]
    fn gemini_function_response_payload_preserves_objects() {
        let value = serde_json::json!({
            "documents": [
                { "id": "pricing", "score": 0.91 }
            ]
        });

        let payload = GeminiModel::gemini_function_response_payload(value.clone());

        assert_eq!(payload, value);
    }

    #[test]
    fn gemini_function_response_payload_wraps_arrays() {
        let payload =
            GeminiModel::gemini_function_response_payload(serde_json::json!([{ "id": "pricing" }]));

        assert_eq!(payload, serde_json::json!({ "result": [{ "id": "pricing" }] }));
    }
}
