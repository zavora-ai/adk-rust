//! Adapter model support for LoRA and X-LoRA fine-tuned models.
//!
//! This module provides support for loading and using LoRA (Low-Rank Adaptation)
//! and X-LoRA (eXtended LoRA) adapters with mistral.rs models.
//!
//! ## LoRA Adapters
//!
//! LoRA adapters allow efficient fine-tuning by adding small trainable matrices
//! to the model's attention layers. This module supports:
//!
//! - Single adapter loading
//! - Multi-adapter loading (multiple LoRA adapters)
//! - Runtime adapter selection via request parameters
//!
//! ## X-LoRA Adapters
//!
//! X-LoRA extends LoRA with dynamic adapter mixing, allowing the model to
//! automatically blend multiple adapters based on the input context.
//!
//! ## Example
//!
//! ```rust,ignore
//! use adk_mistralrs::{MistralRsAdapterModel, AdapterConfig, MistralRsConfig, ModelSource};
//!
//! // Load a model with LoRA adapter
//! let config = MistralRsConfig::builder()
//!     .model_source(ModelSource::huggingface("meta-llama/Llama-2-7b-hf"))
//!     .adapter(AdapterConfig::lora("username/my-lora-adapter"))
//!     .build();
//!
//! let model = MistralRsAdapterModel::new(config).await?;
//!
//! // List available adapters
//! let adapters = model.available_adapters();
//!
//! // Swap to a different adapter at runtime
//! model.swap_adapter("another-adapter").await?;
//! ```

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use adk_core::{
    Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, UsageMetadata,
};
use async_trait::async_trait;
use futures::stream;
use mistralrs::Ordering;
use mistralrs::{
    AutoDeviceMapParams, DeviceMapSetting, IsqType, LoraModelBuilder, PagedAttentionMetaBuilder,
    RequestBuilder, Response, TextMessageRole, TextMessages, TextModelBuilder, Topology,
    XLoraModelBuilder,
};
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

use crate::config::{
    AdapterConfig, AdapterType, Device, MistralRsConfig, ModelSource, QuantizationLevel,
};
use crate::error::{MistralRsError, Result};

/// A mistral.rs model with LoRA or X-LoRA adapter support.
///
/// This struct wraps a mistral.rs model with adapter capabilities,
/// implementing the ADK `Llm` trait while providing additional
/// adapter management functionality.
///
/// # Features
///
/// - Load models with LoRA or X-LoRA adapters
/// - Hot-swap adapters at runtime
/// - Query available adapters
/// - Select adapters per-request
///
/// # Example
///
/// ```rust,ignore
/// use adk_mistralrs::{MistralRsAdapterModel, AdapterConfig, MistralRsConfig, ModelSource};
///
/// let config = MistralRsConfig::builder()
///     .model_source(ModelSource::huggingface("meta-llama/Llama-2-7b-hf"))
///     .adapter(AdapterConfig::lora("username/my-lora-adapter"))
///     .build();
///
/// let model = MistralRsAdapterModel::new(config).await?;
/// ```
pub struct MistralRsAdapterModel {
    /// The underlying mistral.rs model instance
    model: Arc<mistralrs::Model>,
    /// Model name for identification
    name: String,
    /// Configuration used to create this model
    config: MistralRsConfig,
    /// Currently active adapter name
    active_adapter: RwLock<Option<String>>,
    /// Set of available adapter names
    available_adapters: HashSet<String>,
}

impl MistralRsAdapterModel {
    /// Create a new adapter model from configuration.
    ///
    /// The configuration must include an adapter configuration. If no adapter
    /// is specified, use `MistralRsModel` instead.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration with adapter settings
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No adapter configuration is provided
    /// - The adapter files cannot be found
    /// - The model fails to load
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = MistralRsConfig::builder()
    ///     .model_source(ModelSource::huggingface("meta-llama/Llama-2-7b-hf"))
    ///     .adapter(AdapterConfig::lora("username/my-lora-adapter"))
    ///     .build();
    ///
    /// let model = MistralRsAdapterModel::new(config).await?;
    /// ```
    #[instrument(skip(config), fields(model_source = ?config.model_source))]
    pub async fn new(config: MistralRsConfig) -> Result<Self> {
        let adapter_config = config.adapter.as_ref().ok_or_else(|| {
            MistralRsError::invalid_config(
                "adapter",
                "Adapter configuration is required for MistralRsAdapterModel",
                "Use AdapterConfig::lora() or AdapterConfig::xlora() to configure an adapter",
            )
        })?;

        let model_id = match &config.model_source {
            ModelSource::HuggingFace(id) => id.clone(),
            ModelSource::Local(path) => path.display().to_string(),
            ModelSource::Gguf(path) => path.display().to_string(),
            ModelSource::Uqff(path) => path.display().to_string(),
        };

        info!(
            "Loading mistral.rs adapter model: {} with {} adapter",
            model_id, adapter_config.adapter_type
        );

        // Build the base text model builder
        let mut text_builder = TextModelBuilder::new(model_id.clone());

        // Apply ISQ quantization if configured
        if let Some(isq) = &config.isq {
            let isq_type = quantization_level_to_isq(isq.level);
            text_builder = text_builder.with_isq(isq_type);
            debug!("ISQ quantization enabled: {:?}", isq.level);
        }

        // Apply device selection
        let device_map = device_to_device_map(&config.device.device);
        text_builder = text_builder.with_device_mapping(device_map);
        debug!("Device configured: {:?}", config.device.device);

        // Apply PagedAttention if configured
        if config.paged_attention {
            text_builder = text_builder
                .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())
                .map_err(|e| {
                    MistralRsError::model_load(
                        &model_id,
                        format!("PagedAttention initialization failed: {e}"),
                    )
                })?;
            debug!("PagedAttention enabled");
        }

        // Apply topology file if configured
        if let Some(topology_path) = &config.topology_path {
            if topology_path.exists() {
                match Topology::from_path(topology_path) {
                    Ok(topology) => {
                        text_builder = text_builder.with_topology(topology);
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
            text_builder = text_builder.with_max_num_seqs(num_ctx);
            debug!("Context length configured: {}", num_ctx);
        }

        // Apply chat template if configured
        if let Some(chat_template) = &config.chat_template {
            text_builder = text_builder.with_chat_template(chat_template.clone());
            debug!("Custom chat template configured");
        }

        // Apply tokenizer path if configured
        if let Some(tokenizer_path) = &config.tokenizer_path {
            text_builder =
                text_builder.with_tokenizer_json(tokenizer_path.to_string_lossy().to_string());
            debug!("Custom tokenizer path: {:?}", tokenizer_path);
        }

        // Enable logging
        text_builder = text_builder.with_logging();

        // Build the model based on adapter type
        let (model, available_adapters) = match adapter_config.adapter_type {
            AdapterType::LoRA => {
                let adapter_ids = adapter_config.all_adapter_ids();
                debug!("Loading LoRA adapters: {:?}", adapter_ids);

                let lora_builder =
                    LoraModelBuilder::from_text_model_builder(text_builder, adapter_ids.clone());

                let model = lora_builder.build().await.map_err(|e| {
                    MistralRsError::adapter_load(
                        adapter_ids.join(", "),
                        format!("LoRA adapter loading failed: {}", e),
                    )
                })?;

                let adapters: HashSet<String> = adapter_ids.into_iter().collect();
                (model, adapters)
            }
            AdapterType::XLoRA => {
                let ordering_path = adapter_config.ordering.as_ref().ok_or_else(|| {
                    MistralRsError::invalid_config(
                        "ordering",
                        "X-LoRA requires an ordering file",
                        "Provide an ordering JSON file path with AdapterConfig::xlora()",
                    )
                })?;

                if !ordering_path.exists() {
                    return Err(MistralRsError::invalid_config(
                        "ordering",
                        format!("X-LoRA ordering file not found: {}", ordering_path.display()),
                        "Verify the ordering file path is correct",
                    ));
                }

                // Parse the ordering file to get adapter information
                let ordering = load_ordering_file(ordering_path)?;
                let adapter_names: HashSet<String> =
                    ordering.adapters.clone().unwrap_or_default().into_iter().collect();

                debug!("Loading X-LoRA model with ordering from: {:?}", ordering_path);

                let mut xlora_builder = XLoraModelBuilder::from_text_model_builder(
                    text_builder,
                    &adapter_config.adapter_source,
                    ordering,
                );

                if let Some(tgt_idx) = adapter_config.tgt_non_granular_index {
                    xlora_builder = xlora_builder.tgt_non_granular_index(tgt_idx);
                }

                let model = xlora_builder.build().await.map_err(|e| {
                    MistralRsError::adapter_load(
                        &adapter_config.adapter_source,
                        format!("X-LoRA model loading failed: {}", e),
                    )
                })?;

                (model, adapter_names)
            }
        };

        info!(
            "Adapter model loaded successfully: {} with {} adapter(s)",
            model_id,
            available_adapters.len()
        );

        let active_adapter = adapter_config.all_adapter_ids().first().cloned();

        Ok(Self {
            model: Arc::new(model),
            name: model_id,
            config,
            active_adapter: RwLock::new(active_adapter),
            available_adapters,
        })
    }

    /// Create a LoRA adapter model from HuggingFace model and adapter IDs.
    ///
    /// # Arguments
    ///
    /// * `model_id` - HuggingFace base model ID
    /// * `adapter_id` - HuggingFace LoRA adapter ID
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let model = MistralRsAdapterModel::from_hf_lora(
    ///     "meta-llama/Llama-2-7b-hf",
    ///     "username/my-lora-adapter"
    /// ).await?;
    /// ```
    pub async fn from_hf_lora(model_id: &str, adapter_id: &str) -> Result<Self> {
        let config = MistralRsConfig::builder()
            .model_source(ModelSource::huggingface(model_id))
            .adapter(AdapterConfig::lora(adapter_id))
            .build();
        Self::new(config).await
    }

    /// Create a multi-adapter LoRA model from HuggingFace.
    ///
    /// # Arguments
    ///
    /// * `model_id` - HuggingFace base model ID
    /// * `adapter_ids` - List of HuggingFace LoRA adapter IDs
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let model = MistralRsAdapterModel::from_hf_lora_multi(
    ///     "meta-llama/Llama-2-7b-hf",
    ///     vec!["adapter1", "adapter2"]
    /// ).await?;
    /// ```
    pub async fn from_hf_lora_multi(
        model_id: &str,
        adapter_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self> {
        let config = MistralRsConfig::builder()
            .model_source(ModelSource::huggingface(model_id))
            .adapter(AdapterConfig::lora_multi(adapter_ids))
            .build();
        Self::new(config).await
    }

    /// Create an X-LoRA adapter model from HuggingFace.
    ///
    /// # Arguments
    ///
    /// * `model_id` - HuggingFace base model ID
    /// * `xlora_model_id` - HuggingFace X-LoRA model ID
    /// * `ordering_path` - Path to the ordering JSON file
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let model = MistralRsAdapterModel::from_hf_xlora(
    ///     "meta-llama/Llama-2-7b-hf",
    ///     "username/my-xlora-model",
    ///     "ordering.json"
    /// ).await?;
    /// ```
    pub async fn from_hf_xlora(
        model_id: &str,
        xlora_model_id: &str,
        ordering_path: impl AsRef<Path>,
    ) -> Result<Self> {
        let config = MistralRsConfig::builder()
            .model_source(ModelSource::huggingface(model_id))
            .adapter(AdapterConfig::xlora(xlora_model_id, ordering_path.as_ref().to_path_buf()))
            .build();
        Self::new(config).await
    }

    /// Get the list of available adapter names.
    ///
    /// # Returns
    ///
    /// A vector of adapter names that can be used with `swap_adapter()`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let adapters = model.available_adapters();
    /// println!("Available adapters: {:?}", adapters);
    /// ```
    pub fn available_adapters(&self) -> Vec<String> {
        self.available_adapters.iter().cloned().collect()
    }

    /// Get the currently active adapter name.
    ///
    /// # Returns
    ///
    /// The name of the currently active adapter, or `None` if no adapter is active.
    pub async fn active_adapter(&self) -> Option<String> {
        self.active_adapter.read().await.clone()
    }

    /// Swap to a different adapter at runtime.
    ///
    /// This allows changing which adapter is used for subsequent requests
    /// without reloading the model.
    ///
    /// # Arguments
    ///
    /// * `adapter_name` - Name of the adapter to activate
    ///
    /// # Errors
    ///
    /// Returns an error if the adapter name is not in the list of available adapters.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// model.swap_adapter("my-other-adapter").await?;
    /// ```
    pub async fn swap_adapter(&self, adapter_name: &str) -> Result<()> {
        if !self.available_adapters.contains(adapter_name) {
            return Err(MistralRsError::adapter_not_found(adapter_name, self.available_adapters()));
        }

        let mut active = self.active_adapter.write().await;
        *active = Some(adapter_name.to_string());
        debug!("Swapped to adapter: {}", adapter_name);
        Ok(())
    }

    /// Check if a specific adapter is available.
    ///
    /// # Arguments
    ///
    /// * `adapter_name` - Name of the adapter to check
    ///
    /// # Returns
    ///
    /// `true` if the adapter is available, `false` otherwise.
    pub fn has_adapter(&self, adapter_name: &str) -> bool {
        self.available_adapters.contains(adapter_name)
    }

    /// Get the model configuration.
    pub fn config(&self) -> &MistralRsConfig {
        &self.config
    }

    /// Get the adapter configuration.
    pub fn adapter_config(&self) -> Option<&AdapterConfig> {
        self.config.adapter.as_ref()
    }

    /// Check if this is an X-LoRA model.
    pub fn is_xlora(&self) -> bool {
        self.config.adapter.as_ref().map(|a| a.adapter_type == AdapterType::XLoRA).unwrap_or(false)
    }

    /// Convert ADK request to mistral.rs messages with adapter selection
    fn build_messages(&self, request: &LlmRequest) -> TextMessages {
        let mut messages = TextMessages::new();

        for content in &request.contents {
            let role = match content.role.as_str() {
                "user" => TextMessageRole::User,
                "model" | "assistant" => TextMessageRole::Assistant,
                "system" => TextMessageRole::System,
                _ => TextMessageRole::User,
            };

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
impl Llm for MistralRsAdapterModel {
    fn name(&self) -> &str {
        &self.name
    }

    #[instrument(skip(self, request), fields(model = %self.name))]
    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        debug!("Generating content with {} messages", request.contents.len());

        let messages = self.build_messages(&request);

        // Get the active adapter for this request
        let active_adapter = self.active_adapter.read().await.clone();

        if stream {
            let model = Arc::clone(&self.model);
            let adapter_for_stream = active_adapter.clone();

            let response_stream = async_stream::stream! {
                #[allow(unused_imports)]
                use futures::StreamExt;

                // Build request with adapter selection if available
                let request = if let Some(adapter) = adapter_for_stream {
                    RequestBuilder::from(messages)
                        .set_adapters(vec![adapter])
                } else {
                    RequestBuilder::from(messages)
                };

                let stream_result = model.stream_chat_request(request).await;

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
            // Build request with adapter selection if available
            let request = if let Some(adapter) = active_adapter {
                RequestBuilder::from(messages).set_adapters(vec![adapter])
            } else {
                RequestBuilder::from(messages)
            };

            let response = self
                .model
                .send_chat_request(request)
                .await
                .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

            let adk_response = self.convert_response(&response);
            Ok(Box::pin(stream::once(async { Ok(adk_response) })))
        }
    }
}

/// Load and parse an X-LoRA ordering file.
fn load_ordering_file(path: &Path) -> Result<Ordering> {
    let file = std::fs::File::open(path).map_err(|e| {
        MistralRsError::invalid_config(
            "ordering",
            format!("Failed to open ordering file '{}': {}", path.display(), e),
            "Verify the file exists and has read permissions",
        )
    })?;

    serde_json::from_reader(file).map_err(|e| {
        MistralRsError::invalid_config(
            "ordering",
            format!("Failed to parse ordering file '{}': {}", path.display(), e),
            "Verify the JSON format is correct. See mistral.rs documentation for ordering file schema.",
        )
    })
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
fn device_to_device_map(device: &Device) -> DeviceMapSetting {
    match device {
        Device::Auto => DeviceMapSetting::Auto(AutoDeviceMapParams::default_text()),
        Device::Cpu => DeviceMapSetting::dummy(),
        Device::Cuda(_) => DeviceMapSetting::Auto(AutoDeviceMapParams::default_text()),
        Device::Metal => DeviceMapSetting::Auto(AutoDeviceMapParams::default_text()),
    }
}

impl std::fmt::Debug for MistralRsAdapterModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MistralRsAdapterModel")
            .field("name", &self.name)
            .field("config", &self.config)
            .field("available_adapters", &self.available_adapters)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_config_lora() {
        let config = AdapterConfig::lora("test/adapter");
        assert_eq!(config.adapter_type, AdapterType::LoRA);
        assert_eq!(config.adapter_source, "test/adapter");
        assert!(!config.is_multi_adapter());
    }

    #[test]
    fn test_adapter_config_multi_lora() {
        let config = AdapterConfig::lora_multi(vec!["adapter1", "adapter2", "adapter3"]);
        assert_eq!(config.adapter_type, AdapterType::LoRA);
        assert_eq!(config.adapter_source, "adapter1");
        assert!(config.is_multi_adapter());
        assert_eq!(config.all_adapter_ids(), vec!["adapter1", "adapter2", "adapter3"]);
    }

    #[test]
    fn test_adapter_config_xlora() {
        let config = AdapterConfig::xlora("xlora/model", std::path::PathBuf::from("order.json"));
        assert_eq!(config.adapter_type, AdapterType::XLoRA);
        assert!(config.ordering.is_some());
    }

    #[test]
    fn test_adapter_type_display() {
        assert_eq!(format!("{}", AdapterType::LoRA), "LoRA");
        assert_eq!(format!("{}", AdapterType::XLoRA), "X-LoRA");
    }
}
