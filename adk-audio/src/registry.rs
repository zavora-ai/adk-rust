//! Local model registry for downloading and caching model weights.
//!
//! Uses the [`hf_hub`] crate (when available) to download models from
//! HuggingFace Hub on first use, caching them locally for subsequent runs.

use std::path::PathBuf;

use crate::error::{AudioError, AudioResult};

/// Registry for managing local model downloads and caching.
///
/// Supports both MLX-format (`.safetensors` + `config.json`) and
/// ONNX-format (`.onnx`) models from HuggingFace Hub.
///
/// On first access the model is downloaded via the HuggingFace Hub API
/// (requires the `onnx` or `mlx` feature for the `hf-hub` dependency).
/// Subsequent calls return the cached path immediately.
///
/// # Example
///
/// ```rust,ignore
/// let registry = LocalModelRegistry::default();
/// let path = registry.get_or_download("onnx-community/whisper-base").await?;
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
    ///
    /// The method first checks for a local cache directory at
    /// `<cache_dir>/<org>--<model>`. If found, it returns immediately.
    /// Otherwise it uses the `hf-hub` crate to download all repository
    /// files into the HuggingFace cache and returns that path.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::ModelDownload`] if `model_id` is empty or
    /// the download fails.
    pub async fn get_or_download(&self, model_id: &str) -> AudioResult<PathBuf> {
        if model_id.is_empty() {
            return Err(AudioError::ModelDownload {
                model_id: model_id.to_string(),
                message: "model_id cannot be empty".into(),
            });
        }

        // Check our own cache directory first
        let local_path = self.cache_dir.join(model_id.replace('/', "--"));
        if local_path.exists() {
            tracing::debug!(model_id, path = %local_path.display(), "model found in local cache");
            return Ok(local_path);
        }

        // Download via hf-hub
        self.download_from_hub(model_id).await
    }

    /// Get the cache directory path.
    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    /// Compute the local path for a model ID (without downloading).
    pub fn model_path(&self, model_id: &str) -> PathBuf {
        self.cache_dir.join(model_id.replace('/', "--"))
    }

    /// Download a model repository from HuggingFace Hub.
    ///
    /// Uses the `hf-hub` crate's sync API (wrapped in `spawn_blocking`
    /// so we don't block the async runtime). The Hub API caches files
    /// under `~/.cache/huggingface/hub/` by default; we return the
    /// snapshot directory that contains all downloaded files.
    #[cfg(any(feature = "onnx", feature = "mlx", feature = "qwen3-tts"))]
    async fn download_from_hub(&self, model_id: &str) -> AudioResult<PathBuf> {
        let model_id_owned = model_id.to_string();

        tracing::info!(model_id, "downloading model from HuggingFace Hub (first run)");

        let model_dir = tokio::task::spawn_blocking(move || Self::download_sync(&model_id_owned))
            .await
            .map_err(|e| AudioError::ModelDownload {
                model_id: model_id.to_string(),
                message: format!("download task panicked: {e}"),
            })??;

        tracing::info!(
            model_id,
            path = %model_dir.display(),
            "model download complete"
        );

        Ok(model_dir)
    }

    /// Synchronous download implementation using hf-hub.
    #[cfg(any(feature = "onnx", feature = "mlx", feature = "qwen3-tts"))]
    fn download_sync(model_id: &str) -> AudioResult<PathBuf> {
        use hf_hub::api::sync::Api;

        let api = Api::new().map_err(|e| AudioError::ModelDownload {
            model_id: model_id.to_string(),
            message: format!("failed to create HuggingFace API client: {e}"),
        })?;

        let repo = api.model(model_id.to_string());

        // Fetch the repo info to discover all files
        let repo_info = repo.info().map_err(|e| AudioError::ModelDownload {
            model_id: model_id.to_string(),
            message: format!("failed to fetch repo info: {e}"),
        })?;

        let siblings = repo_info.siblings;
        if siblings.is_empty() {
            return Err(AudioError::ModelDownload {
                model_id: model_id.to_string(),
                message: "repository has no files".into(),
            });
        }

        tracing::info!(model_id, file_count = siblings.len(), "downloading model files");

        // Download each file — hf-hub handles caching and deduplication
        let mut last_path: Option<PathBuf> = None;
        for sibling in &siblings {
            let filename = &sibling.rfilename;

            // Skip very large files that aren't needed for inference
            // (e.g. .git files, READMEs are fine to download)
            if filename.starts_with(".git") {
                continue;
            }

            tracing::debug!(model_id, file = %filename, "downloading");
            let path = repo.get(filename).map_err(|e| AudioError::ModelDownload {
                model_id: model_id.to_string(),
                message: format!("failed to download {filename}: {e}"),
            })?;
            last_path = Some(path);
        }

        // The model directory is the snapshot root.
        // hf-hub stores files under <cache>/models--<org>--<name>/snapshots/<rev>/
        // Files may be in subdirectories (e.g. onnx/), so we walk up from the
        // last downloaded file to find the snapshot root (the directory whose
        // parent is named "snapshots").
        let model_dir =
            last_path.as_ref().and_then(|p| Self::find_snapshot_root(p)).ok_or_else(|| {
                AudioError::ModelDownload {
                    model_id: model_id.to_string(),
                    message: "could not determine model directory from downloaded files".into(),
                }
            })?;

        Ok(model_dir)
    }

    /// Fallback when hf-hub is not available.
    #[cfg(not(any(feature = "onnx", feature = "mlx", feature = "qwen3-tts")))]
    async fn download_from_hub(&self, model_id: &str) -> AudioResult<PathBuf> {
        let local_path = self.cache_dir.join(model_id.replace('/', "--"));
        Err(AudioError::ModelDownload {
            model_id: model_id.to_string(),
            message: format!(
                "model not cached and hf-hub feature not enabled. \
                 Either enable the `onnx` or `mlx` feature, or manually place \
                 model files at: {}",
                local_path.display()
            ),
        })
    }

    /// Walk up from a file path to find the HuggingFace snapshot root directory.
    ///
    /// The snapshot root is the directory whose parent is named `"snapshots"`.
    /// For example, given a path like:
    /// `~/.cache/huggingface/hub/models--org--name/snapshots/abc123/onnx/model.onnx`
    /// this returns `~/.cache/huggingface/hub/models--org--name/snapshots/abc123/`.
    ///
    /// Falls back to the immediate parent if no `snapshots` ancestor is found
    /// (e.g. for locally cached models not from HuggingFace Hub).
    #[cfg(any(feature = "onnx", feature = "mlx", feature = "qwen3-tts"))]
    fn find_snapshot_root(file_path: &std::path::Path) -> Option<PathBuf> {
        let mut current = file_path.parent()?;
        loop {
            if let Some(parent) = current.parent() {
                if parent.file_name().and_then(|n| n.to_str()) == Some("snapshots") {
                    return Some(current.to_path_buf());
                }
                current = parent;
            } else {
                // No "snapshots" ancestor found — fall back to immediate parent
                return file_path.parent().map(|p| p.to_path_buf());
            }
        }
    }
}

/// Get the user's cache directory, falling back to current directory.
fn dirs_cache_dir() -> PathBuf {
    // Use HOME env var on macOS/Linux
    std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".cache"))
        .unwrap_or_else(|_| PathBuf::from(".cache"))
}
