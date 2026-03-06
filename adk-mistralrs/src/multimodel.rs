//! Multi-model support for serving multiple models from a single instance.
//!
//! This module provides `MistralRsMultiModel` which allows loading and routing
//! requests to multiple models based on model name. This is useful for A/B testing,
//! model comparison, and serving different models for different use cases.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_mistralrs::{MistralRsMultiModel, MistralRsConfig, ModelSource};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let mut multi_model = MistralRsMultiModel::new();
//!
//!     // Add models
//!     let config1 = MistralRsConfig::builder()
//!         .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
//!         .build();
//!     multi_model.add_model("phi", config1).await?;
//!
//!     let config2 = MistralRsConfig::builder()
//!         .model_source(ModelSource::huggingface("meta-llama/Llama-3.2-3B-Instruct"))
//!         .build();
//!     multi_model.add_model("llama", config2).await?;
//!
//!     // Set default model
//!     multi_model.set_default("phi")?;
//!
//!     // Route requests by model name
//!     let response = multi_model.generate_with_model("llama", request).await?;
//!
//!     Ok(())
//! }
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use adk_core::{Llm, LlmRequest, LlmResponseStream};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::MistralRsModel;
use crate::config::{MistralRsConfig, ModelArchitecture, ModelSource, QuantizationLevel};
use crate::error::{MistralRsError, Result};

/// Multi-model server that can load and route requests to multiple models.
///
/// This struct manages multiple `MistralRsModel` instances and routes requests
/// to the appropriate model based on the model name specified in the request.
pub struct MistralRsMultiModel {
    /// Map of model name to model instance
    models: RwLock<HashMap<String, Arc<MistralRsModel>>>,
    /// Default model name to use when no model is specified
    default_model: RwLock<Option<String>>,
    /// Name for this multi-model instance
    name: String,
}

impl MistralRsMultiModel {
    /// Create a new empty multi-model instance.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_mistralrs::MistralRsMultiModel;
    ///
    /// let multi_model = MistralRsMultiModel::new();
    /// ```
    pub fn new() -> Self {
        Self {
            models: RwLock::new(HashMap::new()),
            default_model: RwLock::new(None),
            name: "mistralrs-multi".to_string(),
        }
    }

    /// Create a new multi-model instance with a custom name.
    pub fn with_name(name: impl Into<String>) -> Self {
        Self {
            models: RwLock::new(HashMap::new()),
            default_model: RwLock::new(None),
            name: name.into(),
        }
    }

    /// Add a model to the multi-model instance.
    ///
    /// # Arguments
    ///
    /// * `name` - Unique name for this model (used for routing)
    /// * `config` - Configuration for the model
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = MistralRsConfig::builder()
    ///     .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
    ///     .build();
    /// multi_model.add_model("phi", config).await?;
    /// ```
    pub async fn add_model(&self, name: impl Into<String>, config: MistralRsConfig) -> Result<()> {
        let name = name.into();        info!("Adding model '{}' to multi-model instance", name);

        let model = MistralRsModel::new(config).await?;
        let model = Arc::new(model);

        let mut models = self.models.write().await;

        // If this is the first model, set it as default
        let is_first = models.is_empty();
        models.insert(name.clone(), model);

        if is_first {
            let mut default = self.default_model.write().await;
            *default = Some(name.clone());
            info!("Set '{}' as default model", name);
        }

        Ok(())
    }

    /// Add a pre-built model to the multi-model instance.
    ///
    /// # Arguments
    ///
    /// * `name` - Unique name for this model (used for routing)
    /// * `model` - Pre-built MistralRsModel instance
    pub async fn add_existing_model(
        &self,
        name: impl Into<String>,
        model: MistralRsModel,
    ) -> Result<()> {
        let name = name.into();        info!("Adding existing model '{}' to multi-model instance", name);

        let model = Arc::new(model);
        let mut models = self.models.write().await;

        let is_first = models.is_empty();
        models.insert(name.clone(), model);

        if is_first {
            let mut default = self.default_model.write().await;
            *default = Some(name.clone());
            info!("Set '{}' as default model", name);
        }

        Ok(())
    }

    /// Remove a model from the multi-model instance.
    ///
    /// # Arguments
    ///
    /// * `name` - Name of the model to remove
    ///
    /// # Returns
    ///
    /// Returns `true` if the model was removed, `false` if it didn't exist.
    pub async fn remove_model(&self, name: &str) -> bool {
        let mut models = self.models.write().await;
        let removed = models.remove(name).is_some();

        if removed {
            info!("Removed model '{}' from multi-model instance", name);

            // If we removed the default model, clear it or set a new one
            let mut default = self.default_model.write().await;
            if default.as_deref() == Some(name) {
                *default = models.keys().next().cloned();
                if let Some(new_default) = default.as_ref() {
                    info!("Set '{}' as new default model", new_default);
                }
            }
        }

        removed
    }

    /// Set the default model to use when no model is specified.
    ///
    /// # Arguments
    ///
    /// * `name` - Name of the model to set as default
    ///
    /// # Errors
    ///
    /// Returns an error if the model doesn't exist.
    pub async fn set_default(&self, name: &str) -> Result<()> {
        let models = self.models.read().await;
        if !models.contains_key(name) {
            return Err(MistralRsError::multi_model_routing(
                name,
                models.keys().cloned().collect(),
            ));
        }
        drop(models);

        let mut default = self.default_model.write().await;
        *default = Some(name.to_string());
        info!("Set '{}' as default model", name);

        Ok(())
    }

    /// Get the name of the default model.
    pub async fn default_model(&self) -> Option<String> {
        self.default_model.read().await.clone()
    }

    /// Get a list of all loaded model names.
    pub async fn model_names(&self) -> Vec<String> {
        self.models.read().await.keys().cloned().collect()
    }

    /// Check if a model with the given name exists.
    pub async fn has_model(&self, name: &str) -> bool {
        self.models.read().await.contains_key(name)
    }

    /// Get the number of loaded models.
    pub async fn model_count(&self) -> usize {
        self.models.read().await.len()
    }

    /// Get a reference to a specific model by name.
    pub async fn get_model(&self, name: &str) -> Option<Arc<MistralRsModel>> {
        self.models.read().await.get(name).cloned()
    }

    /// Generate content using a specific model.
    ///
    /// # Arguments
    ///
    /// * `model_name` - Name of the model to use (or None for default)
    /// * `request` - The LLM request
    /// * `stream` - Whether to stream the response
    ///
    /// # Errors
    ///
    /// Returns an error if the model doesn't exist or generation fails.
    pub async fn generate_with_model(
        &self,
        model_name: Option<&str>,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let resolved_name = match model_name {
            Some(name) => name.to_string(),
            None => {
                let default = self.default_model.read().await;
                default.clone().ok_or_else(|| {
                    adk_core::AdkError::Model(
                        "No default model set and no model name specified".to_string(),
                    )
                })?
            }
        };

        debug!("Routing request to model '{}'", resolved_name);

        let models = self.models.read().await;
        let model = models.get(&resolved_name).ok_or_else(|| {
            adk_core::AdkError::Model(format!(
                "Model '{}' not found. Available models: {:?}",
                resolved_name,
                models.keys().collect::<Vec<_>>()
            ))
        })?;

        model.generate_content(request, stream).await
    }
}

impl Default for MistralRsMultiModel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Llm for MistralRsMultiModel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        // Use the default model
        self.generate_with_model(None, request, stream).await
    }
}

impl std::fmt::Debug for MistralRsMultiModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MistralRsMultiModel").field("name", &self.name).finish()
    }
}

/// Configuration for a single model in a multi-model TOML config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiModelEntry {
    /// Model type/architecture
    #[serde(flatten)]
    pub model_type: MultiModelType,

    /// ISQ quantization level (optional)
    #[serde(default)]
    pub in_situ_quant: Option<String>,

    /// Whether this is the default model
    #[serde(default)]
    pub default: bool,
}

/// Model type specification for multi-model config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum MultiModelType {
    /// Plain text model
    Plain {
        /// HuggingFace model ID or local path
        model_id: String,
    },
    /// Vision model
    Vision {
        /// HuggingFace model ID or local path
        model_id: String,
        /// Vision architecture type
        #[serde(default)]
        arch: Option<String>,
    },
    /// Embedding model
    Embedding {
        /// HuggingFace model ID or local path
        model_id: String,
        /// Embedding architecture type
        #[serde(default)]
        arch: Option<String>,
    },
    /// GGUF quantized model
    Gguf {
        /// Path to GGUF file
        path: String,
    },
    /// UQFF pre-quantized model
    Uqff {
        /// HuggingFace model ID
        model_id: String,
        /// UQFF file names
        files: Vec<String>,
    },
}

/// Multi-model configuration loaded from TOML/JSON.
pub type MultiModelConfig = HashMap<String, MultiModelEntry>;

impl MistralRsMultiModel {
    /// Load multi-model configuration from a JSON file.
    ///
    /// The JSON format follows the mistral.rs multi-model config format:
    ///
    /// ```json
    /// {
    ///   "model-name": {
    ///     "Plain": { "model_id": "org/model" },
    ///     "in_situ_quant": "4"
    ///   }
    /// }
    /// ```
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the JSON configuration file
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let multi_model = MistralRsMultiModel::from_config("models.json").await?;
    /// ```
    pub async fn from_config(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(MistralRsError::invalid_config(
                "config_path",
                format!("Configuration file not found: {}", path.display()),
                "Verify the configuration file path is correct",
            ));
        }

        let content = std::fs::read_to_string(path).map_err(|e| {
            MistralRsError::invalid_config(
                "config_path",
                format!("Failed to read config file '{}': {}", path.display(), e),
                "Verify the file has read permissions",
            )
        })?;

        let config: MultiModelConfig = serde_json::from_str(&content).map_err(|e| {
            MistralRsError::invalid_config(
                "config_path",
                format!("Failed to parse config file '{}': {}", path.display(), e),
                "Verify the JSON format is correct. See documentation for multi-model config schema.",
            )
        })?;

        Self::from_config_map(config).await
    }

    /// Load multi-model configuration from a parsed config map.
    ///
    /// # Arguments
    ///
    /// * `config` - Parsed multi-model configuration
    pub async fn from_config_map(config: MultiModelConfig) -> Result<Self> {
        let multi_model = Self::new();
        let mut default_model: Option<String> = None;

        for (name, entry) in config {
            info!("Loading model '{}' from config", name);

            let model_config = entry_to_config(&entry)?;
            multi_model.add_model(&name, model_config).await?;

            if entry.default {
                default_model = Some(name.clone());
            }
        }

        // Set explicit default if specified
        if let Some(default) = default_model {
            multi_model.set_default(&default).await?;
        }

        Ok(multi_model)
    }
}

/// Convert a MultiModelEntry to MistralRsConfig.
fn entry_to_config(entry: &MultiModelEntry) -> Result<MistralRsConfig> {
    let mut builder = MistralRsConfig::builder();

    // Set model source based on type
    match &entry.model_type {
        MultiModelType::Plain { model_id } => {
            builder = builder
                .model_source(ModelSource::huggingface(model_id))
                .architecture(ModelArchitecture::Plain);
        }
        MultiModelType::Vision { model_id, .. } => {
            builder = builder
                .model_source(ModelSource::huggingface(model_id))
                .architecture(ModelArchitecture::Vision);
        }
        MultiModelType::Embedding { model_id, .. } => {
            builder = builder
                .model_source(ModelSource::huggingface(model_id))
                .architecture(ModelArchitecture::Embedding);
        }
        MultiModelType::Gguf { path } => {
            builder = builder
                .model_source(ModelSource::gguf(path))
                .architecture(ModelArchitecture::Plain);
        }
        MultiModelType::Uqff { model_id, files: _ } => {
            // For UQFF, we use the model_id as HuggingFace source
            // The files are handled separately during model loading
            builder = builder
                .model_source(ModelSource::huggingface(model_id))
                .architecture(ModelArchitecture::Plain);
            // Note: UQFF file handling would need to be added to MistralRsModel
        }
    }

    // Apply ISQ quantization if specified
    if let Some(isq) = &entry.in_situ_quant {
        let level = parse_quantization_level(isq)?;
        builder = builder.isq(level);
    }

    Ok(builder.build())
}

/// Parse a quantization level string to QuantizationLevel enum.
fn parse_quantization_level(s: &str) -> Result<QuantizationLevel> {
    match s.to_lowercase().as_str() {
        "2" | "q2k" | "q2_k" => Ok(QuantizationLevel::Q2K),
        "3" | "q3k" | "q3_k" => Ok(QuantizationLevel::Q3K),
        "4" | "q4k" | "q4_k" | "q4_0" => Ok(QuantizationLevel::Q4K),
        "5" | "q5k" | "q5_k" | "q5_0" => Ok(QuantizationLevel::Q5K),
        "6" | "q6k" | "q6_k" => Ok(QuantizationLevel::Q6K),
        "8" | "q8_0" => Ok(QuantizationLevel::Q8_0),
        "q4_1" => Ok(QuantizationLevel::Q4_1),
        "q5_1" => Ok(QuantizationLevel::Q5_1),
        "q8_1" => Ok(QuantizationLevel::Q8_1),
        _ => Err(MistralRsError::invalid_config(
            "in_situ_quant",
            format!("Unknown quantization level: '{}'", s),
            "Valid values: 2, 3, 4, 5, 6, 8, q2k, q3k, q4k, q5k, q6k, q4_0, q4_1, q5_0, q5_1, q8_0, q8_1",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_quantization_level() {
        assert!(matches!(parse_quantization_level("4"), Ok(QuantizationLevel::Q4K)));
        assert!(matches!(parse_quantization_level("q4k"), Ok(QuantizationLevel::Q4K)));
        assert!(matches!(parse_quantization_level("Q4K"), Ok(QuantizationLevel::Q4K)));
        assert!(matches!(parse_quantization_level("8"), Ok(QuantizationLevel::Q8_0)));
        assert!(matches!(parse_quantization_level("q8_0"), Ok(QuantizationLevel::Q8_0)));
        assert!(parse_quantization_level("invalid").is_err());
    }

    #[test]
    fn test_multi_model_entry_deserialize() {
        let json = r#"{
            "Plain": { "model_id": "test/model" },
            "in_situ_quant": "4"
        }"#;

        let entry: MultiModelEntry = serde_json::from_str(json).unwrap();
        assert!(matches!(entry.model_type, MultiModelType::Plain { .. }));
        assert_eq!(entry.in_situ_quant, Some("4".to_string()));
    }

    #[test]
    fn test_multi_model_config_deserialize() {
        let json = r#"{
            "llama": {
                "Plain": { "model_id": "meta-llama/Llama-3.2-3B-Instruct" },
                "in_situ_quant": "4"
            },
            "phi": {
                "Plain": { "model_id": "microsoft/Phi-3.5-mini-instruct" }
            }
        }"#;

        let config: MultiModelConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.len(), 2);
        assert!(config.contains_key("llama"));
        assert!(config.contains_key("phi"));
    }

    #[tokio::test]
    async fn test_multi_model_new() {
        let multi_model = MistralRsMultiModel::new();
        assert_eq!(multi_model.model_count().await, 0);
        assert!(multi_model.default_model().await.is_none());
    }

    #[tokio::test]
    async fn test_multi_model_with_name() {
        let multi_model = MistralRsMultiModel::with_name("test-multi");
        assert_eq!(multi_model.name(), "test-multi");
    }

    #[test]
    fn test_entry_to_config_plain() {
        let entry = MultiModelEntry {
            model_type: MultiModelType::Plain { model_id: "test/model".to_string() },
            in_situ_quant: None,
            default: false,
        };

        let config = entry_to_config(&entry).unwrap();
        assert!(matches!(config.model_source, ModelSource::HuggingFace(_)));
        assert_eq!(config.architecture, ModelArchitecture::Plain);
    }

    #[test]
    fn test_entry_to_config_with_isq() {
        let entry = MultiModelEntry {
            model_type: MultiModelType::Plain { model_id: "test/model".to_string() },
            in_situ_quant: Some("4".to_string()),
            default: false,
        };

        let config = entry_to_config(&entry).unwrap();
        assert!(config.isq.is_some());
    }

    #[test]
    fn test_entry_to_config_gguf() {
        let entry = MultiModelEntry {
            model_type: MultiModelType::Gguf { path: "/path/to/model.gguf".to_string() },
            in_situ_quant: None,
            default: false,
        };

        let config = entry_to_config(&entry).unwrap();
        assert!(matches!(config.model_source, ModelSource::Gguf(_)));
    }
}
