use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum CatalogSource {
    Embedded,
    Local(PathBuf),
    Remote(String),
}

#[derive(Debug, Clone)]
pub struct CatalogArtifact {
    pub catalog_id: String,
    pub catalog: Value,
    pub metadata: Option<Value>,
    pub source: CatalogSource,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog not found: {0}")]
    NotFound(String),
    #[error("catalog IO error: {0}")]
    Io(String),
    #[error("catalog JSON error: {0}")]
    Json(String),
    #[error("remote catalogs disabled")]
    RemoteDisabled,
    #[error("remote catalog error: {0}")]
    Remote(String),
}

#[derive(Debug, Clone)]
enum CatalogEntry {
    Embedded { catalog: &'static str, metadata: Option<&'static str> },
    File { catalog_path: PathBuf, metadata_path: Option<PathBuf> },
}

#[derive(Debug, Clone)]
pub struct CatalogRegistry {
    default_catalog_id: String,
    entries: HashMap<String, CatalogEntry>,
    remote_base_url: Option<String>,
}

impl Default for CatalogRegistry {
    fn default() -> Self {
        let mut registry = Self {
            default_catalog_id: DEFAULT_CATALOG_ID.to_string(),
            entries: HashMap::new(),
            remote_base_url: None,
        };

        registry.register_embedded(
            DEFAULT_CATALOG_ID,
            include_str!("../catalog/extended_catalog.json"),
            Some(include_str!("../catalog/metadata.json")),
        );

        registry
    }
}

impl CatalogRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_remote_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.remote_base_url = Some(base_url.into());
        self
    }

    pub fn register_local(
        &mut self,
        catalog_id: impl Into<String>,
        catalog_path: impl Into<PathBuf>,
        metadata_path: Option<PathBuf>,
    ) -> &mut Self {
        self.entries.insert(
            catalog_id.into(),
            CatalogEntry::File { catalog_path: catalog_path.into(), metadata_path },
        );
        self
    }

    pub fn register_embedded(
        &mut self,
        catalog_id: impl Into<String>,
        catalog: &'static str,
        metadata: Option<&'static str>,
    ) -> &mut Self {
        self.entries.insert(catalog_id.into(), CatalogEntry::Embedded { catalog, metadata });
        self
    }

    pub fn default_catalog_id(&self) -> &str {
        &self.default_catalog_id
    }

    pub fn load_local_catalog(&self, catalog_id: &str) -> Result<CatalogArtifact, CatalogError> {
        let entry = self
            .entries
            .get(catalog_id)
            .ok_or_else(|| CatalogError::NotFound(catalog_id.to_string()))?;

        match entry {
            CatalogEntry::Embedded { catalog, metadata } => {
                let catalog_value: Value =
                    serde_json::from_str(catalog).map_err(|e| CatalogError::Json(e.to_string()))?;
                let metadata_value = metadata.and_then(|raw| serde_json::from_str(raw).ok());
                Ok(CatalogArtifact {
                    catalog_id: catalog_id.to_string(),
                    catalog: catalog_value,
                    metadata: metadata_value,
                    source: CatalogSource::Embedded,
                })
            }
            CatalogEntry::File { catalog_path, metadata_path } => {
                let raw = std::fs::read_to_string(catalog_path)
                    .map_err(|e| CatalogError::Io(e.to_string()))?;
                let catalog_value: Value =
                    serde_json::from_str(&raw).map_err(|e| CatalogError::Json(e.to_string()))?;

                let metadata_value = match metadata_path {
                    Some(path) => std::fs::read_to_string(path)
                        .ok()
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    None => None,
                };

                Ok(CatalogArtifact {
                    catalog_id: catalog_id.to_string(),
                    catalog: catalog_value,
                    metadata: metadata_value,
                    source: CatalogSource::Local(catalog_path.clone()),
                })
            }
        }
    }

    pub async fn resolve_catalog(&self, catalog_id: &str) -> Result<CatalogArtifact, CatalogError> {
        if let Ok(local) = self.load_local_catalog(catalog_id) {
            return Ok(local);
        }

        let url = if catalog_id.starts_with("http://") || catalog_id.starts_with("https://") {
            Some(catalog_id.to_string())
        } else {
            self.remote_base_url
                .as_ref()
                .map(|base| format!("{}/{}", base.trim_end_matches('/'), catalog_id))
        };

        if let Some(url) = url {
            return self.fetch_remote_catalog(&url).await;
        }

        Err(CatalogError::NotFound(catalog_id.to_string()))
    }

    async fn fetch_remote_catalog(&self, url: &str) -> Result<CatalogArtifact, CatalogError> {
        fetch_remote_catalog(url).await
    }
}

const DEFAULT_CATALOG_ID: &str = "zavora.ai:adk-ui/extended@0.2.0";

#[cfg(feature = "remote-catalogs")]
async fn fetch_remote_catalog(url: &str) -> Result<CatalogArtifact, CatalogError> {
    let response = reqwest::get(url).await.map_err(|e| CatalogError::Remote(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(CatalogError::Remote(format!("HTTP {}", status)));
    }
    let text = response.text().await.map_err(|e| CatalogError::Remote(e.to_string()))?;
    let catalog: Value =
        serde_json::from_str(&text).map_err(|e| CatalogError::Json(e.to_string()))?;

    Ok(CatalogArtifact {
        catalog_id: url.to_string(),
        catalog,
        metadata: None,
        source: CatalogSource::Remote(url.to_string()),
    })
}

#[cfg(not(feature = "remote-catalogs"))]
async fn fetch_remote_catalog(_url: &str) -> Result<CatalogArtifact, CatalogError> {
    Err(CatalogError::RemoteDisabled)
}
