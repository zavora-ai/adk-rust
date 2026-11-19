//! The model fetcher (spec 3.7 §7.4), feature `fetch`.
//!
//! Downloads from Hugging Face by revision only, through `hf-hub`'s cache layout so other
//! tools share it, and checks every file's SHA-256 after download and before each load.
//! With `HF_HUB_OFFLINE=1` it opens no socket: a cached file is checked and returned, a
//! missing one is an error.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hf_hub::api::sync::{ApiBuilder, ApiError};
use hf_hub::{Cache, Repo, RepoType};

use crate::error::{Result, VisionError};
use crate::manifest::{
    AcceptanceStore, LicenceGate, LicencePolicy, ModelManifest, ModelSource, VerifiedFile,
    VerifiedModel,
};

const DEFAULT_ENDPOINT: &str = "https://huggingface.co";

/// Where and how the fetcher reads and downloads.
///
/// # Example
///
/// ```
/// use std::collections::HashMap;
/// use adk_vision::FetchConfig;
///
/// let env: HashMap<&str, &str> =
///     [("HF_HOME", "/data/hf"), ("HF_HUB_OFFLINE", "1"), ("HF_TOKEN", "hf_x")].into();
/// let config = FetchConfig::from_lookup(|key| env.get(key).map(|v| v.to_string()));
/// assert_eq!(config.cache_dir, std::path::PathBuf::from("/data/hf/hub"));
/// assert!(config.offline);
/// assert_eq!(config.token.as_deref(), Some("hf_x"));
/// ```
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FetchConfig {
    /// The hub cache: `HF_HUB_CACHE`, else `$HF_HOME/hub`, else `~/.cache/huggingface/hub`.
    pub cache_dir: PathBuf,
    /// The hub endpoint: `HF_ENDPOINT`, else `https://huggingface.co`.
    pub endpoint: String,
    /// The access token: `HF_TOKEN`.
    pub token: Option<String>,
    /// `HF_HUB_OFFLINE` is `1`, `true`, `yes` or `on`: no socket is opened.
    pub offline: bool,
}

impl std::fmt::Debug for FetchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchConfig")
            .field("cache_dir", &self.cache_dir)
            .field("endpoint", &self.endpoint)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("offline", &self.offline)
            .finish()
    }
}

impl FetchConfig {
    /// The configuration from the process environment, by Python's `huggingface_hub`
    /// conventions; the `hf-hub` crate reads none of them itself.
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// The configuration from a variable lookup.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let set = |key: &str| lookup(key).filter(|v| !v.trim().is_empty());
        let cache_dir = set("HF_HUB_CACHE").map(PathBuf::from).unwrap_or_else(|| {
            set("HF_HOME").map(|home| PathBuf::from(home).join("hub")).unwrap_or_else(|| {
                std::env::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".cache")
                    .join("huggingface")
                    .join("hub")
            })
        });
        let offline = set("HF_HUB_OFFLINE").is_some_and(|v| {
            matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
        });
        Self {
            cache_dir,
            endpoint: set("HF_ENDPOINT").unwrap_or_else(|| DEFAULT_ENDPOINT.to_string()),
            token: set("HF_TOKEN"),
            offline,
        }
    }

    /// Uses `dir` as the hub cache.
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cache_dir = dir.into();
        self
    }

    /// Downloads from `endpoint`.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Sets offline mode.
    pub fn with_offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    /// Sets the access token.
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
    }
}

/// Fetches a model's files by revision and verifies them.
///
/// The licence gate runs before any request: a use-restricted model needs a person's
/// recorded acceptance, and a non-commercial or copyleft one a policy that names it. A
/// 401 or 403 from the hub is a gated repository and becomes
/// [`VisionError::LicenceAcceptanceRequired`] naming the repository page.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_vision::{FetchConfig, InMemoryAcceptanceStore, ManifestSet, ModelFetcher};
///
/// # async fn demo(manifests: ManifestSet) -> adk_vision::Result<()> {
/// let fetcher = ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
/// let model = fetcher.fetch(manifests.get("rf-detr-n-q4f16").unwrap()).await?;
/// for file in model.files() {
///     println!("{} {}", file.path().display(), file.digest());
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ModelFetcher {
    config: FetchConfig,
    policy: LicencePolicy,
    acceptances: Arc<dyn AcceptanceStore>,
}

impl std::fmt::Debug for ModelFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelFetcher")
            .field("config", &self.config)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl ModelFetcher {
    /// A fetcher with a configuration and a licence acceptance store.
    pub fn new(config: FetchConfig, acceptances: Arc<dyn AcceptanceStore>) -> Self {
        Self { config, policy: LicencePolicy::new(), acceptances }
    }

    /// Allows the non-commercial and copyleft models the policy names.
    pub fn with_policy(mut self, policy: LicencePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The configuration.
    pub fn config(&self) -> &FetchConfig {
        &self.config
    }

    /// Fetches and verifies on a blocking thread. See [`fetch_blocking`](Self::fetch_blocking).
    ///
    /// # Errors
    ///
    /// As [`fetch_blocking`](Self::fetch_blocking).
    pub async fn fetch(&self, manifest: &ModelManifest) -> Result<VerifiedModel> {
        let fetcher = self.clone();
        let manifest = manifest.clone();
        tokio::task::spawn_blocking(move || fetcher.fetch_blocking(&manifest)).await.map_err(
            |e| VisionError::Backend {
                backend: "fetch".into(),
                message: format!("fetch task failed: {e}"),
            },
        )?
    }

    /// Fetches every file of `manifest` that is not cached and verifies every file.
    ///
    /// # Errors
    ///
    /// The licence gate's refusal before any request; [`VisionError::Offline`] for a file
    /// missing from the cache in offline mode; [`VisionError::LicenceAcceptanceRequired`]
    /// for a 401 or 403; [`VisionError::Fetch`] for other download failures;
    /// [`VisionError::DigestMismatch`] or [`VisionError::SizeMismatch`] for a file that does
    /// not match; [`VisionError::Unsupported`] for ModelScope and URL sources, which arrive
    /// with the backends that need them.
    pub fn fetch_blocking(&self, manifest: &ModelManifest) -> Result<VerifiedModel> {
        manifest.validate()?;
        let gate = LicenceGate::new(&self.policy, self.acceptances.as_ref());
        gate.check(manifest)?;
        match &manifest.source {
            ModelSource::Local { dir } => VerifiedModel::check_dir(manifest, dir, &gate),
            ModelSource::HuggingFace { repo, revision } => {
                let mut files = Vec::with_capacity(manifest.files.len());
                for file in &manifest.files {
                    let path = self.hugging_face_file(manifest, repo, revision, &file.path)?;
                    files.push((file.path.clone(), VerifiedFile::check(&path, file)?));
                }
                tracing::info!(model.id = %manifest.id, model.revision = %revision, "model verified");
                Ok(VerifiedModel::new(manifest.clone(), files))
            }
            ModelSource::ModelScope { .. } | ModelSource::Url { .. } => {
                Err(VisionError::Unsupported(format!(
                    "model `{}`: ModelScope and URL sources arrive with the backends that need them; \
                     download the files and use a `local` source",
                    manifest.id
                )))
            }
        }
    }

    /// The cached path of a Hugging Face file, if it is in the cache. Opens no socket.
    pub fn cached(&self, repo: &str, revision: &str, file: &str) -> Option<PathBuf> {
        let repo = Repo::with_revision(repo.to_string(), RepoType::Model, revision.to_string());
        Cache::new(self.config.cache_dir.clone()).repo(repo).get(file)
    }

    fn hugging_face_file(
        &self,
        manifest: &ModelManifest,
        repo: &str,
        revision: &str,
        file: &str,
    ) -> Result<PathBuf> {
        if let Some(path) = self.cached(repo, revision, file) {
            tracing::debug!(model.id = %manifest.id, model.file = file, "model file cached");
            return Ok(path);
        }
        if self.config.offline {
            return Err(VisionError::Offline {
                model: manifest.id.clone(),
                file: file.to_string(),
            });
        }
        let fetch_error = |message: String| VisionError::Fetch {
            model: manifest.id.clone(),
            file: file.to_string(),
            message,
        };
        let api = ApiBuilder::from_cache(Cache::new(self.config.cache_dir.clone()))
            .with_endpoint(self.config.endpoint.clone())
            .with_token(self.config.token.clone())
            .with_progress(false)
            .with_user_agent("adk-vision", env!("CARGO_PKG_VERSION"))
            .build()
            .map_err(|e| fetch_error(format!("the hub client failed to start: {e}")))?;
        let hub_repo = Repo::with_revision(repo.to_string(), RepoType::Model, revision.to_string());
        tracing::info!(model.id = %manifest.id, model.file = file, model.revision = revision, "fetching model file");
        let path = api.repo(hub_repo).download(file).map_err(|err| match status_of(&err) {
            Some(401 | 403) => VisionError::LicenceAcceptanceRequired {
                model: manifest.id.clone(),
                licence_url: format!("{}/{repo}", self.config.endpoint.trim_end_matches('/')),
            },
            Some(404) => fetch_error(format!("not found at revision {revision}")),
            _ => fetch_error(err.to_string()),
        })?;
        if !in_snapshot(&path, revision) {
            return Err(fetch_error(format!(
                "the hub answered a different commit than revision {revision}"
            )));
        }
        Ok(path)
    }
}

fn status_of(err: &ApiError) -> Option<u16> {
    match err {
        ApiError::RequestError(inner) => match inner.as_ref() {
            ureq::Error::StatusCode(code) => Some(*code),
            _ => None,
        },
        ApiError::TooManyRetries(inner) => status_of(inner),
        _ => None,
    }
}

fn in_snapshot(path: &Path, revision: &str) -> bool {
    path.components().any(|c| c.as_os_str() == revision)
}
