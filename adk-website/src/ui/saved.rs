//! What the desk keeps across a restart (spec 3.5 §8.2, §7.18 "every step is a checkpoint"):
//! the device the setup code paired, the onboarding answers, the charter under review and the
//! web editor's drafts.
//!
//! A desk built with [`WebsiteDeskBuilder::store`](super::WebsiteDeskBuilder::store) saves
//! after every command a person takes and [`WebsiteDesk::restore`] reads it back, so the owner
//! leaves onboarding on one day and resumes it on another, after the site restarted.
//! [`FileDeskStore`] keeps it in one JSON file readable by its owner only, such as
//! `data/desk.json`; previews and gate reports are rebuilt, not kept.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::{AdkError, Result};
use adk_kernel::charter::{CharterVersion, RoleApproval};
use adk_site::pipeline::EntryPoint;
use async_trait::async_trait;
use awp_types::{Charter, ContentId, ContentItem, SiteKind};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::FormValues;
use super::WebsiteDesk;
use crate::kinds::KindProposal;
use crate::onboarding::discovery::{DesignChoice, OwnerCard};
use crate::onboarding::{Stage, StartingPoint};

/// The desk's saved state; opaque, so a [`DeskStore`] only keeps it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedDesk {
    #[serde(default)]
    pub(crate) paired: Option<String>,
    #[serde(default)]
    pub(crate) onboarding: SavedOnboarding,
    #[serde(default)]
    pub(crate) charter: SavedCharter,
    #[serde(default)]
    pub(crate) drafts: Vec<SavedDraft>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedOnboarding {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) start: Option<StartingPoint>,
    #[serde(default)]
    pub(crate) phone: Option<String>,
    #[serde(default)]
    pub(crate) done: BTreeSet<Stage>,
    #[serde(default)]
    pub(crate) found: Option<SavedFound>,
    #[serde(default)]
    pub(crate) kind: Option<SiteKind>,
    #[serde(default)]
    pub(crate) protect: Option<bool>,
    #[serde(default)]
    pub(crate) decisions: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) answers: BTreeMap<String, FormValues>,
    #[serde(default)]
    pub(crate) answer_facts: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub(crate) design: Option<DesignChoice>,
    #[serde(default)]
    pub(crate) built: Option<(Vec<String>, String)>,
    #[serde(default)]
    pub(crate) skipped: BTreeSet<String>,
    #[serde(default)]
    pub(crate) live: Option<(u64, DateTime<Utc>)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedFound {
    pub(crate) source: Option<String>,
    pub(crate) facts: usize,
    pub(crate) pages: usize,
    pub(crate) items: usize,
    pub(crate) legacy: bool,
    pub(crate) proposal: Option<KindProposal>,
    pub(crate) cards: Vec<OwnerCard>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedCharter {
    #[serde(default)]
    pub(crate) draft: Option<Charter>,
    #[serde(default)]
    pub(crate) reviewed: BTreeSet<String>,
    #[serde(default)]
    pub(crate) signatures: Vec<RoleApproval>,
    #[serde(default)]
    pub(crate) committed: Option<CharterVersion>,
}

/// One web editor draft: its items, its media as base64, and where it stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedDraft {
    pub(crate) id: String,
    pub(crate) items: Vec<ContentItem>,
    #[serde(default)]
    pub(crate) media: Vec<(String, String)>,
    pub(crate) entry: EntryPoint,
    pub(crate) proposer: String,
    pub(crate) job: String,
    pub(crate) action_class: String,
    #[serde(default)]
    pub(crate) changes: Option<ContentId>,
    #[serde(default)]
    pub(crate) outcome: Option<String>,
    #[serde(default)]
    pub(crate) done: bool,
    #[serde(default)]
    pub(crate) scheduled: bool,
}

impl SavedDraft {
    pub(crate) fn encode(media: &[(String, bytes::Bytes)]) -> Vec<(String, String)> {
        media.iter().map(|(path, bytes)| (path.clone(), STANDARD.encode(bytes))).collect()
    }

    pub(crate) fn decode(&self) -> Result<Vec<(String, bytes::Bytes)>> {
        self.media
            .iter()
            .map(|(path, data)| {
                STANDARD.decode(data.as_bytes()).map(|bytes| (path.clone(), bytes.into())).map_err(
                    |error| {
                        AdkError::config(format!(
                            "the saved draft {} holds media {path} that does not decode: {error}",
                            self.id
                        ))
                    },
                )
            })
            .collect()
    }
}

/// Where a desk keeps [`SavedDesk`] (spec 3.5 §8.2).
///
/// # Example
///
/// ```rust
/// use adk_website::ui::{DeskStore, InMemoryDeskStore, SavedDesk};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let store = InMemoryDeskStore::default();
/// assert!(store.load().await.unwrap().is_none());
/// store.save(&SavedDesk::default()).await.unwrap();
/// assert_eq!(store.load().await.unwrap(), Some(SavedDesk::default()));
/// # }
/// ```
#[async_trait]
pub trait DeskStore: Send + Sync {
    /// The saved desk, if one was saved.
    ///
    /// # Errors
    ///
    /// The store's error, or a saved desk that does not read.
    async fn load(&self) -> Result<Option<SavedDesk>>;

    /// Replaces the saved desk with `desk`.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn save(&self, desk: &SavedDesk) -> Result<()>;
}

/// A [`DeskStore`] in memory, for tests.
#[derive(Debug, Default)]
pub struct InMemoryDeskStore {
    saved: Mutex<Option<SavedDesk>>,
}

#[async_trait]
impl DeskStore for InMemoryDeskStore {
    async fn load(&self) -> Result<Option<SavedDesk>> {
        Ok(self.saved.lock().await.clone())
    }

    async fn save(&self, desk: &SavedDesk) -> Result<()> {
        *self.saved.lock().await = Some(desk.clone());
        Ok(())
    }
}

/// A [`DeskStore`] in one JSON file, written whole through a temporary file and readable by
/// its owner only: it holds the owner's onboarding answers.
#[derive(Debug, Clone)]
pub struct FileDeskStore {
    path: PathBuf,
}

impl FileDeskStore {
    /// The desk saved at `path`, such as the project's `data/desk.json`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn io(path: &Path, error: impl std::fmt::Display) -> AdkError {
    AdkError::config(format!("the desk's saved state at {}: {error}", path.display()))
}

#[async_trait]
impl DeskStore for FileDeskStore {
    async fn load(&self) -> Result<Option<SavedDesk>> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| io(&self.path, e)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io(&self.path, error)),
        }
    }

    async fn save(&self, desk: &SavedDesk) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(desk).map_err(|e| io(&self.path, e))?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(&temporary, &path)
        })
        .await
        .map_err(|e| io(&self.path, e))?
        .map_err(|e| io(&self.path, e))
    }
}

impl WebsiteDesk {
    /// Reads back what the desk's store saved (spec 3.5 §8.2): the paired device, the
    /// onboarding answers, the charter under review and the editor's drafts. Returns whether
    /// anything was saved. Call it once, after building the desk.
    ///
    /// # Errors
    ///
    /// The store's error, or a saved draft whose media does not decode.
    pub async fn restore(&self) -> Result<bool> {
        let Some(store) = &self.inner.store else { return Ok(false) };
        let Some(saved) = store.load().await? else { return Ok(false) };
        *self.inner.paired.lock().await = saved.paired;
        self.restore_onboarding(saved.onboarding).await;
        self.restore_charter(saved.charter).await;
        self.restore_drafts(saved.drafts).await?;
        Ok(true)
    }

    /// Saves the desk to its store, when it has one; a failure is logged, since the person's
    /// command itself succeeded.
    pub(crate) async fn persist(&self) {
        let Some(store) = self.inner.store.clone() else { return };
        let saved = SavedDesk {
            paired: self.inner.paired.lock().await.clone(),
            onboarding: self.save_onboarding().await,
            charter: self.save_charter().await,
            drafts: self.save_drafts().await,
        };
        if let Err(error) = store.save(&saved).await {
            tracing::warn!(error = %error, website.site = %self.inner.site.id, "the desk's state was not saved");
        }
    }
}

/// The store a desk saves to.
pub(crate) type SharedDeskStore = Arc<dyn DeskStore>;
