//! Where an [`UpdateEngine`](super::UpdateEngine) keeps what it holds for the owner, so held
//! items and held deliveries survive a restart (spec 3.5 §7.8.3 "Approvals on exceptions
//! only", A-I25, A-I27): the owner's decision on a card raised before a restart still finds
//! the change it decides.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::{HeldDelivery, HeldItem};
use crate::error::{Result, SiteError};

/// Everything an engine holds: items held alone, deliveries held whole, and the next
/// delivery number, so numbers are never reused across a restart.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldState {
    /// Items held alone, waiting for a decision.
    #[serde(default)]
    pub items: Vec<HeldItem>,
    /// Deliveries held whole by their bounds.
    #[serde(default)]
    pub deliveries: Vec<HeldDelivery>,
    /// The number the next delivery gets.
    #[serde(default)]
    pub next_delivery: u64,
}

/// A durable home for an engine's [`HeldState`]. The engine reads it once, before its first
/// use of held state, and writes the whole state after every change to it.
///
/// Implementations: [`InMemoryHeldStore`] (the default, lost on restart) and
/// [`FileHeldStore`]; [`held_store_conformance`] checks any of them.
#[async_trait]
pub trait HeldStore: Send + Sync {
    /// The state last saved, or the empty state.
    ///
    /// # Errors
    ///
    /// Returns an error when the state cannot be read.
    async fn load(&self) -> Result<HeldState>;

    /// Replaces the saved state with `state`.
    ///
    /// # Errors
    ///
    /// Returns an error when the state cannot be written.
    async fn save(&self, state: &HeldState) -> Result<()>;
}

/// Held state in memory: the engine's default, lost on restart.
#[derive(Debug, Default)]
pub struct InMemoryHeldStore(Mutex<HeldState>);

#[async_trait]
impl HeldStore for InMemoryHeldStore {
    async fn load(&self) -> Result<HeldState> {
        Ok(self.0.lock().await.clone())
    }

    async fn save(&self, state: &HeldState) -> Result<()> {
        *self.0.lock().await = state.clone();
        Ok(())
    }
}

/// Held state as one JSON file, replaced atomically on every save.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_site::updates::{FileHeldStore, UpdateEngine};
/// # fn demo(pipeline: Arc<adk_site::pipeline::PublishingPipeline>, live: Arc<dyn adk_site::live::LiveStore>) -> adk_site::Result<()> {
/// let engine = UpdateEngine::builder(pipeline, live)
///     .held_store(Arc::new(FileHeldStore::new("/var/lib/site/data/updates/held.json")))
///     .build()?;
/// # let _ = engine;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct FileHeldStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileHeldStore {
    /// A store at `path`, created on the first save.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Mutex::new(()) }
    }
}

#[async_trait]
impl HeldStore for FileHeldStore {
    async fn load(&self) -> Result<HeldState> {
        let _guard = self.lock.lock().await;
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| SiteError::serde(self.path.display().to_string(), e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HeldState::default()),
            Err(e) => Err(SiteError::io(&self.path, e)),
        }
    }

    async fn save(&self, state: &HeldState) -> Result<()> {
        let _guard = self.lock.lock().await;
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| SiteError::io(parent, e))?;
        }
        let bytes =
            serde_json::to_vec_pretty(state).map_err(|e| SiteError::serde("held updates", e))?;
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, bytes).await.map_err(|e| SiteError::io(&tmp, e))?;
        tokio::fs::rename(&tmp, &self.path).await.map_err(|e| SiteError::io(&self.path, e))
    }
}

/// Checks what every [`HeldStore`] promises: an empty store loads the empty state, a saved
/// state loads back equal, and a save replaces the previous one whole. Panics on a violation;
/// call it from a test with `sample`, a state holding at least one item and one delivery.
pub async fn held_store_conformance(store: &dyn HeldStore, sample: &HeldState) {
    assert_eq!(store.load().await.expect("load"), HeldState::default(), "a new store is empty");
    store.save(sample).await.expect("save");
    assert_eq!(&store.load().await.expect("load"), sample, "the saved state loads back");
    let mut smaller = sample.clone();
    smaller.items.clear();
    smaller.next_delivery = sample.next_delivery + 1;
    store.save(&smaller).await.expect("save");
    assert_eq!(store.load().await.expect("load"), smaller, "a save replaces the state whole");
}
