use adk_core::{
    Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result,
    UsageMetadata,
};
use adk_gemini::{Gemini, GeminiBuilder};
use async_trait::async_trait;
use std::time::Duration;

/// Configuration for retry logic
#[derive(Clone, Debug)]
pub struct RetryConfig {
    /// Maximum number of retries for rate-limited requests
    pub max_retries: u32,
    /// Initial delay before first retry
    pub initial_delay: Duration,
    /// Maximum delay between retries
    pub max_delay: Duration,
    /// Multiplier for exponential backoff (e.g., 2.0 doubles the delay each time)
    pub backoff_multiplier: f32,
    /// Whether retries are enabled
    pub enabled: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            backoff_multiplier: 2.0,
            enabled: true,
        }
    }
}

impl RetryConfig {
    /// Create a new RetryConfig with custom settings
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum number of retries
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Set the initial delay before first retry
    pub fn with_initial_delay(mut self, delay: Duration) -> Self {
        self.initial_delay = delay;
        self
    }

    /// Set the maximum delay between retries
    pub fn with_max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = delay;
        self
    }

    /// Set the backoff multiplier
    pub fn with_backoff_multiplier(mut self, multiplier: f32) -> Self {
        self.backoff_multiplier = multiplier;
        self
    }

    /// Disable retries entirely
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

pub struct GeminiModel {
    client: Gemini,
    model_name: String,
    retry_config: RetryConfig,
}

/// Builder for creating GeminiModel instances
pub struct GeminiModelBuilder {
    api_key: Option<String>,
    model_name: String,
    retry_config: RetryConfig,
    // For service account auth
    service_account_json: Option<String>,
    project_id: Option<String>,
    location: Option<String>,
}

impl GeminiModelBuilder {
    /// Create a new builder for API key authentication
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            api_key: None,
            model_name: model.into(),
            retry_config: RetryConfig::default(),
            service_account_json: None,
            project_id: None,
            location: None,
        }
    }

    /// Set the API key for authentication
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Set service account JSON for authentication
    pub fn service_account_json(mut self, json: impl Into<String>) -> Self {
        self.service_account_json = Some(json.into());
        self
    }

    /// Set service account from file path
    pub fn service_account_path(self, path: impl AsRef<std::path::Path>) -> Result<Self> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| adk_core::AdkError::Model(format!("Failed to read service account file: {}", e)))?;
        Ok(self.service_account_json(json))
    }

    /// Set project ID (required for service account auth)
    pub fn project_id(mut self, id: impl Into<String>) -> Self {
        self.project_id = Some(id.into());
        self
    }

    /// Set location (required for service account auth, defaults to "us-central1")
    pub fn location(mut self, loc: impl Into<String>) -> Self {
        self.location = Some(loc.into());
        self
    }

    /// Set retry configuration
    pub fn retry_config(mut self, config: RetryConfig) -> Self {
        self.retry_config = config;
        self
    }

    /// Configure retries using a closure
    pub fn configure_retries<F>(mut self, f: F) -> Self
    where
        F: FnOnce(RetryConfig) -> RetryConfig,
    {
        self.retry_config = f(self.retry_config);
        self
    }

    /// Build the GeminiModel
    pub async fn build(self) -> Result<GeminiModel> {
        match (self.api_key, self.service_account_json) {
            (Some(api_key), None) => {
                // API key authentication
                let client = Gemini::new(api_key)
                    .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

                Ok(GeminiModel {
                    client,
                    model_name: self.model_name,
                    retry_config: self.retry_config,
                })
            }
            (None, Some(service_account_json)) => {
                // Service account authentication
                let project_id = self.project_id
                    .ok_or_else(|| adk_core::AdkError::Model("project_id is required for service account authentication".to_string()))?;
                let location = self.location.unwrap_or_else(|| "us-central1".to_string());

                Self::build_with_service_account(
                    service_account_json,
                    project_id,
                    location,
                    self.model_name,
                    self.retry_config,
                ).await
            }
            (Some(_), Some(_)) => {
                Err(adk_core::AdkError::Model("Cannot use both API key and service account authentication".to_string()))
            }
            (None, None) => {
                Err(adk_core::AdkError::Model("Either api_key or service_account must be provided".to_string()))
            }
        }
    }

    async fn build_with_service_account(
        service_account_json: String,
        project_id: String,
        location: String,
        model_name: String,
        retry_config: RetryConfig,
    ) -> Result<GeminiModel> {
        // Create a GCP auth provider from service account JSON
        let service_account = gcp_auth::CustomServiceAccount::from_json(&service_account_json)
            .map_err(|e| adk_core::AdkError::Model(format!("Failed to parse service account JSON: {}", e)))?;

        // Get access token with the required scopes
        use gcp_auth::TokenProvider;
        let scopes = &["https://www.googleapis.com/auth/cloud-platform"];
        let token = service_account.token(scopes)
            .await
            .map_err(|e| adk_core::AdkError::Model(format!("Failed to get access token: {}", e)))?;

        // Create HTTP client with Bearer token
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token.as_str()))
                .map_err(|e| adk_core::AdkError::Model(format!("Invalid access token: {}", e)))?,
        );

        let http_client = reqwest::ClientBuilder::new()
            .default_headers(headers);

        // Build Vertex AI endpoint URL
        let base_url = format!(
            "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/google/",
            location, project_id, location
        );
        let vertex_url = reqwest::Url::parse(&base_url)
            .map_err(|e| adk_core::AdkError::Model(format!("Invalid Vertex AI URL: {}", e)))?;

        // Create Gemini client with Vertex AI endpoint
        let vertex_model = format!("models/{}", model_name);
        let client = GeminiBuilder::new("")
            .with_model(vertex_model.clone())
            .with_base_url(vertex_url)
            .with_http_client(http_client)
            .build()
            .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

        Ok(GeminiModel {
            client,
            model_name: vertex_model,
            retry_config,
        })
    }
}

impl GeminiModel {
    /// Create a builder for constructing a GeminiModel
    pub fn builder(model: impl Into<String>) -> GeminiModelBuilder {
        GeminiModelBuilder::new(model)
    }

    /// Create a new GeminiModel with an API key (convenience method)
    pub async fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        GeminiModelBuilder::new(model)
            .api_key(api_key)
            .build()
            .await
    }


    /// Create a new GeminiModel using service account authentication (from JSON string)
    ///
    /// This is a convenience method. For more control, use the builder.
    pub async fn new_with_service_account_json(
        service_account_json: impl Into<String>,
        project_id: impl Into<String>,
        location: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        GeminiModelBuilder::new(model)
            .service_account_json(service_account_json)
            .project_id(project_id)
            .location(location)
            .build()
            .await
    }

    /// Create a new GeminiModel using service account authentication (from file path)
    ///
    /// This is a convenience method. For more control, use the builder.
    pub async fn new_with_service_account_path(
        service_account_path: impl AsRef<std::path::Path>,
        project_id: impl Into<String>,
        location: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let json_content = std::fs::read_to_string(service_account_path)
            .map_err(|e| adk_core::AdkError::Model(format!("Failed to read service account file: {}", e)))?;

        Self::new_with_service_account_json(json_content, project_id, location, model).await
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

    /// Internal method that does the actual generation without retry logic
    async fn generate_content_internal(&self, req: LlmRequest, stream: bool) -> Result<LlmResponseStream> {
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

    /// Check if an error is a rate limit error (429)
    fn is_rate_limit_error(&self, error: &adk_core::AdkError) -> bool {
        match error {
            adk_core::AdkError::Model(msg) => {
                msg.contains("429") || msg.contains("RESOURCE_EXHAUSTED") || msg.contains("Resource exhausted")
            }
            _ => false,
        }
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

        // If retries are disabled, call internal method directly
        if !self.retry_config.enabled {
            return self.generate_content_internal(req, stream).await;
        }

        // Retry logic with exponential backoff for rate limiting (429 errors)
        let mut retries = 0;
        let mut delay = self.retry_config.initial_delay;

        loop {
            let result = self.generate_content_internal(req.clone(), stream).await;

            match result {
                Ok(response) => return Ok(response),
                Err(e) if retries < self.retry_config.max_retries && self.is_rate_limit_error(&e) => {
                    retries += 1;
                    adk_telemetry::warn!(
                        "Rate limited (429), retrying {}/{} after {:?}",
                        retries, self.retry_config.max_retries, delay
                    );
                    tokio::time::sleep(delay).await;
                    // Exponential backoff with configurable multiplier
                    let next_delay = delay.as_secs_f64() * self.retry_config.backoff_multiplier as f64;
                    delay = Duration::from_secs_f64(next_delay).min(self.retry_config.max_delay);
                }
                Err(e) => return Err(e),
            }
        }
    }
}
