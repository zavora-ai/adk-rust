//! What a verifier remembers per domain: the pinned root, the last verified checkpoint and
//! the revocations it has seen (spec 3.5 §7.7; AWP 2.0 §15.3).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// A verifier's pinned state for one domain (AWP 2.0 §15.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pin {
    /// The pinned root key's id.
    pub root_kid: String,
    /// The pinned root key's RFC 7638 thumbprint.
    pub root_thumbprint: String,
    /// The last verified checkpoint's tree size.
    pub tree_size: u64,
    /// The last verified checkpoint's root hash, hex.
    pub root_hash: String,
    /// Key ids seen revoked; a later key set that drops a revocation does not revive a key.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub revoked: BTreeSet<String>,
}

/// Why pinned state could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the verifier's pinned state failed: {0}")]
pub struct PinError(pub String);

/// Per-domain pinned state (AWP 2.0 §15.3).
#[async_trait]
pub trait PinStore: Send + Sync {
    /// Returns the pin for `domain`.
    async fn get(&self, domain: &str) -> Result<Option<Pin>, PinError>;
    /// Replaces the pin for `domain`.
    async fn put(&self, domain: &str, pin: Pin) -> Result<(), PinError>;
}

/// In-memory pins.
#[derive(Debug, Clone, Default)]
pub struct InMemoryPinStore {
    pins: Arc<RwLock<BTreeMap<String, Pin>>>,
}

#[async_trait]
impl PinStore for InMemoryPinStore {
    async fn get(&self, domain: &str) -> Result<Option<Pin>, PinError> {
        Ok(self.pins.read().await.get(domain).cloned())
    }

    async fn put(&self, domain: &str, pin: Pin) -> Result<(), PinError> {
        self.pins.write().await.insert(domain.to_owned(), pin);
        Ok(())
    }
}

/// Pins kept in one JSON file, written atomically: what `adk-rust awp verify` uses.
#[derive(Debug)]
pub struct FilePinStore {
    path: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

impl FilePinStore {
    /// Pins stored at `path`; the file is created on the first write.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: tokio::sync::Mutex::new(()) }
    }

    async fn read_all(&self) -> Result<BTreeMap<String, Pin>, PinError> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|error| PinError(error.to_string()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(PinError(error.to_string())),
        }
    }
}

#[async_trait]
impl PinStore for FilePinStore {
    async fn get(&self, domain: &str) -> Result<Option<Pin>, PinError> {
        let _guard = self.lock.lock().await;
        Ok(self.read_all().await?.remove(domain))
    }

    async fn put(&self, domain: &str, pin: Pin) -> Result<(), PinError> {
        let _guard = self.lock.lock().await;
        let mut pins = self.read_all().await?;
        pins.insert(domain.to_owned(), pin);
        let bytes =
            serde_json::to_vec_pretty(&pins).map_err(|error| PinError(error.to_string()))?;
        if let Some(parent) = self.path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await.map_err(|error| PinError(error.to_string()))?;
        }
        let temp = self.path.with_extension("tmp");
        tokio::fs::write(&temp, bytes).await.map_err(|error| PinError(error.to_string()))?;
        tokio::fs::rename(&temp, &self.path).await.map_err(|error| PinError(error.to_string()))
    }
}
