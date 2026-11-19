//! Model manifests (spec 3.7 §7.4): every weight file pinned by revision and SHA-256, with
//! its licence.
//!
//! Licence is data: a [`ModelManifest`] names its licence and [`LicenceClass`]. A
//! use-restricted model loads only after a person's recorded acceptance
//! ([`LicenceGate`]); non-commercial and copyleft models are never in the built-in
//! manifest (V-I12) and load only when the configuration names them. No model file loads
//! unless its SHA-256 matches (V-I10): backends take a [`VerifiedModel`], which only
//! verification constructs.

mod licence;
mod verify;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use licence::{
    AcceptanceStore, FileAcceptanceStore, InMemoryAcceptanceStore, LicenceAcceptance, LicenceGate,
    LicencePolicy,
};
pub use verify::{VerifiedFile, VerifiedModel};

use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::geometry::Letterbox;

/// What a model or backend does.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VisionTask {
    /// Object detection.
    Detect,
    /// Segmentation.
    Segment,
    /// Document reading.
    Read,
    /// Image and text embedding.
    Embed,
    /// Description, structured extraction and grounding.
    Describe,
    /// Video analysis.
    AnalyzeVideo,
    /// Image editing.
    Edit,
}

/// Where a model's files come from.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ModelSource {
    /// A Hugging Face repository at a commit.
    HuggingFace {
        /// `owner/name`.
        repo: String,
        /// A 40-digit commit hash, never a branch.
        revision: String,
    },
    /// A ModelScope repository at a revision.
    ModelScope {
        /// `owner/name`.
        repo: String,
        /// The revision.
        revision: String,
    },
    /// Files at `<base>/<path>`.
    Url {
        /// The base URL.
        base: String,
    },
    /// Files the user placed in a directory, such as their own RF-DETR export.
    Local {
        /// The directory.
        dir: PathBuf,
    },
}

/// One file of a model, pinned by SHA-256 and size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFile {
    /// The path inside the source, with `/` separators.
    pub path: String,
    /// The file's SHA-256.
    pub sha256: Digest,
    /// The file's size in bytes.
    pub size: u64,
}

/// How freely a model's weights may be used.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LicenceClass {
    /// Apache-2.0, MIT, BSD: may be fetched by default.
    Permissive,
    /// Commercial use with restrictions (the SAM License): fetched only after a person
    /// accepts.
    UseRestricted,
    /// Never in the built-in manifest; a user's configuration must allow it by name.
    NonCommercial,
    /// Never in the built-in manifest; a user's configuration must allow it by name.
    Copyleft,
}

impl std::fmt::Display for LicenceClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Permissive => "permissive",
            Self::UseRestricted => "use-restricted",
            Self::NonCommercial => "non-commercial",
            Self::Copyleft => "copyleft",
        })
    }
}

/// A model's licence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenceInfo {
    /// An SPDX identifier, or `LicenseRef-<name>` for a custom licence such as Meta's SAM
    /// License.
    pub id: String,
    /// Where a person reads it.
    pub url: String,
    /// Its class.
    pub class: LicenceClass,
}

/// The runtime that executes a model.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum RuntimeSpec {
    /// ONNX Runtime through `ort`.
    Ort {
        /// The ONNX opset.
        opset: u32,
    },
    /// Candle.
    Candle,
    /// mistral.rs.
    MistralRs {
        /// The mistral.rs architecture name.
        architecture: String,
    },
}

/// A model's fixed input size and how an image is fitted into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputSpec {
    /// Input width.
    pub width: u32,
    /// Input height.
    pub height: u32,
    /// How an image is fitted.
    pub letterbox: Letterbox,
}

/// A model's input size, normalisation and labels.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IoSpec {
    /// The fixed input, for models that take one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<InputSpec>,
    /// Per-channel mean subtracted from RGB values in `[0, 1]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean: Option<[f32; 3]>,
    /// Per-channel standard deviation RGB values are divided by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub std: Option<[f32; 3]>,
    /// Class labels, by output index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

/// A model: its task, where its files come from, each file's SHA-256 and size, its
/// licence, its runtime and its input and output.
///
/// # Example
///
/// ```
/// use adk_vision::{LicenceClass, ModelManifest};
///
/// let manifest = ModelManifest::from_json(r#"{
///     "id": "rf-detr-n-q4f16",
///     "task": "detect",
///     "source": {
///         "kind": "huggingFace",
///         "repo": "onnx-community/rfdetr_nano-ONNX",
///         "revision": "eae21cee0687a91bcf9fa071605c48d7705d2d91"
///     },
///     "files": [{
///         "path": "onnx/model_q4f16.onnx",
///         "sha256": "sha256:42d8072bc00975405531e4e7f20dda40a6cb5ed67e345ecd764f92748463a996",
///         "size": 18996625
///     }],
///     "licence": {
///         "id": "Apache-2.0",
///         "url": "https://www.apache.org/licenses/LICENSE-2.0",
///         "class": "permissive"
///     },
///     "runtime": { "kind": "ort", "opset": 17 }
/// }"#).unwrap();
/// assert_eq!(manifest.licence.class, LicenceClass::Permissive);
///
/// // A branch is not a revision.
/// let branch = r#"{"id":"m","task":"detect","source":{"kind":"huggingFace","repo":"a/b","revision":"main"},
///   "files":[],"licence":{"id":"MIT","url":"https://x","class":"permissive"},"runtime":{"kind":"candle"}}"#;
/// assert!(ModelManifest::from_json(branch).is_err());
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelManifest {
    /// Stable identifier, for example `rf-detr-m`.
    pub id: String,
    /// What the model does.
    pub task: VisionTask,
    /// Where its files come from.
    pub source: ModelSource,
    /// Every file with its SHA-256 and size.
    pub files: Vec<ModelFile>,
    /// Its licence.
    pub licence: LicenceInfo,
    /// The runtime that executes it.
    pub runtime: RuntimeSpec,
    /// Input size, normalisation and labels.
    #[serde(default)]
    pub io: IoSpec,
}

impl ModelManifest {
    /// Parses and validates a manifest.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] for invalid JSON or a manifest [`validate`](Self::validate)
    /// refuses.
    pub fn from_json(text: &str) -> Result<Self> {
        let manifest: Self = serde_json::from_str(text).map_err(|e| VisionError::Manifest {
            id: "<unparsed>".into(),
            reason: e.to_string(),
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Checks the manifest: an id of lowercase letters, digits, `.`, `_` and `-`; a
    /// Hugging Face revision that is a 40-digit commit hash; at least one file; relative
    /// file paths with no `..`; and a licence with an id and a URL.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] naming the first problem.
    pub fn validate(&self) -> Result<()> {
        let invalid = |reason: String| VisionError::Manifest { id: self.id.clone(), reason };
        if self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        {
            return Err(invalid(
                "the id must be lowercase letters, digits, `.`, `_` and `-`".into(),
            ));
        }
        match &self.source {
            ModelSource::HuggingFace { repo, revision } => {
                if repo.split('/').count() != 2 || repo.split('/').any(str::is_empty) {
                    return Err(invalid(format!("`{repo}` is not an `owner/name` repository")));
                }
                if revision.len() != 40 || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(invalid(format!(
                        "revision `{revision}` is not a 40-digit commit hash; pin a commit, never a branch"
                    )));
                }
            }
            ModelSource::ModelScope { repo, revision } => {
                if repo.is_empty() || revision.is_empty() {
                    return Err(invalid(
                        "a ModelScope source needs a repository and a revision".into(),
                    ));
                }
            }
            ModelSource::Url { base } => {
                if !base.starts_with("https://") {
                    return Err(invalid(format!("`{base}` is not an https URL")));
                }
            }
            ModelSource::Local { .. } => {}
        }
        if self.files.is_empty() {
            return Err(invalid("the manifest pins no file".into()));
        }
        for file in &self.files {
            let path = &file.path;
            if path.is_empty()
                || path.starts_with('/')
                || path.contains('\\')
                || path.split('/').any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err(invalid(format!("file path `{path}` must be relative, with no `..`")));
            }
        }
        if self.licence.id.is_empty() || self.licence.url.is_empty() {
            return Err(invalid("the licence needs an id and a URL".into()));
        }
        Ok(())
    }

    /// The file at `path`, if the manifest pins one.
    pub fn file(&self, path: &str) -> Option<&ModelFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// A set of manifests by id: the built-in entries plus the user's own.
///
/// # Example
///
/// ```
/// use adk_vision::ManifestSet;
///
/// let builtin = ManifestSet::builtin().unwrap();
/// // V-I12: nothing non-commercial or copyleft is built in.
/// assert!(builtin.iter().all(|m| matches!(
///     m.licence.class,
///     adk_vision::LicenceClass::Permissive | adk_vision::LicenceClass::UseRestricted
/// )));
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ManifestSet {
    manifests: BTreeMap<String, ModelManifest>,
}

/// The built-in manifest. Entries arrive with each backend's workstream, pinned by the
/// benchmark run that accepts the model (spec 3.7 §7.4).
const BUILTIN: &str = include_str!("../../manifests/builtin.json");

impl ManifestSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in manifest.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] when an entry is invalid or is non-commercial or copyleft;
    /// the crate's tests keep this from shipping.
    pub fn builtin() -> Result<Self> {
        let entries: Vec<ModelManifest> = serde_json::from_str(BUILTIN)
            .map_err(|e| VisionError::Manifest { id: "<builtin>".into(), reason: e.to_string() })?;
        let mut set = Self::new();
        for manifest in entries {
            if matches!(
                manifest.licence.class,
                LicenceClass::NonCommercial | LicenceClass::Copyleft
            ) {
                return Err(VisionError::Manifest {
                    id: manifest.id.clone(),
                    reason: format!(
                        "a {} model cannot be built in; it belongs in a user manifest",
                        manifest.licence.class
                    ),
                });
            }
            set = set.with(manifest)?;
        }
        Ok(set)
    }

    /// Adds a manifest.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] when it is invalid or its id is already in the set.
    pub fn with(mut self, manifest: ModelManifest) -> Result<Self> {
        manifest.validate()?;
        if self.manifests.contains_key(&manifest.id) {
            return Err(VisionError::Manifest {
                id: manifest.id.clone(),
                reason: "another manifest already uses this id".into(),
            });
        }
        self.manifests.insert(manifest.id.clone(), manifest);
        Ok(self)
    }

    /// Adds every `*.json` manifest in `dir`, the user's own models.
    ///
    /// # Errors
    ///
    /// [`VisionError::Io`] when the directory cannot be read, or the error of the first
    /// manifest that does not parse or clashes.
    pub fn with_dir(mut self, dir: &Path) -> Result<Self> {
        let entries =
            std::fs::read_dir(dir).map_err(|e| VisionError::io("could not read", dir, e))?;
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort();
        for path in paths {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| VisionError::io("could not read", &path, e))?;
            self = self.with(ModelManifest::from_json(&text)?)?;
        }
        Ok(self)
    }

    /// The manifest with this id.
    pub fn get(&self, id: &str) -> Option<&ModelManifest> {
        self.manifests.get(id)
    }

    /// Every manifest, by id.
    pub fn iter(&self) -> impl Iterator<Item = &ModelManifest> {
        self.manifests.values()
    }

    /// The number of manifests.
    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    /// `true` when the set is empty.
    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }
}
