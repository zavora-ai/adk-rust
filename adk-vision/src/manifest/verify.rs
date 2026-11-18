//! [`VerifiedModel`]: model files whose SHA-256 matched their manifest.

use std::path::{Path, PathBuf};

use super::{LicenceGate, ModelFile, ModelManifest};
use crate::digest::Digest;
use crate::error::{Result, VisionError};

/// One model file whose size and SHA-256 matched its manifest when it was checked.
///
/// Only verification constructs one, so a backend that takes a [`VerifiedModel`] cannot
/// load an unchecked file (V-I10):
///
/// ```compile_fail,E0451
/// use adk_vision::VerifiedFile;
///
/// let forged = VerifiedFile { path: "/tmp/model.onnx".into(), digest: adk_vision::Digest::of(b""), size: 0 };
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedFile {
    path: PathBuf,
    digest: Digest,
    size: u64,
}

impl VerifiedFile {
    /// Checks the file at `path` against its manifest entry: size first, then SHA-256.
    ///
    /// # Errors
    ///
    /// [`VisionError::SizeMismatch`] or [`VisionError::DigestMismatch`] with both values,
    /// or [`VisionError::Io`].
    pub fn check(path: &Path, expected: &ModelFile) -> Result<Self> {
        let size =
            std::fs::metadata(path).map_err(|e| VisionError::io("could not read", path, e))?.len();
        if size != expected.size {
            return Err(VisionError::SizeMismatch {
                path: path.to_path_buf(),
                expected: expected.size,
                actual: size,
            });
        }
        let (digest, _) = Digest::of_file(path)?;
        if digest != expected.sha256 {
            tracing::warn!(
                model.file = %path.display(),
                expected = %expected.sha256,
                actual = %digest,
                "model file rejected"
            );
            return Err(VisionError::DigestMismatch {
                path: path.to_path_buf(),
                expected: expected.sha256,
                actual: digest,
            });
        }
        Ok(Self { path: path.to_path_buf(), digest, size })
    }

    /// Where the file is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Its SHA-256.
    pub fn digest(&self) -> Digest {
        self.digest
    }

    /// Its size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// A model whose every file matched its manifest, under a licence the gate allowed.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedModel {
    manifest: ModelManifest,
    files: Vec<(String, VerifiedFile)>,
}

impl VerifiedModel {
    pub(crate) fn new(manifest: ModelManifest, files: Vec<(String, VerifiedFile)>) -> Self {
        Self { manifest, files }
    }

    /// Checks the files of `manifest` in `dir`, laid out at their manifest paths, after the
    /// licence gate allows the model. Every file is hashed on every call, so a file changed
    /// since an earlier check is caught.
    ///
    /// # Errors
    ///
    /// The licence gate's refusal, then the first file's [`VerifiedFile::check`] error.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::{
    ///     Digest, InMemoryAcceptanceStore, LicenceGate, LicencePolicy, ModelManifest,
    ///     VerifiedModel, VisionError,
    /// };
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// std::fs::write(dir.path().join("w.bin"), b"weights").unwrap();
    /// let manifest = ModelManifest::from_json(&format!(r#"{{
    ///     "id": "toy", "task": "detect",
    ///     "source": {{"kind": "local", "dir": "{}"}},
    ///     "files": [{{"path": "w.bin", "size": 7, "sha256": "{}"}}],
    ///     "licence": {{"id": "MIT", "url": "https://opensource.org/license/mit", "class": "permissive"}},
    ///     "runtime": {{"kind": "candle"}}
    /// }}"#, dir.path().display(), Digest::of(b"weights"))).unwrap();
    /// let (policy, store) = (LicencePolicy::new(), InMemoryAcceptanceStore::new());
    /// let gate = LicenceGate::new(&policy, &store);
    /// assert!(VerifiedModel::check_dir(&manifest, dir.path(), &gate).is_ok());
    ///
    /// std::fs::write(dir.path().join("w.bin"), b"tamper!").unwrap();
    /// let err = VerifiedModel::check_dir(&manifest, dir.path(), &gate).unwrap_err();
    /// assert!(matches!(err, VisionError::DigestMismatch { .. }));
    /// ```
    pub fn check_dir(manifest: &ModelManifest, dir: &Path, gate: &LicenceGate<'_>) -> Result<Self> {
        manifest.validate()?;
        gate.check(manifest)?;
        let mut files = Vec::with_capacity(manifest.files.len());
        for file in &manifest.files {
            let path = dir.join(&file.path);
            files.push((file.path.clone(), VerifiedFile::check(&path, file)?));
        }
        tracing::info!(model.id = %manifest.id, model.files = files.len(), "model verified");
        Ok(Self::new(manifest.clone(), files))
    }

    /// The manifest.
    pub fn manifest(&self) -> &ModelManifest {
        &self.manifest
    }

    /// The verified files, in manifest order.
    pub fn files(&self) -> impl Iterator<Item = &VerifiedFile> {
        self.files.iter().map(|(_, f)| f)
    }

    /// The verified file at a manifest path.
    pub fn file(&self, path: &str) -> Option<&VerifiedFile> {
        self.files.iter().find(|(p, _)| p == path).map(|(_, f)| f)
    }
}
