//! Local model registry for downloading and caching model weights.

use std::path::PathBuf;

use crate::error::{AudioError, AudioResult};

/// Registry for managing local model downloads and caching.
///
/// Supports both MLX-format (`.safetensors` + `config.json`) and
/// ONNX-format (`.onnx`) models from HuggingFace Hub.
///
/// # Example
///
/// ```rust,ignore
/// let registry = LocalModelRegistry::default();
/// let path = registry.get_or_download("mlx-community/Kokoro-82M-bf16").await?;
/// ```
pub struct LocalModelRegistry {
    cache_dir: PathBuf,
}

impl Default for LocalModelRegistry {
    fn default() -> Self {
        let cache_dir = dirs_cache_dir().join("adk-audio/models");
        Self { cache_dir }
    }
}

impl LocalModelRegistry {
    /// Create a registry with a custom cache directory.
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self { cache_dir: cache_dir.into() }
    }

    /// Get the local path for a model, downloading from HuggingFace Hub if not cached.
    pub async fn get_or_download(&self, model_id: &str) -> AudioResult<PathBuf> {
        if model_id.is_empty() {
            return Err(AudioError::ModelDownload {
                model_id: model_id.to_string(),
                message: "model_id cannot be empty".into(),
            });
        }
        let local_path = self.cache_dir.join(model_id.replace('/', "--"));
        if local_path.exists() {
            return Ok(local_path);
        }
        // TODO: Download via hf-hub crate in Phase 10
        Err(AudioError::ModelDownload {
            model_id: model_id.to_string(),
            message: format!(
                "model not cached and download not yet implemented. Expected at: {}",
                local_path.display()
            ),
        })
    }

    /// Get the cache directory path.
    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    /// Compute the local path for a model ID (without downloading).
    pub fn model_path(&self, model_id: &str) -> PathBuf {
        self.cache_dir.join(model_id.replace('/', "--"))
    }
}

/// Get the user's cache directory, falling back to current directory.
fn dirs_cache_dir() -> PathBuf {
    // Use HOME env var on macOS/Linux
    std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".cache"))
        .unwrap_or_else(|_| PathBuf::from(".cache"))
}
