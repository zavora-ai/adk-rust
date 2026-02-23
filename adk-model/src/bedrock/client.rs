//! Amazon Bedrock client implementation.
//!
//! Uses the AWS SDK Converse API for both streaming and non-streaming
//! inference. Credentials are loaded automatically from the environment
//! via `aws-config` (environment variables, shared config, IMDS, etc.).

use super::config::BedrockConfig;
use super::convert::{
    adk_request_to_bedrock, bedrock_response_to_adk, bedrock_stream_content_start_to_adk,
    bedrock_stream_delta_to_adk, bedrock_stream_stop_to_adk,
};
use crate::retry::RetryConfig;
use adk_core::{AdkError, Llm, LlmRequest, LlmResponse, LlmResponseStream};
use async_stream::try_stream;
use async_trait::async_trait;
use aws_sdk_bedrockruntime::types::ConverseStreamOutput;
use tracing::{debug, info, instrument};

/// Amazon Bedrock client backed by the AWS SDK Converse API.
///
/// Supports both streaming (`converse_stream`) and non-streaming (`converse`)
/// inference. AWS credentials are loaded from the standard credential chain
/// (environment variables, `~/.aws/credentials`, IMDS, etc.).
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::bedrock::{BedrockClient, BedrockConfig};
///
/// let config = BedrockConfig::new("us-east-1", "us.anthropic.claude-sonnet-4-6");
/// let client = BedrockClient::new(config).await?;
///
/// // Use via the Llm trait
/// let response = client.generate_content(request, false).await?;
/// ```
pub struct BedrockClient {
    client: aws_sdk_bedrockruntime::Client,
    model_id: String,
    region: String,
    retry_config: RetryConfig,
    prompt_caching: Option<super::config::BedrockCacheConfig>,
}

impl BedrockClient {
    /// Create a new Bedrock client from the given configuration.
    ///
    /// Loads AWS credentials from the standard credential chain
    /// (environment variables, shared config, IMDS, etc.) and constructs
    /// an `aws_sdk_bedrockruntime::Client`.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Model` if the AWS SDK configuration fails to load.
    pub async fn new(config: BedrockConfig) -> Result<Self, AdkError> {
        let region = config.region.clone();
        let model_id = config.model_id.clone();
        let prompt_caching = config.prompt_caching.clone();

        let mut sdk_config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(config.region.clone()));

        if let Some(endpoint_url) = &config.endpoint_url {
            sdk_config_loader = sdk_config_loader.endpoint_url(endpoint_url);
        }

        let sdk_config = sdk_config_loader.load().await;
        let client = aws_sdk_bedrockruntime::Client::new(&sdk_config);

        info!("bedrock client created for region={region}, model={model_id}");

        Ok(Self { client, model_id, region, retry_config: RetryConfig::default(), prompt_caching })
    }

    /// Set the retry configuration, consuming and returning `self`.
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    /// Set the retry configuration by mutable reference.
    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.retry_config = retry_config;
    }

    /// Returns a reference to the current retry configuration.
    pub fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }
}

#[async_trait]
impl Llm for BedrockClient {
    fn name(&self) -> &str {
        &self.model_id
    }

    #[instrument(skip_all, fields(model_id = %self.model_id, region = %self.region, stream))]
    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        let bedrock_input = adk_request_to_bedrock(
            &request.contents,
            &request.tools,
            request.config.as_ref(),
            self.prompt_caching.as_ref(),
        )
        .map_err(|e| {
            AdkError::Model(format!(
                "Bedrock request conversion failed for region={}, model={}: {e}",
                self.region, self.model_id
            ))
        })?;

        if stream {
            self.generate_streaming(bedrock_input).await
        } else {
            self.generate_non_streaming(bedrock_input).await
        }
    }
}

impl BedrockClient {
    /// Non-streaming: call `converse` and wrap the single response in a stream.
    async fn generate_non_streaming(
        &self,
        input: super::convert::BedrockConverseInput,
    ) -> Result<LlmResponseStream, AdkError> {
        debug!("bedrock non-streaming converse for model={}", self.model_id);

        let response = self
            .client
            .converse()
            .model_id(&self.model_id)
            .set_messages(Some(input.messages))
            .set_system(Some(input.system))
            .set_inference_config(input.inference_config)
            .set_tool_config(input.tool_config)
            .send()
            .await
            .map_err(|e| {
                AdkError::Model(format!(
                    "Bedrock API error for region={}, model={}: {e}",
                    self.region, self.model_id
                ))
            })?;

        let output = response.output.ok_or_else(|| {
            AdkError::Model(format!("Bedrock response missing output for model={}", self.model_id))
        })?;

        let adk_response =
            bedrock_response_to_adk(&output, &response.stop_reason, response.usage.as_ref());

        let response_stream = try_stream! {
            yield adk_response;
        };

        Ok(Box::pin(response_stream))
    }

    /// Streaming: call `converse_stream` and convert each event to an `LlmResponse`.
    async fn generate_streaming(
        &self,
        input: super::convert::BedrockConverseInput,
    ) -> Result<LlmResponseStream, AdkError> {
        debug!("bedrock streaming converse for model={}", self.model_id);

        let mut stream_output = self
            .client
            .converse_stream()
            .model_id(&self.model_id)
            .set_messages(Some(input.messages))
            .set_system(Some(input.system))
            .set_inference_config(input.inference_config)
            .set_tool_config(input.tool_config)
            .send()
            .await
            .map_err(|e| {
                AdkError::Model(format!(
                    "Bedrock API error for region={}, model={}: {e}",
                    self.region, self.model_id
                ))
            })?;

        let model_id = self.model_id.clone();
        let region = self.region.clone();

        let response_stream = try_stream! {
            // Track tool use state for accumulating partial JSON arguments.
            let mut tool_name: Option<String> = None;
            let mut tool_id: Option<String> = None;
            let mut tool_args_buf = String::new();
            // Track reasoning signature for the current reasoning content block.
            let mut reasoning_signature: Option<String> = None;
            // Buffer the stop response and usage metadata so they can be merged.
            // Bedrock sends Metadata after MessageStop, so we defer emitting
            // the final chunk until the stream ends.
            let mut pending_stop: Option<LlmResponse> = None;
            let mut pending_usage: Option<adk_core::UsageMetadata> = None;

            while let Some(event) = stream_output.stream.recv().await.map_err(|e| {
                AdkError::Model(format!(
                    "Bedrock stream error for region={region}, model={model_id}: {e}"
                ))
            })? {
                match event {
                    ConverseStreamOutput::ContentBlockStart(start_event) => {
                        if let Some(start) = &start_event.start {
                            // If starting a tool use block, track the name and ID.
                            if let aws_sdk_bedrockruntime::types::ContentBlockStart::ToolUse(
                                tool_start,
                            ) = start
                            {
                                tool_name = Some(tool_start.name.clone());
                                tool_id = Some(tool_start.tool_use_id.clone());
                                tool_args_buf.clear();
                            }

                            if let Some(response) = bedrock_stream_content_start_to_adk(start) {
                                yield response;
                            }
                        }
                    }
                    ConverseStreamOutput::ContentBlockDelta(delta_event) => {
                        if let Some(delta) = &delta_event.delta {
                            // Accumulate tool use argument deltas.
                            if let aws_sdk_bedrockruntime::types::ContentBlockDelta::ToolUse(
                                tool_delta,
                            ) = delta
                            {
                                tool_args_buf.push_str(&tool_delta.input);
                            }

                            // Capture reasoning signature deltas for later attachment.
                            if let aws_sdk_bedrockruntime::types::ContentBlockDelta::ReasoningContent(
                                reasoning_delta,
                            ) = delta
                            {
                                if let Ok(sig) = reasoning_delta.as_signature() {
                                    reasoning_signature = Some(sig.clone());
                                }
                            }

                            if let Some(response) = bedrock_stream_delta_to_adk(delta) {
                                yield response;
                            }
                        }
                    }
                    ConverseStreamOutput::ContentBlockStop(_) => {
                        // If we were accumulating tool arguments, emit the complete FunctionCall.
                        if let (Some(name), Some(id)) = (tool_name.take(), tool_id.take()) {
                            let args: serde_json::Value =
                                serde_json::from_str(&tool_args_buf).unwrap_or_default();
                            tool_args_buf.clear();

                            yield LlmResponse {
                                content: Some(adk_core::Content {
                                    role: "model".to_string(),
                                    parts: vec![adk_core::Part::FunctionCall {
                                        name,
                                        args,
                                        id: Some(id),
                                        thought_signature: None,
                                    }],
                                }),
                                usage_metadata: None,
                                finish_reason: None,
                                citation_metadata: None,
                                partial: false,
                                turn_complete: false,
                                interrupted: false,
                                error_code: None,
                                error_message: None,
                            };
                        }

                        // If we accumulated a reasoning signature, emit it as a
                        // Part::Thinking with the signature so downstream consumers
                        // can attach it to the reasoning block.
                        if let Some(sig) = reasoning_signature.take() {
                            yield LlmResponse {
                                content: Some(adk_core::Content {
                                    role: "model".to_string(),
                                    parts: vec![adk_core::Part::Thinking {
                                        thinking: String::new(),
                                        signature: Some(sig),
                                    }],
                                }),
                                usage_metadata: None,
                                finish_reason: None,
                                citation_metadata: None,
                                partial: true,
                                turn_complete: false,
                                interrupted: false,
                                error_code: None,
                                error_message: None,
                            };
                        }
                    }
                    ConverseStreamOutput::MessageStart(_) => {
                        // MessageStart carries the role; no content to emit.
                    }
                    ConverseStreamOutput::MessageStop(stop_event) => {
                        // Buffer the stop chunk — Metadata arrives after this.
                        pending_stop = Some(bedrock_stream_stop_to_adk(&stop_event.stop_reason));
                    }
                    ConverseStreamOutput::Metadata(metadata_event) => {
                        if let Some(usage) = &metadata_event.usage {
                            pending_usage = Some(adk_core::UsageMetadata {
                                prompt_token_count: usage.input_tokens,
                                candidates_token_count: usage.output_tokens,
                                total_token_count: usage.total_tokens,
                                cache_read_input_token_count: usage.cache_read_input_tokens,
                                cache_creation_input_token_count: usage.cache_write_input_tokens,
                                ..Default::default()
                            });
                        }
                    }
                    _ => {
                        // Unknown event variant — skip.
                    }
                }
            }

            // Emit the final stop chunk with usage metadata merged in.
            if let Some(mut stop) = pending_stop {
                stop.usage_metadata = pending_usage.take();
                yield stop;
            }
        };

        Ok(Box::pin(response_stream))
    }
}
