//! `DirectoryHost`: releases as directories, switched atomically (spec 3.5 §7.8 "Hosts").

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use async_trait::async_trait;
use awp_types::{Atomicity, FileEntry, HostLimits, ReleaseManifest};
use bytes::Bytes;

use super::{HostTarget, PublishReport, SiteHost};
use crate::error::{Result, SiteError};
use crate::log::LOG_PREFIX;
use crate::redirects::RedirectsFormat;
use crate::release::{
    RELEASES_PREFIX, StagedDelta, StagedRelease, file_digest, manifest_digest, manifest_path,
};

const NAME: &str = "directory";

/// A host that keeps each release in its own directory and switches a pointer last
/// (spec 3.5 §7.8, §7.8.3 `Atomicity::Deploy`).
///
/// | Path under the root | Holds |
/// |---------------------|-------|
/// | `releases/<seq>/` | A production release: its files, every manifest so far and the log |
/// | `previews/<seq>-<digest>/` | A preview release |
/// | `current.pointer`, `preview.pointer` | The directory each target serves, replaced by an atomic rename |
/// | `current`, `preview` | Symbolic links to the same directories, for a static file server (Unix) |
///
/// A publish copies the release into a staging directory — files, manifest, log tiles,
/// checkpoint — then renames it into place and switches the pointer, so a reader sees the
/// previous release or the next one, never a mixture. Old releases stay for rollback.
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::host::{DirectoryHost, HostTarget, SiteHost};
///
/// # async fn demo(staged: &adk_site::release::StagedRelease) -> adk_site::Result<()> {
/// let host = DirectoryHost::new("/srv/example.com");
/// host.publish(staged, HostTarget::Production).await?;
/// let page = host.fetch("/index.html").await?;
/// # let _ = page;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct DirectoryHost {
    root: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> SiteError + '_ {
    move |e| SiteError::io(path, e)
}

fn relative(path: &str) -> Result<PathBuf> {
    let trimmed = path.trim_start_matches('/');
    let mut file = PathBuf::from(trimmed);
    if trimmed.is_empty() || trimmed.ends_with('/') {
        file = file.join("index.html");
    }
    if file.components().all(|c| matches!(c, Component::Normal(_))) {
        Ok(file)
    } else {
        Err(SiteError::host(NAME, format!("path '{path}' leaves the site root")))
    }
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(io(parent))?;
    }
    std::fs::copy(from, to).map_err(io(from))?;
    Ok(())
}

fn write(to: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(io(parent))?;
    }
    std::fs::write(to, bytes).map_err(io(to))
}

/// Copies every file under `dir/prefix` into `target/prefix`.
fn carry(dir: &Path, target: &Path, prefix: &str) -> Result<()> {
    let from = dir.join(prefix);
    for file in crate::generator::list_files(&from)? {
        copy(&from.join(&file), &target.join(prefix).join(&file))?;
    }
    Ok(())
}

fn matches_digest(path: &Path, entry: &FileEntry) -> bool {
    std::fs::read(path).is_ok_and(|bytes| file_digest(&bytes) == entry.sha256)
}

fn nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

impl DirectoryHost {
    /// A host rooted at `root`, created on first publish.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), lock: tokio::sync::Mutex::new(()) }
    }

    /// The host's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn pointer(&self, target: HostTarget) -> PathBuf {
        self.root.join(match target {
            HostTarget::Production => "current.pointer",
            HostTarget::Preview => "preview.pointer",
        })
    }

    /// The directory `target` serves, if it serves one.
    ///
    /// # Errors
    ///
    /// [`SiteError::Io`] when the pointer cannot be read.
    pub fn serving_dir(&self, target: HostTarget) -> Result<Option<PathBuf>> {
        let pointer = self.pointer(target);
        match std::fs::read_to_string(&pointer) {
            Ok(text) => Ok(Some(self.root.join(text.trim()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SiteError::io(pointer, e)),
        }
    }

    fn switch(&self, target: HostTarget, dir_name: &str) -> Result<()> {
        let pointer = self.pointer(target);
        let tmp = self.root.join(format!(".pointer-{}", nanos()));
        std::fs::write(&tmp, dir_name).map_err(io(&tmp))?;
        #[cfg(unix)]
        {
            let link = self.root.join(match target {
                HostTarget::Production => "current",
                HostTarget::Preview => "preview",
            });
            let tmp_link = self.root.join(format!(".link-{}", nanos()));
            std::os::unix::fs::symlink(dir_name, &tmp_link).map_err(io(&tmp_link))?;
            std::fs::rename(&tmp_link, &link).map_err(io(&link))?;
        }
        std::fs::rename(&tmp, &pointer).map_err(io(&pointer))
    }

    fn publish_sync(&self, release: &StagedRelease, target: HostTarget) -> Result<PublishReport> {
        let start = Instant::now();
        let seq = release.manifest.seq;
        let (group, dir_name) = match target {
            HostTarget::Production => ("releases", format!("releases/{seq}")),
            HostTarget::Preview => {
                ("previews", format!("previews/{seq}-{}", &release.digest.hex()[..12]))
            }
        };
        let final_dir = self.root.join(&dir_name);
        if final_dir.exists() {
            if target == HostTarget::Preview {
                self.switch(target, &dir_name)?;
                return Ok(PublishReport {
                    host: NAME.into(),
                    target,
                    seq,
                    uploaded: Vec::new(),
                    removed: Vec::new(),
                    elapsed: start.elapsed(),
                });
            }
            return Err(SiteError::Sequence(format!(
                "release {seq} is already published on this host"
            )));
        }
        let current = self.serving_dir(target)?;
        if target == HostTarget::Production
            && let Some(current) = &current
            && let Some(previous) = release.manifest.previous.as_ref()
        {
            let previous_seq = seq - 1;
            let stored = current.join(manifest_path(previous_seq));
            if let Ok(bytes) = std::fs::read(&stored)
                && file_digest(&bytes) != *previous
            {
                return Err(SiteError::Sequence(format!(
                    "release {seq} follows a manifest this host does not serve as release {previous_seq}"
                )));
            }
        }
        let staging = self.root.join(group).join(format!(".staging-{seq}-{}", nanos()));
        std::fs::create_dir_all(&staging).map_err(io(&staging))?;
        let result = (|| -> Result<Vec<String>> {
            let mut uploaded = Vec::new();
            for (path, entry) in &release.files {
                let file = relative(path)?;
                let unchanged = !release.changed.contains(path);
                let reused = unchanged
                    && current.as_ref().is_some_and(|dir| {
                        let candidate = dir.join(&file);
                        matches_digest(&candidate, entry)
                            && copy(&candidate, &staging.join(&file)).is_ok()
                    });
                if !reused {
                    write(&staging.join(&file), &release.read(path)?)?;
                    uploaded.push(path.clone());
                }
            }
            if let Some(dir) = &current {
                carry(dir, &staging, RELEASES_PREFIX)?;
            }
            write(&staging.join(manifest_path(seq)), &release.manifest_bytes)?;
            uploaded.push(format!("/{}", manifest_path(seq)));
            if let Some(log) = &release.log {
                if let Some(dir) = &current {
                    carry(dir, &staging, LOG_PREFIX)?;
                }
                for (path, bytes) in log.files() {
                    write(&staging.join(path), bytes)?;
                    uploaded.push(format!("/{path}"));
                }
            }
            Ok(uploaded)
        })();
        let uploaded = match result {
            Ok(uploaded) => uploaded,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
        std::fs::rename(&staging, &final_dir).map_err(io(&final_dir))?;
        self.switch(target, &dir_name)?;
        Ok(PublishReport {
            host: NAME.into(),
            target,
            seq,
            uploaded,
            removed: release.removed.iter().cloned().collect(),
            elapsed: start.elapsed(),
        })
    }

    fn rollback_sync(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        let start = Instant::now();
        let stored = self.root.join("releases").join(to.seq.to_string());
        let not_found = || SiteError::ReleaseNotFound {
            seq: to.seq,
            message: format!("not under {}", stored.display()),
        };
        if !stored.is_dir() {
            return Err(not_found());
        }
        let read_manifest = |seq: u64| -> Result<ReleaseManifest> {
            let path = stored.join(manifest_path(seq));
            let bytes = std::fs::read(&path).map_err(|_| not_found())?;
            serde_json::from_slice(&bytes)
                .map_err(|e| SiteError::serde(path.display().to_string(), e))
        };
        let own = read_manifest(to.seq)?;
        let (expected, _) = manifest_digest(to)?;
        let (actual, _) = manifest_digest(&own)?;
        if expected != actual {
            return Err(SiteError::DigestMismatch {
                path: format!("/{}", manifest_path(to.seq)),
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
        let mut chain = vec![own];
        while let Some(base) = chain.last().and_then(|m| m.base) {
            chain.push(read_manifest(base)?);
        }
        let mut files: BTreeMap<String, FileEntry> = BTreeMap::new();
        for manifest in chain.iter().rev() {
            files = manifest.apply_to(&files);
        }
        for (path, entry) in &files {
            let local = stored.join(relative(path)?);
            if !matches_digest(&local, entry) {
                let actual = std::fs::read(&local)
                    .map(|b| file_digest(&b).to_string())
                    .unwrap_or_else(|_| "missing".into());
                return Err(SiteError::DigestMismatch {
                    path: path.clone(),
                    expected: entry.sha256.to_string(),
                    actual,
                });
            }
        }
        let current = self.serving_dir(HostTarget::Production)?.ok_or_else(not_found)?;
        let dir_name = format!("releases/rollback-{}-{}", to.seq, nanos());
        let staging = self.root.join(format!("releases/.staging-rollback-{}", nanos()));
        let mut uploaded = Vec::new();
        for path in files.keys() {
            let file = relative(path)?;
            copy(&stored.join(&file), &staging.join(&file))?;
            uploaded.push(path.clone());
        }
        carry(&current, &staging, RELEASES_PREFIX)?;
        carry(&current, &staging, LOG_PREFIX)?;
        let before: BTreeSet<String> = crate::generator::list_files(&current)?
            .into_iter()
            .map(|p| format!("/{p}"))
            .filter(|p| !crate::release::is_log_path(p))
            .collect();
        std::fs::rename(&staging, self.root.join(&dir_name)).map_err(io(&staging))?;
        self.switch(HostTarget::Production, &dir_name)?;
        Ok(PublishReport {
            host: NAME.into(),
            target: HostTarget::Production,
            seq: to.seq,
            uploaded,
            removed: before.into_iter().filter(|p| !files.contains_key(p)).collect(),
            elapsed: start.elapsed(),
        })
    }
}

#[async_trait]
impl SiteHost for DirectoryHost {
    fn name(&self) -> &str {
        NAME
    }

    async fn publish(&self, release: &StagedRelease, target: HostTarget) -> Result<PublishReport> {
        let _guard = self.lock.lock().await;
        let host = Self::new(self.root.clone());
        let release = release.clone();
        tokio::task::spawn_blocking(move || host.publish_sync(&release, target))
            .await
            .map_err(|e| SiteError::host(NAME, e.to_string()))?
    }

    async fn rollback(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        let _guard = self.lock.lock().await;
        let host = Self::new(self.root.clone());
        let to = to.clone();
        tokio::task::spawn_blocking(move || host.rollback_sync(&to))
            .await
            .map_err(|e| SiteError::host(NAME, e.to_string()))?
    }

    async fn fetch(&self, path: &str) -> Result<Option<Bytes>> {
        let Some(dir) = self.serving_dir(HostTarget::Production)? else { return Ok(None) };
        let full = dir.join(relative(path)?);
        match tokio::fs::read(&full).await {
            Ok(bytes) => Ok(Some(Bytes::from(bytes))),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(None)
            }
            Err(e) if full.is_dir() => {
                Err(SiteError::host(NAME, format!("'{path}' is a directory: {e}")))
            }
            Err(e) => Err(SiteError::io(full, e)),
        }
    }

    fn redirects_format(&self) -> RedirectsFormat {
        RedirectsFormat::Json
    }

    fn limits(&self) -> HostLimits {
        HostLimits { max_publishes_per_hour: None, delta_upload: true, atomic: Atomicity::Deploy }
    }

    async fn publish_delta(
        &self,
        delta: &StagedDelta,
        target: HostTarget,
    ) -> Result<PublishReport> {
        self.publish(delta.release(), target).await
    }
}
