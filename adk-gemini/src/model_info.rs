//! Types for the Gemini Models API (`models.list` and `models.get`).
//!
//! These types represent the metadata returned by the Gemini API about
//! available models, including token limits, supported methods, and
//! default generation parameters.

use serde::{Deserialize, Serialize};

/// Information about a Generative Language Model returned by the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// Resource name in `models/{model}` format.
    pub name: String,
    /// Base model identifier (e.g. `gemini-2.5-flash`).
    #[serde(default)]
    pub base_model_id: String,
    /// Version string (e.g. `2.5`).
    #[serde(default)]
    pub version: String,
    /// Human-readable display name.
    #[serde(default)]
    pub display_name: String,
    /// Short description of the model.
    #[serde(default)]
    pub description: String,
    /// Maximum number of input tokens.
    #[serde(default)]
    pub input_token_limit: u32,
    /// Maximum number of output tokens.
    #[serde(default)]
    pub output_token_limit: u32,
    /// Supported generation methods (e.g. `generateContent`, `embedContent`).
    #[serde(default)]
    pub supported_generation_methods: Vec<String>,
    /// Whether the model supports thinking/reasoning.
    #[serde(default)]
    pub thinking: bool,
    /// Default temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Maximum allowed temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_temperature: Option<f64>,
    /// Default top-p value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Default top-k value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
}

impl ModelInfo {
    /// Returns true if this model supports `generateContent`.
    pub fn supports_generate_content(&self) -> bool {
        self.supported_generation_methods.iter().any(|m| m == "generateContent")
    }

    /// Returns true if this model supports `embedContent`.
    pub fn supports_embed_content(&self) -> bool {
        self.supported_generation_methods.iter().any(|m| m == "embedContent")
    }
}

/// Paginated response from `models.list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListModelsResponse {
    /// The returned models.
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    /// Token for the next page, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}
