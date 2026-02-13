use serde::{Deserialize, Serialize};
use std::fmt::{self, Formatter};
use std::str::FromStr;

/// Represents available Gemini and embedding models.
///
/// This enum supports serialization/deserialization compatible with the
/// Google AI Studio API format (e.g., `models/gemini-3-flash`).
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[non_exhaustive] // Allows adding new models (Gemini 3.5, 4.0) without breaking changes
pub enum Model {
    /// The latest Flash model (v3), optimized for high-frequency, low-latency tasks.
    #[default]
    #[serde(rename = "models/gemini-3-flash")]
    Gemini3Flash,

    /// The latest Pro model (v3), optimized for complex reasoning and large context.
    #[serde(rename = "models/gemini-3-pro")]
    Gemini3Pro,

    // --- Legacy v2.5 Models ---
    #[serde(rename = "models/gemini-2.5-flash")]
    Gemini25Flash,
    #[serde(rename = "models/gemini-2.5-flash-lite")]
    Gemini25FlashLite,
    #[serde(rename = "models/gemini-2.5-pro")]
    Gemini25Pro,

    // --- Embeddings ---
    #[serde(rename = "models/text-embedding-004")]
    TextEmbedding004,
    #[serde(rename = "models/gemini-embedding-001")]
    GeminiEmbedding001,

    /// Fallback for experimental or future models not yet typed.
    #[serde(untagged)]
    Custom(String),
}

impl Model {
    /// Returns the API model identifier (e.g., "models/gemini-3-flash").
    pub fn as_str(&self) -> &str {
        match self {
            Model::Gemini3Flash => "models/gemini-3-flash",
            Model::Gemini3Pro => "models/gemini-3-pro",
            Model::Gemini25Flash => "models/gemini-2.5-flash",
            Model::Gemini25FlashLite => "models/gemini-2.5-flash-lite",
            Model::Gemini25Pro => "models/gemini-2.5-pro",
            Model::TextEmbedding004 => "models/text-embedding-004",
            Model::GeminiEmbedding001 => "models/gemini-embedding-001",
            Model::Custom(model) => model,
        }
    }

    /// Constructs the fully qualified Vertex AI resource path.
    ///
    /// # Arguments
    /// * `project_id` - The GCP project ID.
    /// * `location` - The GCP region (e.g., "us-central1").
    pub fn vertex_model_path(&self, project_id: &str, location: &str) -> String {
        let full_name = self.as_str();

        // 1. Handle Custom overrides that are already fully qualified paths
        if let Model::Custom(custom_path) = self {
            if custom_path.starts_with("projects/") {
                return custom_path.clone();
            }
            if custom_path.starts_with("publishers/") {
                return format!("projects/{project_id}/locations/{location}/{custom_path}");
            }
        }

        // 2. Standardize logic: Strip "models/" prefix to get the raw Model ID.
        // This prevents having to maintain a second match statement for IDs.
        let model_id = full_name.strip_prefix("models/").unwrap_or(full_name);

        format!("projects/{project_id}/locations/{location}/publishers/google/models/{model_id}")
    }
}

// --- Standard Traits Implementation ---

impl AsRef<str> for Model {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<Model> for String {
    fn from(model: Model) -> Self {
        model.to_string()
    }
}

impl From<String> for Model {
    fn from(s: String) -> Self {
        // Attempt to parse known models, fallback to Custom
        // This is a simple implementation; for stricter parsing use serde_json::from_value
        match s.as_str() {
            "models/gemini-3-flash" => Model::Gemini3Flash,
            "models/gemini-3-pro" => Model::Gemini3Pro,
            "models/gemini-2.5-flash" => Model::Gemini25Flash,
            "models/gemini-2.5-flash-lite" => Model::Gemini25FlashLite,
            "models/gemini-2.5-pro" => Model::Gemini25Pro,
            "models/text-embedding-004" => Model::TextEmbedding004,
            "models/gemini-embedding-001" => Model::GeminiEmbedding001,
            _ => Model::Custom(s),
        }
    }
}

impl FromStr for Model {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Model::from(s.to_string()))
    }
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vertex_paths() {
        let project = "my-project";
        let loc = "us-central1";

        // Standard Model
        let model = Model::Gemini3Flash;
        assert_eq!(
            model.vertex_model_path(project, loc),
            "projects/my-project/locations/us-central1/publishers/google/models/gemini-3-flash"
        );

        // Custom Model (Tuned)
        let tuned =
            Model::Custom("projects/my-project/locations/us-central1/endpoints/12345".to_string());
        assert_eq!(
            tuned.vertex_model_path(project, loc),
            "projects/my-project/locations/us-central1/endpoints/12345"
        );
    }
}
