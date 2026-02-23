//! Vision model support for multimodal inference.
//!
//! This module provides support for vision-language models that can process
//! both text and images, such as LLaVa, Qwen-VL, Gemma 3, Phi-3.5-vision, etc.
//!
//! ## Features
//!
//! - Load vision models from HuggingFace Hub
//! - Process images in JPEG, PNG, WebP formats
//! - Support base64-encoded images and URLs
//! - Multimodal generation with text + images + audio
//!
//! ## Example
//!
//! ```rust,ignore
//! use adk_mistralrs::{MistralRsVisionModel, MistralRsConfig, ModelSource};
//! use image::DynamicImage;
//!
//! let model = MistralRsVisionModel::from_hf("microsoft/Phi-3.5-vision-instruct").await?;
//!
//! // Generate with an image
//! let image = image::open("photo.jpg")?;
//! let response = model.generate_with_image(
//!     "What is in this image?",
//!     vec![image],
//! ).await?;
//! ```

use std::path::Path;
use std::sync::Arc;

use adk_core::{
    Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, UsageMetadata,
};
use async_trait::async_trait;
use futures::stream;
use image::DynamicImage;
use mistralrs::{
    AudioInput, AutoDeviceMapParams, DeviceMapSetting, IsqType, PagedAttentionMetaBuilder,
    Response, TextMessageRole, Topology, VisionMessages, VisionModelBuilder,
};
use tracing::{debug, info, instrument, warn};

use crate::config::{Device, MistralRsConfig, ModelArchitecture, ModelSource, QuantizationLevel};
use crate::convert::{AudioFormat, ImageFormat};
use crate::error::{MistralRsError, Result};

/// A mistral.rs vision model for multimodal inference.
///
/// This struct wraps a mistral.rs vision model and implements the ADK `Llm` trait,
/// providing additional methods for image and audio input handling.
///
/// # Supported Models
///
/// - LLaVa (llava-hf/llava-1.5-7b-hf)
/// - Phi-3.5-vision (microsoft/Phi-3.5-vision-instruct)
/// - Qwen-VL (Qwen/Qwen2-VL-7B-Instruct)
/// - Gemma 3 vision variants
/// - And other vision-language models supported by mistral.rs
///
/// # Example
///
/// ```rust,ignore
/// use adk_mistralrs::{MistralRsVisionModel, MistralRsConfig, ModelSource};
///
/// let model = MistralRsVisionModel::from_hf("microsoft/Phi-3.5-vision-instruct").await?;
/// ```
pub struct MistralRsVisionModel {
    /// The underlying mistral.rs model instance
    model: Arc<mistralrs::Model>,
    /// Model name for identification
    name: String,
    /// Configuration used to create this model
    config: MistralRsConfig,
}

impl MistralRsVisionModel {
    /// Create a new vision model from configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration specifying model source and options
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = MistralRsConfig::builder()
    ///     .model_source(ModelSource::huggingface("microsoft/Phi-3.5-vision-instruct"))
    ///     .architecture(ModelArchitecture::Vision)
    ///     .build();
    /// let model = MistralRsVisionModel::new(config).await?;
    /// ```
    #[instrument(skip(config), fields(model_source = ?config.model_source))]
    pub async fn new(config: MistralRsConfig) -> Result<Self> {
        let model_id = match &config.model_source {
            ModelSource::HuggingFace(id) => id.clone(),
            ModelSource::Local(path) => path.display().to_string(),
            ModelSource::Gguf(path) => path.display().to_string(),
            ModelSource::Uqff(path) => path.display().to_string(),
        };

        info!("Loading mistral.rs vision model: {}", model_id);

        let mut builder = VisionModelBuilder::new(model_id.clone());

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

        // Apply topology file if configured
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

        // Apply MatFormer configuration if present
        if let Some(matformer) = &config.matformer {
            // Apply config path if provided
            if let Some(config_path) = &matformer.config_path {
                builder = builder.with_matformer_config_path(config_path.clone());
                debug!("MatFormer config path: {:?}", config_path);
            }
            // Apply slice name
            builder = builder.with_matformer_slice_name(matformer.target_size.clone());
            debug!("MatFormer slice configured: {}", matformer.target_size);
        }

        // Enable logging
        builder = builder.with_logging();

        // Build the model
        let model = builder
            .build()
            .await
            .map_err(|e| MistralRsError::model_load(&model_id, e.to_string()))?;

        info!("Vision model loaded successfully: {}", model_id);

        Ok(Self { model: Arc::new(model), name: model_id, config })
    }

    /// Create from HuggingFace model ID with defaults.
    ///
    /// # Arguments
    ///
    /// * `model_id` - HuggingFace model ID (e.g., "microsoft/Phi-3.5-vision-instruct")
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let model = MistralRsVisionModel::from_hf("microsoft/Phi-3.5-vision-instruct").await?;
    /// ```
    pub async fn from_hf(model_id: &str) -> Result<Self> {
        let config = MistralRsConfig::builder()
            .model_source(ModelSource::huggingface(model_id))
            .architecture(ModelArchitecture::Vision)
            .build();
        Self::new(config).await
    }

    /// Create with ISQ quantization.
    ///
    /// # Arguments
    ///
    /// * `model_id` - HuggingFace model ID
    /// * `level` - Quantization level to apply
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let model = MistralRsVisionModel::from_hf_with_isq(
    ///     "microsoft/Phi-3.5-vision-instruct",
    ///     QuantizationLevel::Q4K
    /// ).await?;
    /// ```
    pub async fn from_hf_with_isq(model_id: &str, level: QuantizationLevel) -> Result<Self> {
        let config = MistralRsConfig::builder()
            .model_source(ModelSource::huggingface(model_id))
            .architecture(ModelArchitecture::Vision)
            .isq(level)
            .build();
        Self::new(config).await
    }

    /// Get the model configuration.
    pub fn config(&self) -> &MistralRsConfig {
        &self.config
    }

    /// Get a reference to the underlying mistral.rs model.
    ///
    /// This is useful for advanced operations that require direct access
    /// to the mistral.rs Model API.
    pub fn inner(&self) -> &mistralrs::Model {
        &self.model
    }

    /// Generate a response with image input.
    ///
    /// # Arguments
    ///
    /// * `prompt` - Text prompt describing what to do with the image
    /// * `images` - Vector of images to process
    ///
    /// # Returns
    ///
    /// The model's text response describing or analyzing the images.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let image = image::open("photo.jpg")?;
    /// let response = model.generate_with_image(
    ///     "Describe this image in detail.",
    ///     vec![image],
    /// ).await?;
    /// println!("{}", response);
    /// ```
    pub async fn generate_with_image(
        &self,
        prompt: &str,
        images: Vec<DynamicImage>,
    ) -> Result<String> {
        let messages = VisionMessages::new()
            .add_image_message(TextMessageRole::User, prompt, images, &self.model)
            .map_err(|e| MistralRsError::image_processing(e.to_string()))?;

        let response = self
            .model
            .send_chat_request(messages)
            .await
            .map_err(|e| MistralRsError::inference(&self.name, e.to_string()))?;

        response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .ok_or_else(|| MistralRsError::inference(&self.name, "No response content"))
    }

    /// Generate a response with multimodal input (text + images + audio).
    ///
    /// # Arguments
    ///
    /// * `prompt` - Text prompt
    /// * `images` - Vector of images (can be empty)
    /// * `audios` - Vector of audio inputs (can be empty)
    ///
    /// # Returns
    ///
    /// The model's text response.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let image = image::open("photo.jpg")?;
    /// let audio = AudioInput::from_bytes(&audio_bytes)?;
    /// let response = model.generate_with_multimodal(
    ///     "Describe what you see and hear.",
    ///     vec![image],
    ///     vec![audio],
    /// ).await?;
    /// ```
    pub async fn generate_with_multimodal(
        &self,
        prompt: &str,
        images: Vec<DynamicImage>,
        audios: Vec<AudioInput>,
    ) -> Result<String> {
        let messages = VisionMessages::new()
            .add_multimodal_message(TextMessageRole::User, prompt, images, audios, &self.model)
            .map_err(|e| MistralRsError::image_processing(e.to_string()))?;

        let response = self
            .model
            .send_chat_request(messages)
            .await
            .map_err(|e| MistralRsError::inference(&self.name, e.to_string()))?;

        response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .ok_or_else(|| MistralRsError::inference(&self.name, "No response content"))
    }

    /// Build VisionMessages from an ADK LlmRequest.
    ///
    /// This extracts text, images, and audio from the request parts.
    fn build_vision_messages(&self, request: &LlmRequest) -> Result<VisionMessages> {
        let mut messages = VisionMessages::new();

        for content in &request.contents {
            let role = match content.role.as_str() {
                "user" => TextMessageRole::User,
                "model" | "assistant" => TextMessageRole::Assistant,
                "system" => TextMessageRole::System,
                _ => TextMessageRole::User,
            };

            // Collect text parts
            let text: String = content
                .parts
                .iter()
                .filter_map(|part| match part {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");

            // Collect image parts
            let images: Vec<DynamicImage> = content
                .parts
                .iter()
                .filter_map(|part| match part {
                    Part::InlineData { mime_type, data } => {
                        if is_image_mime_type(mime_type) {
                            image_from_bytes(data).ok()
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .collect();

            // Collect audio parts
            let audios: Vec<AudioInput> = content
                .parts
                .iter()
                .filter_map(|part| match part {
                    Part::InlineData { mime_type, data } => {
                        if is_audio_mime_type(mime_type) {
                            audio_from_bytes(data).ok()
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .collect();

            // Add message based on content type
            if !images.is_empty() || !audios.is_empty() {
                messages = messages
                    .add_multimodal_message(role, &text, images, audios, &self.model)
                    .map_err(|e| MistralRsError::image_processing(e.to_string()))?;
            } else if !text.is_empty() {
                messages = messages.add_message(role, text);
            }
        }

        Ok(messages)
    }

    /// Convert mistral.rs response to ADK response.
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
impl Llm for MistralRsVisionModel {
    fn name(&self) -> &str {
        &self.name
    }

    #[instrument(skip(self, request), fields(model = %self.name))]
    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        debug!("Generating vision content with {} messages", request.contents.len());

        let messages = self
            .build_vision_messages(&request)
            .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

        if stream {
            let model = Arc::clone(&self.model);

            let response_stream = async_stream::stream! {
                #[allow(unused_imports)]
                use futures::StreamExt;

                let stream_result = model.stream_chat_request(messages).await;

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
            let response = self
                .model
                .send_chat_request(messages)
                .await
                .map_err(|e| adk_core::AdkError::Model(e.to_string()))?;

            let adk_response = self.convert_response(&response);
            Ok(Box::pin(stream::once(async { Ok(adk_response) })))
        }
    }
}

/// Convert QuantizationLevel to mistral.rs IsqType.
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

/// Convert Device to mistral.rs DeviceMapSetting.
fn device_to_device_map(device: &Device) -> DeviceMapSetting {
    match device {
        Device::Auto => DeviceMapSetting::Auto(AutoDeviceMapParams::default_vision()),
        Device::Cpu => DeviceMapSetting::dummy(),
        Device::Cuda(_) => DeviceMapSetting::Auto(AutoDeviceMapParams::default_vision()),
        Device::Metal => DeviceMapSetting::Auto(AutoDeviceMapParams::default_vision()),
    }
}

/// Check if a MIME type is an image type.
fn is_image_mime_type(mime_type: &str) -> bool {
    ImageFormat::is_supported_mime_type(mime_type)
}

/// Check if a MIME type is an audio type.
fn is_audio_mime_type(mime_type: &str) -> bool {
    AudioFormat::is_supported_mime_type(mime_type)
}

/// Decode a base64-encoded image.
pub fn image_from_base64(data: &str) -> Result<DynamicImage> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| MistralRsError::image_processing(format!("Invalid base64: {}", e)))?;

    image::load_from_memory(&bytes)
        .map_err(|e| MistralRsError::image_processing(format!("Failed to decode image: {}", e)))
}

/// Load an image from raw bytes.
pub fn image_from_bytes(data: &[u8]) -> Result<DynamicImage> {
    image::load_from_memory(data)
        .map_err(|e| MistralRsError::image_processing(format!("Failed to decode image: {}", e)))
}

/// Decode a base64-encoded audio.
pub fn audio_from_base64(data: &str) -> Result<AudioInput> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| MistralRsError::audio_processing(format!("Invalid base64: {}", e)))?;

    AudioInput::from_bytes(&bytes)
        .map_err(|e| MistralRsError::audio_processing(format!("Failed to decode audio: {}", e)))
}

/// Load audio from raw bytes.
pub fn audio_from_bytes(data: &[u8]) -> Result<AudioInput> {
    AudioInput::from_bytes(data)
        .map_err(|e| MistralRsError::audio_processing(format!("Failed to decode audio: {}", e)))
}

/// Load an image from a file path.
pub fn image_from_path(path: impl AsRef<Path>) -> Result<DynamicImage> {
    let path = path.as_ref();
    image::open(path).map_err(|e| {
        MistralRsError::image_processing(format!(
            "Failed to load image from '{}': {}",
            path.display(),
            e
        ))
    })
}

/// Load an image from a URL (blocking).
#[cfg(feature = "reqwest")]
pub fn image_from_url(url: &str) -> Result<DynamicImage> {
    let bytes = reqwest::blocking::get(url)
        .map_err(|e| MistralRsError::image_processing(format!("Failed to fetch URL: {}", e)))?
        .bytes()
        .map_err(|e| MistralRsError::image_processing(format!("Failed to read response: {}", e)))?;

    image::load_from_memory(&bytes)
        .map_err(|e| MistralRsError::image_processing(format!("Failed to decode image: {}", e)))
}

/// Load an image from a URL (async).
#[cfg(feature = "reqwest")]
pub async fn image_from_url_async(url: &str) -> Result<DynamicImage> {
    let bytes = reqwest::get(url)
        .await
        .map_err(|e| MistralRsError::image_processing(format!("Failed to fetch URL: {}", e)))?
        .bytes()
        .await
        .map_err(|e| MistralRsError::image_processing(format!("Failed to read response: {}", e)))?;

    image::load_from_memory(&bytes)
        .map_err(|e| MistralRsError::image_processing(format!("Failed to decode image: {}", e)))
}

/// Load audio from a file path.
pub fn audio_from_path(path: impl AsRef<Path>) -> Result<AudioInput> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| {
        MistralRsError::audio_processing(format!(
            "Failed to read audio file '{}': {}",
            path.display(),
            e
        ))
    })?;

    AudioInput::from_bytes(&bytes).map_err(|e| {
        MistralRsError::audio_processing(format!(
            "Failed to decode audio from '{}': {}",
            path.display(),
            e
        ))
    })
}

impl std::fmt::Debug for MistralRsVisionModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MistralRsVisionModel")
            .field("name", &self.name)
            .field("config", &self.config)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_image_mime_type() {
        assert!(is_image_mime_type("image/jpeg"));
        assert!(is_image_mime_type("image/jpg"));
        assert!(is_image_mime_type("image/png"));
        assert!(is_image_mime_type("image/webp"));
        assert!(is_image_mime_type("image/gif"));
        assert!(is_image_mime_type("IMAGE/JPEG")); // Case insensitive
        assert!(!is_image_mime_type("audio/wav"));
        assert!(!is_image_mime_type("text/plain"));
    }

    #[test]
    fn test_is_audio_mime_type() {
        assert!(is_audio_mime_type("audio/wav"));
        assert!(is_audio_mime_type("audio/wave"));
        assert!(is_audio_mime_type("audio/x-wav"));
        assert!(is_audio_mime_type("audio/mp3"));
        assert!(is_audio_mime_type("audio/mpeg"));
        assert!(is_audio_mime_type("audio/flac"));
        assert!(is_audio_mime_type("audio/ogg"));
        assert!(is_audio_mime_type("AUDIO/WAV")); // Case insensitive
        assert!(!is_audio_mime_type("image/jpeg"));
        assert!(!is_audio_mime_type("text/plain"));
    }

    #[test]
    fn test_image_from_base64_invalid() {
        let result = image_from_base64("not-valid-base64!!!");
        assert!(result.is_err());
    }

    #[test]
    fn test_audio_from_base64_invalid() {
        let result = audio_from_base64("not-valid-base64!!!");
        assert!(result.is_err());
    }
}
