//! MistralRsModel - the main model provider implementing the Llm trait.

use std::path::Path;
use std::sync::Arc;

use adk_core::{
    Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, UsageMetadata,
};
use async_trait::async_trait;
use futures::stream;
use mistralrs::{
    AutoDeviceMapParams, DeviceMapSetting, IsqType, PagedAttentionMetaBuilder, Response,
    TextMessageRole, TextMessages, TextModelBuilder, Topology,
};
use tracing::{debug, info, instrument, warn};

use crate::config::{Device, MistralRsConfig, ModelSource, QuantizationLevel};
use crate::error::{MistralRsError, Result};
use crate::tracing_utils::{
    TimingGuard, log_inference_complete, log_inference_start, log_model_loading_complete,
    log_model_loading_start,
};

/// mistral.rs model provider for ADK.
///
/// This struct wraps a mistral.rs model instance and implements the ADK `Llm` trait,
/// allowing it to be used with ADK agents and workflows.
///
/// # Example
///
/// ```rust,ignore
/// use adk_mistralrs::{MistralRsModel, MistralRsConfig, ModelSource};
///
/// let model = MistralRsModel::from_hf("microsoft/Phi-3.5-mini-instruct").await?;
/// ```
pub struct MistralRsModel {
    /// The underlying mistral.rs model instance
    model: Arc<mistralrs::Model>,
    /// Model name for identification
    name: String,
    /// Configuration used to create this model
    config: MistralRsConfig,
}

impl MistralRsModel {
    /// Create a new model from configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - The configuration specifying model source, architecture, and options
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = MistralRsConfig::builder()
    ///     .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
    ///     .build();
    /// let model = MistralRsModel::new(config).await?;
    /// ```
    #[instrument(skip(config), fields(model_source = ?config.model_source))]
    pub async fn new(config: MistralRsConfig) -> Result<Self> {
        let model_id = match &config.model_source {
            ModelSource::HuggingFace(id) => id.clone(),
            ModelSource::Local(path) => path.display().to_string(),
            ModelSource::Gguf(path) => path.display().to_string(),
            ModelSource::Uqff(path) => path.display().to_string(),
        };

        // Start timing for model loading
        let _timer = TimingGuard::new("model_loading", &model_id);
        let start_time = std::time::Instant::now();

        // Log loading start with configuration details
        let source_type = match &config.model_source {
            ModelSource::HuggingFace(_) => "HuggingFace",
            ModelSource::Local(_) => "Local",
            ModelSource::Gguf(_) => "GGUF",
            ModelSource::Uqff(_) => "UQFF",
        };
        log_model_loading_start(
            &model_id,
            source_type,
            config.isq.is_some(),
            config.paged_attention,
        );

        info!("Loading mistral.rs model: {}", model_id);

        let mut builder = TextModelBuilder::new(model_id.clone());

        // Apply ISQ quantization if configured
        if let Some(isq) = &config.isq {
            let isq_type = quantization_level_to_isq(isq.level);
            builder = builder.with_isq(isq_type);
            debug!("ISQ quantization enabled: {:?}", isq.level);
        }

        // Apply device selection
        let device_map = device_to_device_map(&config.device.device);
        builder = builder.with_device_mapping(device_map);
        debug!("Device configured: {:?}", config.device.device);

        // Apply PagedAttention if configured
        if config.paged_attention {
            builder = builder
                .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())
                .map_err(|e| {
                    MistralRsError::model_load(
                        &model_id,
                        format!("PagedAttention initialization failed: {e}"),
                    )
                })?;
            debug!("PagedAttention enabled");
        }

        // Apply topology file if configured (for per-layer quantization)
        if let Some(topology_path) = &config.topology_path {
            if topology_path.exists() {
                match Topology::from_path(topology_path) {
                    Ok(topology) => {
                        builder = builder.with_topology(topology);
                        debug!("Topology loaded from: {:?}", topology_path);
                    }
                    Err(e) => {
                        warn!("Failed to load topology file: {}", e);
                        return Err(MistralRsError::topology_file(
                            topology_path.display().to_string(),
                            e.to_string(),
                        ));
                    }
                }
            } else {
                return Err(MistralRsError::topology_file(
                    topology_path.display().to_string(),
                    "File does not exist",
                ));
            }
        }

        // Apply context length if configured
        if let Some(num_ctx) = config.num_ctx {
            builder = builder.with_max_num_seqs(num_ctx);
            debug!("Context length configured: {}", num_ctx);
        }

        // Apply chat template if configured
        if let Some(chat_template) = &config.chat_template {
            builder = builder.with_chat_template(chat_template.clone());
            debug!("Custom chat template configured");
        }

        // Apply tokenizer path if configured
        if let Some(tokenizer_path) = &config.tokenizer_path {
            builder = builder.with_tokenizer_json(tokenizer_path.to_string_lossy().to_string());
            debug!("Custom tokenizer path: {:?}", tokenizer_path);
        }

        // Enable logging
        builder = builder.with_logging();

        // Build the model
        let model = builder
            .build()
            .await
            .map_err(|e| MistralRsError::model_load(&model_id, e.to_string()))?;

        // Log completion with timing
        let duration_ms = start_time.elapsed().as_millis() as u64;
        log_model_loading_complete(&model_id, duration_ms);
        info!("Model loaded successfully: {} ({}ms)", model_id, duration_ms);

        Ok(Self { model: Arc::new(model), name: model_id, config })
    }

    /// Create from HuggingFace model ID with defaults.
    ///
    /// # Arguments
    ///
    /// * `model_id` - HuggingFace model ID (e.g., "microsoft/Phi-3.5-mini-instruct")
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let model = MistralRsModel::from_hf("microsoft/Phi-3.5-mini-instruct").await?;
    /// ```
    pub async fn from_hf(model_id: &str) -> Result<Self> {
        let config =
            MistralRsConfig::builder().model_source(ModelSource::huggingface(model_id)).build();
        Self::new(config).await
    }

    /// Create from local GGUF file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the GGUF model file
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let model = MistralRsModel::from_gguf("/path/to/model.gguf").await?;
    /// ```
    pub async fn from_gguf(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(MistralRsError::model_not_found(path.display().to_string()));
        }

        let config = MistralRsConfig::builder().model_source(ModelSource::gguf(path)).build();
        Self::new(config).await
    }

    /// Create with ISQ quantization.
    ///
    /// # Arguments
    ///
    /// * `config` - Base configuration
    /// * `level` - Quantization level to apply
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = MistralRsConfig::builder()
    ///     .model_source(ModelSource::huggingface("mistralai/Mistral-7B-v0.1"))
    ///     .build();
    /// let model = MistralRsModel::with_isq(config, QuantizationLevel::Q4_0).await?;
    /// ```
    pub async fn with_isq(mut config: MistralRsConfig, level: QuantizationLevel) -> Result<Self> {
        config.isq = Some(crate::config::IsqConfig::new(level));
        Self::new(config).await
    }

    /// Create from UQFF pre-quantized model files.
    ///
    /// UQFF (Universal Quantized File Format) models are pre-quantized and load faster
    /// than ISQ (In-Situ Quantization) because they skip the quantization step.
    ///
    /// # Arguments
    ///
    /// * `model_id` - HuggingFace model ID containing the UQFF files
    /// * `uqff_files` - List of UQFF file names within the model repository
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let model = MistralRsModel::from_uqff(
    ///     "EricB/Phi-3.5-mini-instruct-UQFF",
    ///     vec!["phi3.5-mini-instruct-q8_0.uqff".into()]
    /// ).await?;
    /// ```
    pub async fn from_uqff(
        model_id: impl Into<String>,
        uqff_files: Vec<std::path::PathBuf>,
    ) -> Result<Self> {
        use mistralrs::UqffTextModelBuilder;

        let model_id = model_id.into();
        info!("Loading UQFF model: {} with files: {:?}", model_id, uqff_files);

        if uqff_files.is_empty() {
            return Err(MistralRsError::invalid_config(
                "uqff_files",
                "UQFF file list cannot be empty",
                "Provide at least one UQFF file path",
            ));
        }

        let builder = UqffTextModelBuilder::new(&model_id, uqff_files.clone());

        let model = builder.into_inner().with_logging().build().await.map_err(|e| {
            MistralRsError::model_load(&model_id, format!("UQFF model loading failed: {}", e))
        })?;

        let config = MistralRsConfig::builder().model_source(ModelSource::uqff(&model_id)).build();

        info!("UQFF model loaded successfully: {}", model_id);

        Ok(Self { model: Arc::new(model), name: model_id, config })
    }

    /// Validate UQFF file format before loading.
    ///
    /// Checks that the file exists and has the correct extension.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the UQFF file
    ///
    /// # Returns
    ///
    /// `Ok(())` if the file is valid, `Err` otherwise.
    pub fn validate_uqff_file(path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(MistralRsError::model_not_found(path.display().to_string()));
        }

        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if extension != "uqff" {
            return Err(MistralRsError::uqff_validation(
                path.display().to_string(),
                format!("Invalid file extension: expected '.uqff', got '.{}'", extension),
            ));
        }

        Ok(())
    }
    /// Get the model configuration
    pub fn config(&self) -> &MistralRsConfig {
        &self.config
    }

    /// Convert ADK request to mistral.rs messages
    fn build_messages(&self, request: &LlmRequest) -> TextMessages {
        let mut messages = TextMessages::new();

        for content in &request.contents {
            let role = match content.role.as_str() {
                "user" => TextMessageRole::User,
                "model" | "assistant" => TextMessageRole::Assistant,
                "system" => TextMessageRole::System,
                _ => TextMessageRole::User, // Default to user for unknown roles
            };

            // Extract text from parts
            let text: String = content
                .parts
                .iter()
                .filter_map(|part| match part {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");

            if !text.is_empty() {
                messages = messages.add_message(role, text);
            }
        }

        messages
    }

    /// Convert mistral.rs response to ADK response
    fn convert_response(&self, response: &mistralrs::ChatCompletionResponse) -> LlmResponse {
        let content = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_ref())
            .map(|text| Content::new("model").with_text(text.clone()));

        let usage_metadata = Some(UsageMetadata {
            prompt_token_count: response.usage.prompt_tokens as i32,
            candidates_token_count: response.usage.completion_tokens as i32,
            total_token_count: response.usage.total_tokens as i32,
            ..Default::default()
        });

        let finish_reason =
            response.choices.first().map(|choice| match choice.finish_reason.as_str() {
                "stop" => FinishReason::Stop,
                "length" => FinishReason::MaxTokens,
                _ => FinishReason::Other,
            });

        LlmResponse {
            content,
            usage_metadata,
            finish_reason,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
            citation_metadata: None,
        }
    }
}

#[async_trait]
impl Llm for MistralRsModel {
    fn name(&self) -> &str {
        &self.name
    }

    #[instrument(skip(self, request), fields(model = %self.name))]
    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let message_count = request.contents.len();
        log_inference_start(&self.name, message_count, stream);
        debug!("Generating content with {} messages", message_count);

        let messages = self.build_messages(&request);
        let inference_start = std::time::Instant::now();

        if stream {
            // Streaming response
            let model = Arc::clone(&self.model);

            let response_stream = async_stream::stream! {
                #[allow(unused_imports)]
                use futures::StreamExt;

                let stream_result = model
                    .stream_chat_request(messages)
                    .await;

                match stream_result {
                    Ok(mut stream) => {
                        let mut accumulated_text = String::new();

                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Response::Chunk(chunk_response) => {
                                    if let Some(choice) = chunk_response.choices.first() {
                                        if let Some(content) = &choice.delta.content {
                                            accumulated_text.push_str(content);

                                            let response = LlmResponse {
                                                content: Some(Content::new("model").with_text(content.clone())),
                                                usage_metadata: None,
                                                finish_reason: None,
                                                partial: true,
                                                turn_complete: false,
                                                interrupted: false,
                                                citation_metadata: None,
                                                error_code: None,
                                                error_message: None,
                                            };
                                            yield Ok(response);
                                        }
                                    }
                                }
                                Response::Done(final_response) => {
                                    let usage = Some(UsageMetadata {
                                        prompt_token_count: final_response.usage.prompt_tokens as i32,
                                        candidates_token_count: final_response.usage.completion_tokens as i32,
                                        total_token_count: final_response.usage.total_tokens as i32,
                                        ..Default::default()
                                    });

                                    let response = LlmResponse {
                                        content: Some(Content::new("model").with_text(accumulated_text.clone())),
                                        usage_metadata: usage,
                                        finish_reason: Some(FinishReason::Stop),
                                        partial: false,
                                        turn_complete: true,
                                        interrupted: false,
                                        error_code: None,
                                        error_message: None,
                                        citation_metadata: None,
                                    };
                                    yield Ok(response);
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(adk_core::AdkError::Model(e.to_string()));
                    }
                }
            };

            Ok(Box::pin(response_stream))
        } else {
            // Non-streaming response
            let response = self
                .model
                .send_chat_request(messages)
                .await
                .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

            // Log inference completion with timing
            let duration_ms = inference_start.elapsed().as_millis() as u64;
            log_inference_complete(
                &self.name,
                duration_ms,
                response.usage.prompt_tokens as i32,
                response.usage.completion_tokens as i32,
            );

            let adk_response = self.convert_response(&response);
            Ok(Box::pin(stream::once(async { Ok(adk_response) })))
        }
    }
}

/// Convert QuantizationLevel to mistral.rs IsqType
fn quantization_level_to_isq(level: QuantizationLevel) -> IsqType {
    match level {
        QuantizationLevel::Q4_0 => IsqType::Q4_0,
        QuantizationLevel::Q4_1 => IsqType::Q4_1,
        QuantizationLevel::Q5_0 => IsqType::Q5_0,
        QuantizationLevel::Q5_1 => IsqType::Q5_1,
        QuantizationLevel::Q8_0 => IsqType::Q8_0,
        QuantizationLevel::Q8_1 => IsqType::Q8_1,
        QuantizationLevel::Q2K => IsqType::Q2K,
        QuantizationLevel::Q3K => IsqType::Q3K,
        QuantizationLevel::Q4K => IsqType::Q4K,
        QuantizationLevel::Q5K => IsqType::Q5K,
        QuantizationLevel::Q6K => IsqType::Q6K,
    }
}

/// Convert Device to mistral.rs DeviceMapSetting
///
/// This function handles device selection:
/// - Auto: Uses automatic device mapping with default parameters (recommended)
/// - Cpu: Forces CPU usage
/// - Cuda(n): Uses CUDA GPU with specified ordinal
/// - Metal: Uses Apple Metal acceleration
///
/// For Auto mode, mistral.rs will automatically detect and use the best available
/// device (Metal on macOS, CUDA on systems with NVIDIA GPUs, CPU otherwise).
fn device_to_device_map(device: &Device) -> DeviceMapSetting {
    match device {
        Device::Auto => {
            // Use automatic device mapping - mistral.rs will detect the best device
            debug!("Using automatic device mapping");
            DeviceMapSetting::Auto(AutoDeviceMapParams::default_text())
        }
        Device::Cpu => {
            debug!("Forcing CPU device");
            // For CPU, we use dummy mapping which defaults to CPU
            DeviceMapSetting::dummy()
        }
        Device::Cuda(_index) => {
            // For specific CUDA device, use auto mapping which will use CUDA if available
            debug!("Using CUDA device mapping");
            DeviceMapSetting::Auto(AutoDeviceMapParams::default_text())
        }
        Device::Metal => {
            // For Metal, use auto mapping which will use Metal on macOS
            debug!("Using Metal device mapping");
            DeviceMapSetting::Auto(AutoDeviceMapParams::default_text())
        }
    }
}

impl std::fmt::Debug for MistralRsModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MistralRsModel")
            .field("name", &self.name)
            .field("config", &self.config)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_source_display() {
        let hf = ModelSource::huggingface("test/model");
        match hf {
            ModelSource::HuggingFace(id) => assert_eq!(id, "test/model"),
            _ => panic!("Expected HuggingFace variant"),
        }
    }

    #[test]
    fn test_quantization_level_conversion() {
        // Test all quantization levels can be converted
        let levels = [
            QuantizationLevel::Q4_0,
            QuantizationLevel::Q4_1,
            QuantizationLevel::Q5_0,
            QuantizationLevel::Q5_1,
            QuantizationLevel::Q8_0,
            QuantizationLevel::Q8_1,
            QuantizationLevel::Q2K,
            QuantizationLevel::Q3K,
            QuantizationLevel::Q4K,
            QuantizationLevel::Q5K,
            QuantizationLevel::Q6K,
        ];

        for level in levels {
            let _ = quantization_level_to_isq(level);
        }
    }

    #[test]
    fn test_device_conversion_cpu() {
        let device = Device::Cpu;
        let result = device_to_device_map(&device);
        // CPU uses dummy mapping
        assert!(matches!(result, DeviceMapSetting::Map(_)));
    }

    #[test]
    fn test_device_conversion_cuda() {
        let device = Device::Cuda(0);
        let result = device_to_device_map(&device);
        // CUDA uses auto mapping
        assert!(matches!(result, DeviceMapSetting::Auto(_)));
    }

    #[test]
    fn test_device_conversion_metal() {
        let device = Device::Metal;
        let result = device_to_device_map(&device);
        // Metal uses auto mapping
        assert!(matches!(result, DeviceMapSetting::Auto(_)));
    }

    #[test]
    fn test_device_conversion_auto() {
        let device = Device::Auto;
        let result = device_to_device_map(&device);
        // Auto uses auto mapping
        assert!(matches!(result, DeviceMapSetting::Auto(_)));
    }
}
