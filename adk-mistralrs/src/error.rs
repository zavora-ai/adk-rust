//! Error types for adk-mistralrs.
//!
//! This module provides comprehensive error types for the mistral.rs integration.
//! All errors include contextual information and actionable guidance to help
//! users diagnose and resolve issues quickly.
//!
//! # Error Categories
//!
//! - **Model Loading**: Errors during model initialization and loading
//! - **Inference**: Errors during text generation and inference
//! - **Configuration**: Invalid or incompatible configuration settings
//! - **Media Processing**: Image and audio processing errors
//! - **Adapters**: LoRA/X-LoRA adapter loading and swapping errors
//! - **External Services**: MCP client and external service errors
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_mistralrs::{MistralRsModel, MistralRsError};
//!
//! match MistralRsModel::from_hf("invalid/model").await {
//!     Ok(model) => println!("Model loaded"),
//!     Err(MistralRsError::ModelNotFound { path, suggestion }) => {
//!         eprintln!("Model not found: {}", path);
//!         eprintln!("Suggestion: {}", suggestion);
//!     }
//!     Err(e) => eprintln!("Error: {}", e),
//! }
//! ```

use thiserror::Error;

/// Errors that can occur when using mistral.rs models.
///
/// Each error variant includes contextual information and, where applicable,
/// actionable suggestions for resolution.
#[derive(Debug, Error)]
pub enum MistralRsError {
    /// Model loading failed with detailed context.
    ///
    /// This error occurs when the model cannot be loaded from the specified source.
    /// Common causes include:
    /// - Invalid model ID or path
    /// - Network issues when downloading from HuggingFace
    /// - Insufficient memory for model weights
    /// - Incompatible model format
    #[error("Model loading failed for '{model_id}': {reason}. {suggestion}")]
    ModelLoad {
        /// The model ID or path that failed to load
        model_id: String,
        /// The specific reason for the failure
        reason: String,
        /// Actionable suggestion for resolution
        suggestion: String,
    },

    /// Model file or directory not found.
    ///
    /// This error occurs when the specified model path does not exist.
    #[error("Model not found at path '{path}'. {suggestion}")]
    ModelNotFound {
        /// The path that was not found
        path: String,
        /// Suggestion for resolution
        suggestion: String,
    },

    /// Unsupported model architecture.
    ///
    /// This error occurs when attempting to load a model with an architecture
    /// that is not supported by the current configuration.
    #[error(
        "Unsupported architecture '{architecture}' for model '{model_id}'. Supported architectures: {supported}"
    )]
    UnsupportedArchitecture {
        /// The unsupported architecture
        architecture: String,
        /// The model ID
        model_id: String,
        /// List of supported architectures
        supported: String,
    },

    /// Requested device is not available.
    ///
    /// This error occurs when the specified compute device (CPU, CUDA, Metal)
    /// is not available on the system.
    #[error("Device '{device}' is not available. {suggestion}")]
    DeviceNotAvailable {
        /// The requested device
        device: String,
        /// Suggestion for resolution
        suggestion: String,
    },

    /// Out of memory during model loading or inference.
    ///
    /// This error occurs when there is insufficient memory (RAM or VRAM)
    /// to load the model or perform inference.
    #[error("Out of memory while {operation}: {details}. {suggestion}")]
    OutOfMemory {
        /// The operation that ran out of memory
        operation: String,
        /// Details about the memory issue
        details: String,
        /// Actionable suggestion for resolution
        suggestion: String,
    },

    /// Inference failed during text generation.
    ///
    /// This error occurs when the model fails to generate a response.
    #[error("Inference failed for model '{model_id}': {reason}")]
    Inference {
        /// The model that failed
        model_id: String,
        /// The reason for the failure
        reason: String,
    },

    /// Invalid configuration provided.
    ///
    /// This error occurs when the configuration contains invalid or
    /// incompatible settings.
    #[error("Invalid configuration: {field} - {reason}. {suggestion}")]
    InvalidConfig {
        /// The configuration field that is invalid
        field: String,
        /// The reason it is invalid
        reason: String,
        /// Suggestion for resolution
        suggestion: String,
    },

    /// Image processing failed.
    ///
    /// This error occurs when processing image input for vision models.
    #[error("Image processing failed: {reason}. Supported formats: JPEG, PNG, WebP, GIF")]
    ImageProcessing {
        /// The reason for the failure
        reason: String,
    },

    /// Audio processing failed.
    ///
    /// This error occurs when processing audio input for multimodal models.
    #[error("Audio processing failed: {reason}. Supported formats: WAV, MP3, FLAC, OGG")]
    AudioProcessing {
        /// The reason for the failure
        reason: String,
    },

    /// Tool conversion failed.
    ///
    /// This error occurs when converting ADK tool declarations to mistral.rs format.
    #[error("Tool conversion failed for '{tool_name}': {reason}")]
    ToolConversion {
        /// The tool that failed to convert
        tool_name: String,
        /// The reason for the failure
        reason: String,
    },

    /// Chat template error.
    ///
    /// This error occurs when applying or parsing chat templates.
    #[error("Chat template error: {reason}. {suggestion}")]
    ChatTemplate {
        /// The reason for the failure
        reason: String,
        /// Suggestion for resolution
        suggestion: String,
    },

    /// Adapter loading or swapping failed.
    ///
    /// This error occurs when loading LoRA/X-LoRA adapters or swapping
    /// between adapters at runtime.
    #[error("Adapter '{adapter_name}' failed: {reason}. {suggestion}")]
    AdapterLoad {
        /// The adapter that failed
        adapter_name: String,
        /// The reason for the failure
        reason: String,
        /// Suggestion for resolution
        suggestion: String,
    },

    /// Adapter not found in available adapters.
    ///
    /// This error occurs when attempting to swap to an adapter that
    /// is not loaded.
    #[error("Adapter '{name}' not found. Available adapters: {available:?}")]
    AdapterNotFound {
        /// The adapter name that was not found
        name: String,
        /// List of available adapter names
        available: Vec<String>,
    },

    /// MCP client error.
    ///
    /// This error occurs when connecting to or communicating with MCP servers.
    #[error("MCP client error for server '{server}': {reason}")]
    McpClient {
        /// The MCP server that had the error
        server: String,
        /// The reason for the failure
        reason: String,
    },

    /// Embedding generation failed.
    ///
    /// This error occurs when generating embeddings fails.
    #[error("Embedding generation failed: {reason}")]
    Embedding {
        /// The reason for the failure
        reason: String,
    },

    /// Speech generation failed.
    ///
    /// This error occurs when generating speech audio fails.
    #[error("Speech generation failed: {reason}")]
    Speech {
        /// The reason for the failure
        reason: String,
    },

    /// Diffusion/image generation failed.
    ///
    /// This error occurs when generating images with diffusion models fails.
    #[error("Image generation failed: {reason}")]
    Diffusion {
        /// The reason for the failure
        reason: String,
    },

    /// Multi-model routing error.
    ///
    /// This error occurs when routing requests in multi-model configurations.
    #[error(
        "Multi-model routing error: model '{model_name}' not found. Available models: {available:?}"
    )]
    MultiModelRouting {
        /// The model name that was not found
        model_name: String,
        /// List of available model names
        available: Vec<String>,
    },

    /// UQFF file validation failed.
    ///
    /// This error occurs when validating UQFF pre-quantized model files.
    #[error("UQFF validation failed for '{path}': {reason}")]
    UqffValidation {
        /// The UQFF file path
        path: String,
        /// The reason for the failure
        reason: String,
    },

    /// Topology file error.
    ///
    /// This error occurs when loading or parsing topology files for
    /// per-layer quantization.
    #[error("Topology file error for '{path}': {reason}")]
    TopologyFile {
        /// The topology file path
        path: String,
        /// The reason for the failure
        reason: String,
    },

    /// Generic error wrapper for underlying errors.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl MistralRsError {
    // =========================================================================
    // Convenience constructors for common error patterns
    // =========================================================================

    /// Create a model loading error with context.
    pub fn model_load(model_id: impl Into<String>, reason: impl Into<String>) -> Self {
        let model_id = model_id.into();        let reason = reason.into();        let suggestion = Self::suggest_model_load_fix(&reason);
        Self::ModelLoad { model_id, reason, suggestion }
    }

    /// Create a model not found error.
    pub fn model_not_found(path: impl Into<String>) -> Self {
        let path = path.into();        let suggestion = "Verify the path exists and is accessible. For HuggingFace models, ensure the model ID is correct (e.g., 'mistralai/Magistral-Small-2509').".to_string();
        Self::ModelNotFound { path, suggestion }
    }

    /// Create an unsupported architecture error.
    pub fn unsupported_architecture(
        architecture: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        Self::UnsupportedArchitecture {
            architecture: architecture.into(),
            model_id: model_id.into(),
            supported: "Plain, Vision, Diffusion, Speech, Embedding, LoRA, X-LoRA".to_string(),
        }
    }

    /// Create a device not available error.
    pub fn device_not_available(device: impl Into<String>) -> Self {
        let device = device.into();        let suggestion = Self::suggest_device_fix(&device);
        Self::DeviceNotAvailable { device, suggestion }
    }

    /// Create an out of memory error.
    pub fn out_of_memory(operation: impl Into<String>, details: impl Into<String>) -> Self {
        Self::OutOfMemory {
            operation: operation.into(),
            details: details.into(),
            suggestion: "Try: 1) Enable ISQ quantization (e.g., Q4K) to reduce memory usage, \
                        2) Reduce context length (num_ctx), \
                        3) Enable PagedAttention for more efficient memory use, \
                        4) Use a smaller model variant."
                .to_string(),
        }
    }

    /// Create an inference error.
    pub fn inference(model_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Inference { model_id: model_id.into(), reason: reason.into()}
    }

    /// Create an invalid configuration error.
    pub fn invalid_config(
        field: impl Into<String>,
        reason: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self::InvalidConfig {
            field: field.into(),
            reason: reason.into(),
            suggestion: suggestion.into(),
        }
    }

    /// Create an image processing error.
    pub fn image_processing(reason: impl Into<String>) -> Self {
        Self::ImageProcessing { reason: reason.into()}
    }

    /// Create an audio processing error.
    pub fn audio_processing(reason: impl Into<String>) -> Self {
        Self::AudioProcessing { reason: reason.into()}
    }

    /// Create a tool conversion error.
    pub fn tool_conversion(tool_name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ToolConversion { tool_name: tool_name.into(), reason: reason.into()}
    }

    /// Create a chat template error.
    pub fn chat_template(reason: impl Into<String>) -> Self {
        Self::ChatTemplate {
            reason: reason.into(),
            suggestion: "Verify the chat template syntax. For custom templates, ensure they follow the Jinja2 format used by HuggingFace tokenizers.".to_string(),
        }
    }

    /// Create an adapter loading error.
    pub fn adapter_load(adapter_name: impl Into<String>, reason: impl Into<String>) -> Self {
        let adapter_name = adapter_name.into();        let reason = reason.into();        let suggestion = "Verify the adapter path/ID is correct. For HuggingFace adapters, ensure the adapter is compatible with the base model.".to_string();
        Self::AdapterLoad { adapter_name, reason, suggestion }
    }

    /// Create an adapter not found error.
    pub fn adapter_not_found(name: impl Into<String>, available: Vec<String>) -> Self {
        Self::AdapterNotFound { name: name.into(), available }
    }

    /// Create an MCP client error.
    pub fn mcp_client(server: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::McpClient { server: server.into(), reason: reason.into()}
    }

    /// Create an embedding error.
    pub fn embedding(reason: impl Into<String>) -> Self {
        Self::Embedding { reason: reason.into()}
    }

    /// Create a speech generation error.
    pub fn speech(reason: impl Into<String>) -> Self {
        Self::Speech { reason: reason.into()}
    }

    /// Create a diffusion/image generation error.
    pub fn diffusion(reason: impl Into<String>) -> Self {
        Self::Diffusion { reason: reason.into()}
    }

    /// Create a multi-model routing error.
    pub fn multi_model_routing(model_name: impl Into<String>, available: Vec<String>) -> Self {
        Self::MultiModelRouting { model_name: model_name.into(), available }
    }

    /// Create a UQFF validation error.
    pub fn uqff_validation(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::UqffValidation { path: path.into(), reason: reason.into()}
    }

    /// Create a topology file error.
    pub fn topology_file(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::TopologyFile { path: path.into(), reason: reason.into()}
    }

    // =========================================================================
    // Helper methods for generating suggestions
    // =========================================================================

    /// Generate a suggestion for model loading errors based on the error message.
    fn suggest_model_load_fix(reason: &str) -> String {
        let reason_lower = reason.to_lowercase();

        if reason_lower.contains("not found") || reason_lower.contains("404") {
            return "Verify the model ID is correct. Check https://huggingface.co/models for available models.".to_string();
        }

        if reason_lower.contains("memory") || reason_lower.contains("oom") {
            return "Try enabling ISQ quantization (e.g., Q4K) or using a smaller model."
                .to_string();
        }

        if reason_lower.contains("network") || reason_lower.contains("connection") {
            return "Check your internet connection. For offline use, download the model first and use a local path.".to_string();
        }

        if reason_lower.contains("permission") || reason_lower.contains("access") {
            return "Check file permissions. For gated models, ensure you have accepted the license on HuggingFace.".to_string();
        }

        if reason_lower.contains("format") || reason_lower.contains("invalid") {
            return "Verify the model format is supported. For GGUF files, ensure they are not corrupted.".to_string();
        }

        "Check the model documentation for requirements and compatibility.".to_string()
    }

    /// Generate a suggestion for device availability errors.
    fn suggest_device_fix(device: &str) -> String {
        let device_lower = device.to_lowercase();

        if device_lower.contains("cuda") {
            return "CUDA is not available. Ensure: 1) NVIDIA GPU is present, 2) CUDA drivers are installed, 3) The 'cuda' feature is enabled in Cargo.toml.".to_string();
        }

        if device_lower.contains("metal") {
            return "Metal is not available. Ensure: 1) Running on macOS with Apple Silicon or AMD GPU, 2) The 'metal' feature is enabled in Cargo.toml.".to_string();
        }

        "Use Device::Auto to automatically select the best available device.".to_string()
    }

    // =========================================================================
    // Error classification methods
    // =========================================================================

    /// Check if this error is recoverable (can be retried).
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            MistralRsError::Inference { .. }
                | MistralRsError::McpClient { .. }
                | MistralRsError::Other(_)
        )
    }

    /// Check if this error is related to configuration.
    pub fn is_config_error(&self) -> bool {
        matches!(
            self,
            MistralRsError::InvalidConfig { .. }
                | MistralRsError::UnsupportedArchitecture { .. }
                | MistralRsError::TopologyFile { .. }
                | MistralRsError::ChatTemplate { .. }
        )
    }

    /// Check if this error is related to resources (memory, devices).
    pub fn is_resource_error(&self) -> bool {
        matches!(
            self,
            MistralRsError::OutOfMemory { .. }
                | MistralRsError::DeviceNotAvailable { .. }
                | MistralRsError::ModelNotFound { .. }
        )
    }

    /// Get the error category for logging/metrics.
    pub fn category(&self) -> &'static str {
        match self {
            MistralRsError::ModelLoad { .. } => "model_load",
            MistralRsError::ModelNotFound { .. } => "model_not_found",
            MistralRsError::UnsupportedArchitecture { .. } => "unsupported_architecture",
            MistralRsError::DeviceNotAvailable { .. } => "device_not_available",
            MistralRsError::OutOfMemory { .. } => "out_of_memory",
            MistralRsError::Inference { .. } => "inference",
            MistralRsError::InvalidConfig { .. } => "invalid_config",
            MistralRsError::ImageProcessing { .. } => "image_processing",
            MistralRsError::AudioProcessing { .. } => "audio_processing",
            MistralRsError::ToolConversion { .. } => "tool_conversion",
            MistralRsError::ChatTemplate { .. } => "chat_template",
            MistralRsError::AdapterLoad { .. } => "adapter_load",
            MistralRsError::AdapterNotFound { .. } => "adapter_not_found",
            MistralRsError::McpClient { .. } => "mcp_client",
            MistralRsError::Embedding { .. } => "embedding",
            MistralRsError::Speech { .. } => "speech",
            MistralRsError::Diffusion { .. } => "diffusion",
            MistralRsError::MultiModelRouting { .. } => "multi_model_routing",
            MistralRsError::UqffValidation { .. } => "uqff_validation",
            MistralRsError::TopologyFile { .. } => "topology_file",
            MistralRsError::Other(_) => "other",
        }
    }
}

/// Result type alias for MistralRsError
pub type Result<T> = std::result::Result<T, MistralRsError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_load_error_display() {
        let err = MistralRsError::model_load("test/model", "connection timeout");
        let msg = err.to_string();
        assert!(msg.contains("test/model"));
        assert!(msg.contains("connection timeout"));
        assert!(msg.contains("Check")); // Should have a suggestion
    }

    #[test]
    fn test_model_not_found_error_display() {
        let err = MistralRsError::model_not_found("/path/to/model");
        let msg = err.to_string();
        assert!(msg.contains("/path/to/model"));
        assert!(msg.contains("Verify")); // Should have a suggestion
    }

    #[test]
    fn test_out_of_memory_error_display() {
        let err = MistralRsError::out_of_memory("loading model", "GPU memory exhausted");
        let msg = err.to_string();
        assert!(msg.contains("loading model"));
        assert!(msg.contains("GPU memory exhausted"));
        assert!(msg.contains("ISQ")); // Should suggest ISQ
    }

    #[test]
    fn test_device_not_available_cuda() {
        let err = MistralRsError::device_not_available("CUDA:0");
        let msg = err.to_string();
        assert!(msg.contains("CUDA:0"));
        assert!(msg.contains("NVIDIA")); // Should mention NVIDIA
    }

    #[test]
    fn test_device_not_available_metal() {
        let err = MistralRsError::device_not_available("Metal");
        let msg = err.to_string();
        assert!(msg.contains("Metal"));
        assert!(msg.contains("macOS")); // Should mention macOS
    }

    #[test]
    fn test_adapter_not_found_error() {
        let err = MistralRsError::adapter_not_found(
            "missing-adapter",
            vec!["adapter1".to_string(), "adapter2".to_string()],
        );
        let msg = err.to_string();
        assert!(msg.contains("missing-adapter"));
        assert!(msg.contains("adapter1"));
        assert!(msg.contains("adapter2"));
    }

    #[test]
    fn test_error_category() {
        assert_eq!(MistralRsError::model_load("m", "r").category(), "model_load");
        assert_eq!(MistralRsError::model_not_found("p").category(), "model_not_found");
        assert_eq!(MistralRsError::out_of_memory("o", "d").category(), "out_of_memory");
        assert_eq!(MistralRsError::inference("m", "r").category(), "inference");
    }

    #[test]
    fn test_error_classification() {
        // Recoverable errors
        assert!(MistralRsError::inference("m", "r").is_recoverable());
        assert!(MistralRsError::mcp_client("s", "r").is_recoverable());

        // Non-recoverable errors
        assert!(!MistralRsError::model_not_found("p").is_recoverable());
        assert!(!MistralRsError::invalid_config("f", "r", "s").is_recoverable());

        // Config errors
        assert!(MistralRsError::invalid_config("f", "r", "s").is_config_error());
        assert!(MistralRsError::chat_template("r").is_config_error());

        // Resource errors
        assert!(MistralRsError::out_of_memory("o", "d").is_resource_error());
        assert!(MistralRsError::device_not_available("d").is_resource_error());
    }

    #[test]
    fn test_suggestion_generation_network() {
        let err = MistralRsError::model_load("test/model", "network connection failed");
        let msg = err.to_string();
        assert!(msg.contains("internet connection") || msg.contains("offline"));
    }

    #[test]
    fn test_suggestion_generation_memory() {
        let err = MistralRsError::model_load("test/model", "out of memory");
        let msg = err.to_string();
        assert!(msg.contains("ISQ") || msg.contains("quantization"));
    }

    #[test]
    fn test_suggestion_generation_not_found() {
        let err = MistralRsError::model_load("test/model", "model not found 404");
        let msg = err.to_string();
        assert!(msg.contains("huggingface") || msg.contains("Verify"));
    }
}
