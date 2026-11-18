//! Licence acceptance and the [`LicenceGate`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{LicenceClass, ModelManifest};
use crate::digest::Digest;
use crate::error::{Result, VisionError};

/// A person's recorded acceptance of a use-restricted model's licence.
///
/// Recorded by `adk-rust vision models accept <id>`, which shows the licence URL first. No
/// agent tool records one (V-I11).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenceAcceptance {
    /// The model manifest id.
    pub model: String,
    /// The licence's SPDX id or `LicenseRef-` name.
    pub licence_id: String,
    /// The licence URL the person was shown.
    pub licence_url: String,
    /// The digest of the licence text the person read, when it was fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence_text_digest: Option<Digest>,
    /// Who accepted.
    pub principal: String,
    /// When.
    pub accepted_at: DateTime<Utc>,
}

impl LicenceAcceptance {
    /// An acceptance of `manifest`'s licence by `principal`, now.
    pub fn of(manifest: &ModelManifest, principal: impl Into<String>) -> Self {
        Self {
            model: manifest.id.clone(),
            licence_id: manifest.licence.id.clone(),
            licence_url: manifest.licence.url.clone(),
            licence_text_digest: None,
            principal: principal.into(),
            accepted_at: Utc::now(),
        }
    }

    fn covers(&self, manifest: &ModelManifest) -> bool {
        self.model == manifest.id
            && self.licence_id == manifest.licence.id
            && self.licence_url == manifest.licence.url
    }
}

/// Where licence acceptances are kept.
pub trait AcceptanceStore: Send + Sync {
    /// Every recorded acceptance.
    ///
    /// # Errors
    ///
    /// The store's read error.
    fn acceptances(&self) -> Result<Vec<LicenceAcceptance>>;

    /// Records an acceptance.
    ///
    /// # Errors
    ///
    /// The store's write error.
    fn record(&self, acceptance: LicenceAcceptance) -> Result<()>;
}

/// Acceptances in memory.
#[derive(Debug, Default)]
pub struct InMemoryAcceptanceStore {
    acceptances: Mutex<Vec<LicenceAcceptance>>,
}

impl InMemoryAcceptanceStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl AcceptanceStore for InMemoryAcceptanceStore {
    fn acceptances(&self) -> Result<Vec<LicenceAcceptance>> {
        Ok(self.acceptances.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    fn record(&self, acceptance: LicenceAcceptance) -> Result<()> {
        self.acceptances.lock().unwrap_or_else(|e| e.into_inner()).push(acceptance);
        Ok(())
    }
}

/// Acceptances in a JSON file, by default `.adk/vision/licences.json` in the project.
///
/// # Example
///
/// ```
/// use adk_vision::{AcceptanceStore, FileAcceptanceStore};
///
/// let dir = std::env::temp_dir().join(format!("adk-vision-doc-{}", std::process::id()));
/// let store = FileAcceptanceStore::new(dir.join("licences.json"));
/// assert!(store.acceptances().unwrap().is_empty());
/// ```
#[derive(Debug)]
pub struct FileAcceptanceStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileAcceptanceStore {
    /// A store in the file at `path`; the file is created on the first acceptance.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Mutex::new(()) }
    }

    /// The project's store, `<project>/.adk/vision/licences.json`.
    pub fn in_project(project: &Path) -> Self {
        Self::new(project.join(".adk").join("vision").join("licences.json"))
    }

    fn read(&self) -> Result<Vec<LicenceAcceptance>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                VisionError::InvalidInput(format!("{} is not valid: {e}", self.path.display()))
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(err) => Err(VisionError::io("could not read", &self.path, err)),
        }
    }
}

impl AcceptanceStore for FileAcceptanceStore {
    fn acceptances(&self) -> Result<Vec<LicenceAcceptance>> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read()
    }

    fn record(&self, acceptance: LicenceAcceptance) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.read()?;
        all.push(acceptance);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| VisionError::io("could not create", parent, e))?;
        }
        let text = serde_json::to_string_pretty(&all)
            .map_err(|e| VisionError::InvalidInput(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| VisionError::io("could not write", &tmp, e))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| VisionError::io("could not replace", &self.path, e))
    }
}

/// The non-commercial and copyleft models a user's configuration allows, by id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LicencePolicy {
    /// Model ids allowed despite a non-commercial or copyleft licence.
    #[serde(default)]
    pub allow: BTreeSet<String>,
}

impl LicencePolicy {
    /// Allows nothing beyond permissive and accepted use-restricted models.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allows a non-commercial or copyleft model by id.
    pub fn allowing(mut self, model: impl Into<String>) -> Self {
        self.allow.insert(model.into());
        self
    }
}

/// Decides whether a model may be fetched or loaded under its licence.
///
/// # Example
///
/// ```
/// use adk_vision::{
///     InMemoryAcceptanceStore, LicenceAcceptance, LicenceGate, LicencePolicy, ModelManifest,
/// };
///
/// let sam = ModelManifest::from_json(r#"{
///     "id": "sam3-local", "task": "segment",
///     "source": {"kind": "local", "dir": "/models/sam3"},
///     "files": [{"path": "sam3.onnx", "size": 1,
///       "sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000000"}],
///     "licence": {"id": "LicenseRef-SAM", "url": "https://example.org/sam-license",
///       "class": "useRestricted"},
///     "runtime": {"kind": "ort", "opset": 17}
/// }"#).unwrap();
/// let store = InMemoryAcceptanceStore::new();
/// let policy = LicencePolicy::new();
/// let gate = LicenceGate::new(&policy, &store);
/// let refused = gate.check(&sam).unwrap_err().to_string();
/// assert!(refused.contains("licence acceptance required"));
/// assert!(refused.contains("https://example.org/sam-license"));
///
/// use adk_vision::AcceptanceStore;
/// store.record(LicenceAcceptance::of(&sam, "alice@example.com")).unwrap();
/// assert!(gate.check(&sam).is_ok());
/// ```
pub struct LicenceGate<'a> {
    policy: &'a LicencePolicy,
    acceptances: &'a dyn AcceptanceStore,
}

impl<'a> LicenceGate<'a> {
    /// A gate over a policy and an acceptance store.
    pub fn new(policy: &'a LicencePolicy, acceptances: &'a dyn AcceptanceStore) -> Self {
        Self { policy, acceptances }
    }

    /// Allows a permissive model; a use-restricted one with a recorded acceptance of the
    /// same licence; a non-commercial or copyleft one the policy names.
    ///
    /// # Errors
    ///
    /// [`VisionError::LicenceAcceptanceRequired`] with the licence URL,
    /// [`VisionError::LicenceNotAllowed`], or the store's error.
    pub fn check(&self, manifest: &ModelManifest) -> Result<()> {
        match manifest.licence.class {
            LicenceClass::Permissive => Ok(()),
            LicenceClass::UseRestricted => {
                if self.acceptances.acceptances()?.iter().any(|a| a.covers(manifest)) {
                    Ok(())
                } else {
                    Err(VisionError::LicenceAcceptanceRequired {
                        model: manifest.id.clone(),
                        licence_url: manifest.licence.url.clone(),
                    })
                }
            }
            class @ (LicenceClass::NonCommercial | LicenceClass::Copyleft) => {
                if self.policy.allow.contains(&manifest.id) {
                    Ok(())
                } else {
                    Err(VisionError::LicenceNotAllowed {
                        model: manifest.id.clone(),
                        class: class.to_string(),
                        licence_url: manifest.licence.url.clone(),
                    })
                }
            }
        }
    }
}
