use crate::attachment;
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{
    CacheCapable, CitationMetadata, CitationSource, Content, FinishReason, Llm, LlmRequest,
    LlmResponse, LlmResponseStream, Part, Result, UsageMetadata,
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

impl GeminiModel {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        let model_name = model.into();
        let client = Gemini::with_model(api_key.into(), model_name.clone())
            .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

        Ok(Self { client, model_name, retry_config: RetryConfig::default() })
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

        Ok(Self { client, model_name, retry_config: RetryConfig::default() })
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

        Ok(Self { client, model_name, retry_config: RetryConfig::default() })
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

        Ok(Self { client, model_name, retry_config: RetryConfig::default() })
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
                                adk_core::AdkError::Model(format!(
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
                            id: None,
                            thought_signature: thought_signature.clone(),
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
        })
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
        };

        (vec![synthetic_partial, response], true)
    }

    async fn generate_content_internal(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream> {
        // Helper to format the full error chain (Display + all source errors)
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
                            Part::FunctionCall { name, args, thought_signature, .. } => {
                                gemini_parts.push(adk_gemini::Part::FunctionCall {
                                    function_call: adk_gemini::FunctionCall {
                                        name: name.clone(),
                                        args: args.clone(),
                                        thought_signature: thought_signature.clone(),
                                    },
                                    thought_signature: thought_signature.clone(),
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

            // Attach cached content reference if provided
            if let Some(ref name) = config.cached_content {
                let handle = self.client.get_cached_content(name);
                builder = builder.with_cached_content(&handle);
            }
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
                adk_core::AdkError::Model(format_error_chain(&e))
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
                            yield Err(adk_core::AdkError::Model(format_error_chain(&e)));
                        }
                    }
                }
            };

            Ok(Box::pin(mapped_stream))
        } else {
            adk_telemetry::debug!("Executing blocking request");
            let response = builder.execute().await.map_err(|e| {
                adk_telemetry::error!(error = %e, "Model request failed");
                adk_core::AdkError::Model(format_error_chain(&e))
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

        // Convert ADK tool definitions to Gemini FunctionDeclarations
        let mut function_declarations = Vec::new();
        for (name, tool_decl) in tools {
            if name == "google_search" {
                continue;
            }
            if let Ok(func_decl) =
                serde_json::from_value::<adk_gemini::FunctionDeclaration>(tool_decl.clone())
            {
                function_declarations.push(func_decl);
            }
        }
        if !function_declarations.is_empty() {
            cache_builder = cache_builder
                .with_tools(vec![adk_gemini::Tool::with_functions(function_declarations)]);
        }

        let handle = cache_builder
            .execute()
            .await
            .map_err(|e| adk_core::AdkError::Model(format!("cache creation failed: {e}")))?;

        Ok(handle.name().to_string())
    }

    /// Delete a cached content resource by name.
    pub async fn delete_cached_content(&self, name: &str) -> Result<()> {
        let handle = self.client.get_cached_content(name);
        handle
            .delete()
            .await
            .map_err(|(_, e)| adk_core::AdkError::Model(format!("cache deletion failed: {e}")))?;
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
        // Retries only cover request setup/execution. Stream failures after the stream starts
        // are yielded to the caller and are not replayed automatically.
        execute_with_retry(&self.retry_config, is_retryable_model_error, || {
            self.generate_content_internal(req.clone(), stream)
        })
        .await
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
                    return Err(AdkError::Model("code 429 RESOURCE_EXHAUSTED".to_string()));
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
                Err::<(), _>(AdkError::Model("code 400 invalid request".to_string()))
            }
        })
        .await
        .expect_err("non-retryable error should return immediately");

        assert!(matches!(error, AdkError::Model(_)));
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
                Err::<(), _>(AdkError::Model("code 429 RESOURCE_EXHAUSTED".to_string()))
            }
        })
        .await
        .expect_err("disabled retries should return first error");

        assert!(matches!(error, AdkError::Model(_)));
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
}
