//! Content history: every saved version of every item (spec 3.5 §7.8.2, §7.8.3 "Undo per
//! item").
//!
//! A git-backed repository keeps history in git; [`ContentHistory`] is the narrow record the
//! pipeline needs without one: each version, who saved it, and the release it went live in,
//! so `site revert <item> --to <seq>` finds the version that release served.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use awp_types::{ContentId, ContentItem};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::{Result, SiteError};

/// Who saved a version and why.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryMeta {
    /// When it was saved.
    pub at: Option<DateTime<Utc>>,
    /// The principal who saved it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// The release this version went live in, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_seq: Option<u64>,
    /// A note, such as a correction's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One saved version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    /// The version number, from 1.
    pub version: u64,
    /// Who saved it and why.
    pub meta: HistoryMeta,
    /// The item as saved.
    pub item: ContentItem,
}

/// Append-only versions of content items. Implementations run
/// [`content_history_conformance`].
#[async_trait]
pub trait ContentHistory: Send + Sync {
    /// Records a new version of `item` and returns its version number.
    ///
    /// # Errors
    ///
    /// Returns an error when the history cannot be written.
    async fn record(&self, item: &ContentItem, meta: HistoryMeta) -> Result<u64>;

    /// Every version of `id`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the history cannot be read.
    async fn versions(&self, id: &ContentId) -> Result<Vec<HistoryEntry>>;

    /// The version that release `seq` served: the newest version released at or before it.
    ///
    /// # Errors
    ///
    /// Returns an error when the history cannot be read.
    async fn at_release(&self, id: &ContentId, seq: u64) -> Result<Option<HistoryEntry>> {
        Ok(self
            .versions(id)
            .await?
            .into_iter()
            .filter(|entry| entry.meta.release_seq.is_some_and(|released| released <= seq))
            .max_by_key(|entry| (entry.meta.release_seq, entry.version)))
    }

    /// The version released before the newest released one: what a revert restores.
    ///
    /// # Errors
    ///
    /// Returns an error when the history cannot be read.
    async fn previous_release(&self, id: &ContentId) -> Result<Option<HistoryEntry>> {
        let mut released: Vec<HistoryEntry> =
            self.versions(id).await?.into_iter().filter(|e| e.meta.release_seq.is_some()).collect();
        released.sort_by_key(|e| (e.meta.release_seq, e.version));
        released.pop();
        Ok(released.pop())
    }
}

/// History in memory.
#[derive(Debug, Default, Clone)]
pub struct InMemoryContentHistory {
    entries: Arc<RwLock<BTreeMap<ContentId, Vec<HistoryEntry>>>>,
}

#[async_trait]
impl ContentHistory for InMemoryContentHistory {
    async fn record(&self, item: &ContentItem, meta: HistoryMeta) -> Result<u64> {
        let mut entries = self.entries.write().await;
        let list = entries.entry(item.id.clone()).or_default();
        let version = list.len() as u64 + 1;
        list.push(HistoryEntry { version, meta, item: item.clone() });
        Ok(version)
    }

    async fn versions(&self, id: &ContentId) -> Result<Vec<HistoryEntry>> {
        Ok(self.entries.read().await.get(id).cloned().unwrap_or_default())
    }
}

/// History as files: `<dir>/<hex of id>/<version>.json`, each written once.
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::content::{ContentHistory, FileContentHistory};
///
/// # async fn demo() -> adk_site::Result<()> {
/// let history = FileContentHistory::new("./my-site/.history");
/// let versions = history.versions(&"posts/hello/en".into()).await?;
/// # let _ = versions;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct FileContentHistory {
    dir: PathBuf,
    lock: Arc<RwLock<()>>,
}

impl FileContentHistory {
    /// History under `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), lock: Arc::new(RwLock::new(())) }
    }

    fn item_dir(&self, id: &ContentId) -> PathBuf {
        self.dir.join(hex::encode(id.0.as_bytes()))
    }
}

#[async_trait]
impl ContentHistory for FileContentHistory {
    async fn record(&self, item: &ContentItem, meta: HistoryMeta) -> Result<u64> {
        let _guard = self.lock.write().await;
        let dir = self.item_dir(&item.id);
        let item = item.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir).map_err(|e| SiteError::io(&dir, e))?;
            let count = std::fs::read_dir(&dir).map_err(|e| SiteError::io(&dir, e))?.count() as u64;
            let version = count + 1;
            let entry = HistoryEntry { version, meta, item };
            let text = serde_json::to_vec_pretty(&entry)
                .map_err(|e| SiteError::serde("history entry", e))?;
            let path = dir.join(format!("{version:010}.json"));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| SiteError::io(&path, e))?;
            std::io::Write::write_all(&mut file, &text).map_err(|e| SiteError::io(&path, e))?;
            file.sync_data().map_err(|e| SiteError::io(&path, e))?;
            Ok(version)
        })
        .await
        .map_err(|e| SiteError::Pipeline(format!("history task failed: {e}")))?
    }

    async fn versions(&self, id: &ContentId) -> Result<Vec<HistoryEntry>> {
        let _guard = self.lock.read().await;
        let dir = self.item_dir(id);
        tokio::task::spawn_blocking(move || {
            if !dir.exists() {
                return Ok(Vec::new());
            }
            let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
                .map_err(|e| SiteError::io(&dir, e))?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                .collect();
            paths.sort();
            paths
                .into_iter()
                .map(|path| {
                    let bytes = std::fs::read(&path).map_err(|e| SiteError::io(&path, e))?;
                    serde_json::from_slice(&bytes)
                        .map_err(|e| SiteError::serde(path.display().to_string(), e))
                })
                .collect()
        })
        .await
        .map_err(|e| SiteError::Pipeline(format!("history task failed: {e}")))?
    }
}

/// The conformance suite every [`ContentHistory`] runs.
///
/// # Panics
///
/// Panics with the failed expectation.
pub async fn content_history_conformance(history: &dyn ContentHistory, sample: ContentItem) {
    let id = sample.id.clone();
    assert!(history.versions(&id).await.unwrap().is_empty());
    let mut v1 = sample.clone();
    v1.fields.insert("price".into(), 100.into());
    let mut v2 = sample.clone();
    v2.fields.insert("price".into(), 110.into());
    let mut v3 = sample;
    v3.fields.insert("price".into(), 120.into());
    let released = |seq| HistoryMeta { release_seq: Some(seq), ..HistoryMeta::default() };
    assert_eq!(history.record(&v1, released(3)).await.unwrap(), 1);
    assert_eq!(history.record(&v2, HistoryMeta::default()).await.unwrap(), 2, "a draft");
    assert_eq!(history.record(&v2, released(7)).await.unwrap(), 3);
    assert_eq!(history.record(&v3, released(9)).await.unwrap(), 4);
    let versions = history.versions(&id).await.unwrap();
    assert_eq!(versions.iter().map(|v| v.version).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    assert_eq!(versions[0].item, v1, "versions keep the item as saved");
    assert_eq!(history.at_release(&id, 2).await.unwrap(), None);
    assert_eq!(history.at_release(&id, 5).await.unwrap().unwrap().item, v1);
    assert_eq!(history.at_release(&id, 8).await.unwrap().unwrap().item, v2);
    assert_eq!(history.previous_release(&id).await.unwrap().unwrap().item, v2);
    assert!(history.versions(&"other".into()).await.unwrap().is_empty());
}
