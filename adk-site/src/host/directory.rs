//! `DirectoryHost`: releases under a prefix each, switched by a pointer written last (spec 3.5
//! §7.8 "Hosts", §7.8.3 "Hosts at speed", `Atomicity::PrefixPointer`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use awp_types::{Atomicity, Digest, FileEntry, HostLimits, ReleaseManifest};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::{HostTarget, PublishReport, SiteHost};
use crate::error::{Result, SiteError};
use crate::log::{CHECKPOINT_PATH, LOG_PREFIX};
use crate::redirects::RedirectsFormat;
use crate::release::{
    RELEASES_PREFIX, StagedDelta, StagedRelease, file_digest, manifest_digest, manifest_path,
};

const NAME: &str = "directory";
/// Resolved file sets kept in memory.
const CACHED_INDEXES: usize = 4;

/// A release's full file set: path → entry.
type Index = BTreeMap<String, FileEntry>;

/// A host that stores every file version once by its digest, publishes each release under
/// its own prefix and switches one pointer last (spec 3.5 §7.8, §7.8.3
/// `Atomicity::PrefixPointer`).
///
/// | Path under the root | Holds |
/// |---------------------|-------|
/// | `objects/<2 hex>/<62 hex>` | Every file version, written once under its SHA-256 |
/// | `manifests/<seq>.json` | Every logged manifest, served at `/.well-known/awp/releases/<seq>.json` |
/// | `log/…` | The log's tiles, each written once (C2SP tiles never change once written) |
/// | `checkpoints/<seq>` | The checkpoint each release published |
/// | `previews/<seq>-<digest>.json` | A preview's file set |
/// | `releases/<seq>/`, `releases/rollback-<seq>-<n>/`, `previews/<seq>-<digest>/` | A full directory per release for a static file server: hard links into `objects/` (materialised hosts) |
/// | `current.pointer`, `preview.pointer` | What each target serves, replaced by an atomic rename |
/// | `current`, `preview` | Symbolic links to the materialised directories (Unix) |
///
/// A publish stores the release's new file versions, its manifest, its tiles and its
/// checkpoint, then renames the pointer into place, so a reader sees the previous release
/// or the next one, never a mixture. A delta stores only what changed.
///
/// Stored files are read-only. On Unix a release directory hard-links them, so a write
/// through a served file is refused instead of changing every release that shares the
/// object; elsewhere a release directory holds copies. A file replaced by a rename changes
/// that directory only, and [`rollback`](SiteHost::rollback) checks every object's digest
/// before it switches.
///
/// By default every release is also materialised as a directory a static file server can
/// serve, and [`fetch`](SiteHost::fetch) reads that directory, so a file planted in it shows
/// as drift. [`materialise(false)`](Self::materialise) keeps only the objects — a publish
/// then costs what changed, not the size of the site, and reads resolve through the
/// pointer's file set, as an edge in front of object storage does. [`retain_releases`]
/// bounds storage by keeping the files of the latest releases only.
///
/// [`retain_releases`]: Self::retain_releases
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::host::{DirectoryHost, HostTarget, SiteHost};
///
/// # async fn demo(staged: &adk_site::release::StagedRelease) -> adk_site::Result<()> {
/// let host = DirectoryHost::new("/srv/example.com").materialise(false).retain_releases(200);
/// host.publish(staged, HostTarget::Production).await?;
/// let page = host.fetch("/index.html").await?;
/// # let _ = page;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct DirectoryHost {
    store: Arc<Store>,
    lock: tokio::sync::Mutex<()>,
}

#[derive(Debug)]
struct Store {
    root: PathBuf,
    materialise: bool,
    retain: Option<u64>,
    cache: std::sync::Mutex<Vec<(String, Arc<Index>)>>,
}

/// What a target serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pointer {
    /// The release whose files are served; a rollback serves an earlier release's files.
    files: u64,
    /// The latest logged release: its manifests, tiles and checkpoint are served.
    log: u64,
    /// A preview's name under `previews/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preview: Option<String>,
    /// The materialised directory, relative to the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dir: Option<String>,
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

/// The manifest key of a served path: `/a/b/index.html`.
fn key_of(path: &str) -> Result<String> {
    Ok(format!("/{}", relative(path)?.to_string_lossy().replace('\\', "/")))
}

fn write_atomic(to: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(io(parent))?;
    }
    let tmp = to.with_extension(format!("tmp-{}", nanos()));
    std::fs::write(&tmp, bytes).map_err(io(&tmp))?;
    std::fs::rename(&tmp, to).map_err(io(to))
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(e) if path.is_dir() => {
            Err(SiteError::host(NAME, format!("'{}' is a directory: {e}", path.display())))
        }
        Err(e) => Err(SiteError::io(path, e)),
    }
}

/// Makes a stored file read-only. A release directory hard-links the objects every release
/// shares, so a write through one of its files would otherwise change them all.
fn seal(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).map_err(io(path))?.permissions().mode();
        if mode & 0o222 != 0 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & !0o222))
                .map_err(io(path))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Places `from` at `to` in a release directory: a hard link to the sealed, read-only object
/// on Unix (a copy when the file system refuses a link), a copy elsewhere, where a read-only
/// file cannot be replaced by a rename.
fn link(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(io(parent))?;
    }
    #[cfg(unix)]
    {
        if std::fs::hard_link(from, to).is_err() {
            std::fs::copy(from, to).map_err(io(from))?;
        }
        seal(to)
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(from, to).map_err(io(from))?;
        Ok(())
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

impl Store {
    fn object(&self, digest: &Digest) -> PathBuf {
        let hex = digest.hex();
        self.root.join("objects").join(&hex[..2]).join(&hex[2..])
    }

    fn manifest_file(&self, seq: u64) -> PathBuf {
        self.root.join("manifests").join(format!("{seq}.json"))
    }

    fn checkpoint_file(&self, seq: u64) -> PathBuf {
        self.root.join("checkpoints").join(seq.to_string())
    }

    fn pointer_file(&self, target: HostTarget) -> PathBuf {
        self.root.join(match target {
            HostTarget::Production => "current.pointer",
            HostTarget::Preview => "preview.pointer",
        })
    }

    fn pointer(&self, target: HostTarget) -> Result<Option<Pointer>> {
        let path = self.pointer_file(target);
        match read_optional(&path)? {
            None => Ok(None),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| SiteError::serde(path.display().to_string(), e)),
        }
    }

    fn switch(&self, target: HostTarget, pointer: &Pointer) -> Result<()> {
        #[cfg(unix)]
        if let Some(dir) = &pointer.dir {
            let link = self.root.join(match target {
                HostTarget::Production => "current",
                HostTarget::Preview => "preview",
            });
            let tmp_link = self.root.join(format!(".link-{}", nanos()));
            std::os::unix::fs::symlink(dir, &tmp_link).map_err(io(&tmp_link))?;
            std::fs::rename(&tmp_link, &link).map_err(io(&link))?;
        }
        let bytes = serde_json::to_vec(pointer).map_err(|e| SiteError::serde("pointer", e))?;
        write_atomic(&self.pointer_file(target), &bytes)
    }

    /// Stores a file version under its digest, copying it from the build (a clone on file
    /// systems that support one) and checking its digest; returns whether it was new. A stored
    /// object whose bytes no longer match its digest is replaced, so republishing a release
    /// repairs it.
    fn store_object(&self, release: &StagedRelease, path: &str, entry: &FileEntry) -> Result<bool> {
        let object = self.object(&entry.sha256);
        if let Some(bytes) = read_optional(&object)? {
            if file_digest(&bytes) == entry.sha256 {
                return Ok(false);
            }
            tracing::warn!(
                site.host.object = %entry.sha256,
                path,
                "a stored object no longer matches its digest; replacing it"
            );
        }
        let parent = object.parent().unwrap_or(&self.root);
        std::fs::create_dir_all(parent).map_err(io(parent))?;
        let tmp = object.with_extension(format!("tmp-{}", nanos()));
        let source = release.local_path(path);
        std::fs::copy(&source, &tmp).map_err(io(&source))?;
        let bytes = std::fs::read(&tmp).map_err(io(&tmp))?;
        let actual = file_digest(&bytes);
        if actual != entry.sha256 {
            let _ = std::fs::remove_file(&tmp);
            return Err(SiteError::DigestMismatch {
                path: path.to_string(),
                expected: entry.sha256.to_string(),
                actual: actual.to_string(),
            });
        }
        std::fs::rename(&tmp, &object).map_err(io(&object))?;
        seal(&object)?;
        Ok(true)
    }

    fn read_manifest(&self, seq: u64) -> Result<ReleaseManifest> {
        let path = self.manifest_file(seq);
        let bytes = std::fs::read(&path).map_err(|_| SiteError::ReleaseNotFound {
            seq,
            message: format!("no manifest at {}", path.display()),
        })?;
        serde_json::from_slice(&bytes).map_err(|e| SiteError::serde(path.display().to_string(), e))
    }

    fn cached(&self, key: &str) -> Option<Arc<Index>> {
        let cache = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.iter().find(|(k, _)| k == key).map(|(_, index)| index.clone())
    }

    fn remember(&self, key: String, index: Arc<Index>) {
        let mut cache = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|(k, _)| *k != key);
        cache.push((key, index));
        let excess = cache.len().saturating_sub(CACHED_INDEXES);
        cache.drain(..excess);
    }

    /// The full file set of a release, resolved through its delta chain.
    fn release_index(&self, seq: u64) -> Result<Arc<Index>> {
        let key = format!("release:{seq}");
        if let Some(index) = self.cached(&key) {
            return Ok(index);
        }
        let mut chain = vec![self.read_manifest(seq)?];
        while let Some(base) = chain.last().and_then(|m| m.base) {
            chain.push(self.read_manifest(base)?);
        }
        let mut files = Index::new();
        for manifest in chain.iter().rev() {
            files = manifest.apply_to(&files);
        }
        let index = Arc::new(files);
        self.remember(key, index.clone());
        Ok(index)
    }

    fn preview_index(&self, name: &str) -> Result<Arc<Index>> {
        let key = format!("preview:{name}");
        if let Some(index) = self.cached(&key) {
            return Ok(index);
        }
        let path = self.root.join("previews").join(format!("{name}.json"));
        let bytes = std::fs::read(&path).map_err(io(&path))?;
        let index: Arc<Index> = Arc::new(
            serde_json::from_slice(&bytes)
                .map_err(|e| SiteError::serde(path.display().to_string(), e))?,
        );
        self.remember(key, index.clone());
        Ok(index)
    }

    fn index(&self, pointer: &Pointer) -> Result<Arc<Index>> {
        match &pointer.preview {
            Some(name) => self.preview_index(name),
            None => self.release_index(pointer.files),
        }
    }

    /// Writes a full directory of what `pointer` serves, hard links into the store, and
    /// returns its name relative to the root.
    fn materialise_dir(&self, pointer: &Pointer, dir_name: &str) -> Result<String> {
        let final_dir = self.root.join(dir_name);
        if final_dir.is_dir() {
            return Ok(dir_name.to_string());
        }
        let index = self.index(pointer)?;
        let group = dir_name.split('/').next().unwrap_or("releases");
        let staging = self.root.join(group).join(format!(".staging-{}", nanos()));
        let result = (|| -> Result<()> {
            for (path, entry) in index.iter() {
                link(&self.object(&entry.sha256), &staging.join(relative(path)?))?;
            }
            if pointer.preview.is_none() {
                for seq in 1..=pointer.log {
                    let manifest = self.manifest_file(seq);
                    if manifest.exists() {
                        link(&manifest, &staging.join(manifest_path(seq)))?;
                    }
                }
                let log = self.root.join("log");
                for file in crate::generator::list_files(&log)? {
                    link(&log.join(&file), &staging.join(LOG_PREFIX).join(&file))?;
                }
                let checkpoint = self.checkpoint_file(pointer.log);
                if checkpoint.exists() {
                    link(&checkpoint, &staging.join(CHECKPOINT_PATH))?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        std::fs::create_dir_all(final_dir.parent().unwrap_or(&self.root))
            .map_err(io(&self.root))?;
        std::fs::rename(&staging, &final_dir).map_err(io(&final_dir))?;
        Ok(dir_name.to_string())
    }

    fn publish(&self, release: &StagedRelease, target: HostTarget) -> Result<PublishReport> {
        let start = Instant::now();
        let seq = release.manifest.seq;
        match target {
            HostTarget::Preview => self.publish_preview(release, start),
            HostTarget::Production => {
                if self.manifest_file(seq).exists() {
                    return Err(SiteError::Sequence(format!(
                        "release {seq} is already published on this host"
                    )));
                }
                if let Some(previous) = release.manifest.previous.as_ref()
                    && let Some(bytes) = read_optional(&self.manifest_file(seq - 1))?
                    && file_digest(&bytes) != *previous
                {
                    return Err(SiteError::Sequence(format!(
                        "release {seq} follows a manifest this host does not serve as release {}",
                        seq - 1
                    )));
                }
                self.publish_production(release, start)
            }
        }
    }

    /// Stores every file the current pointer does not already serve with the same digest;
    /// returns the paths uploaded.
    fn store_files(&self, release: &StagedRelease, current: Option<&Index>) -> Result<Vec<String>> {
        let mut uploaded = Vec::new();
        for (path, entry) in &release.files {
            let served = current.and_then(|index| index.get(path)) == Some(entry);
            if served && !release.changed.contains(path) {
                continue;
            }
            let written = self.store_object(release, path, entry)?;
            if written || release.changed.contains(path) {
                uploaded.push(path.clone());
            }
        }
        Ok(uploaded)
    }

    fn publish_production(&self, release: &StagedRelease, start: Instant) -> Result<PublishReport> {
        let seq = release.manifest.seq;
        let current_pointer = self.pointer(HostTarget::Production)?;
        let current = match &current_pointer {
            Some(pointer) => Some(self.index(pointer)?),
            None => None,
        };
        let mut uploaded = self.store_files(release, current.as_deref())?;
        write_atomic(&self.manifest_file(seq), &release.manifest_bytes)?;
        uploaded.push(format!("/{}", manifest_path(seq)));
        let mut log_seq = current_pointer.as_ref().map_or(0, |p| p.log);
        if let Some(log) = &release.log {
            for (path, bytes) in &log.tiles {
                let rest = path.trim_start_matches(LOG_PREFIX).trim_start_matches('/');
                let file = self.root.join("log").join(relative(rest)?);
                match read_optional(&file)? {
                    Some(existing) if existing == bytes.as_ref() => {}
                    Some(_) => {
                        return Err(SiteError::host(
                            NAME,
                            format!(
                                "log tile '{path}' already holds other bytes; tiles never change"
                            ),
                        ));
                    }
                    None => write_atomic(&file, bytes)?,
                }
                uploaded.push(format!("/{path}"));
            }
            if let Some((path, note)) = &log.checkpoint {
                write_atomic(&self.checkpoint_file(seq), note)?;
                uploaded.push(format!("/{path}"));
                log_seq = seq;
            }
        }
        let index = Arc::new(release.files.clone());
        self.remember(format!("release:{seq}"), index);
        let mut pointer = Pointer { files: seq, log: log_seq, preview: None, dir: None };
        if self.materialise {
            pointer.dir = Some(self.materialise_dir(&pointer, &format!("releases/{seq}"))?);
        }
        self.switch(HostTarget::Production, &pointer)?;
        if let Some(keep) = self.retain
            && seq.is_multiple_of(keep.div_ceil(2).max(1))
        {
            self.collect(keep, &pointer)?;
        }
        Ok(PublishReport {
            host: NAME.into(),
            target: HostTarget::Production,
            seq,
            uploaded,
            removed: release.removed.iter().cloned().collect(),
            elapsed: start.elapsed(),
        })
    }

    fn publish_preview(&self, release: &StagedRelease, start: Instant) -> Result<PublishReport> {
        let seq = release.manifest.seq;
        let name = format!("{seq}-{}", &release.digest.hex()[..12]);
        let mut pointer = Pointer { files: seq, log: 0, preview: Some(name.clone()), dir: None };
        let files = self.root.join("previews").join(format!("{name}.json"));
        let mut uploaded = Vec::new();
        if !files.exists() {
            uploaded = self.store_files(release, None)?;
            let index = serde_json::to_vec(&release.files)
                .map_err(|e| SiteError::serde("preview file set", e))?;
            write_atomic(&files, &index)?;
            uploaded.push(format!("/{}", manifest_path(seq)));
        }
        if self.materialise {
            pointer.dir = Some(self.materialise_dir(&pointer, &format!("previews/{name}"))?);
        }
        self.switch(HostTarget::Preview, &pointer)?;
        Ok(PublishReport {
            host: NAME.into(),
            target: HostTarget::Preview,
            seq,
            uploaded,
            removed: Vec::new(),
            elapsed: start.elapsed(),
        })
    }

    /// The oldest release whose files the host keeps.
    fn oldest_kept(&self, newest: u64) -> u64 {
        self.retain.map_or(1, |keep| newest.saturating_sub(keep).saturating_add(1).max(1))
    }

    /// Removes the objects and directories of releases older than the last `keep`, keeping
    /// every manifest, tile and checkpoint, and whatever a pointer serves.
    fn collect(&self, keep: u64, pointer: &Pointer) -> Result<()> {
        let oldest = pointer.log.saturating_sub(keep).saturating_add(1).max(1);
        let mut live: BTreeSet<Digest> = BTreeSet::new();
        live.extend(self.release_index(oldest)?.values().map(|e| e.sha256.clone()));
        for seq in oldest + 1..=pointer.log {
            if let Ok(manifest) = self.read_manifest(seq) {
                live.extend(manifest.files.values().map(|e| e.sha256.clone()));
            }
        }
        for target in [HostTarget::Production, HostTarget::Preview] {
            if let Some(served) = self.pointer(target)? {
                live.extend(self.index(&served)?.values().map(|e| e.sha256.clone()));
            }
        }
        let objects = self.root.join("objects");
        let mut removed = 0usize;
        for file in crate::generator::list_files(&objects)? {
            let hex = file.replace('/', "");
            let Ok(digest) = Digest::from_sha256_hex(&hex) else { continue };
            if !live.contains(&digest) {
                let path = objects.join(&file);
                std::fs::remove_file(&path).map_err(io(&path))?;
                removed += 1;
            }
        }
        let releases = self.root.join("releases");
        if let Ok(entries) = std::fs::read_dir(&releases) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let seq = name.parse::<u64>().ok();
                let served = pointer.dir.as_deref() == Some(&format!("releases/{name}"));
                if !served && seq.is_some_and(|seq| seq < oldest) {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        tracing::debug!(
            site.host.objects_removed = removed,
            site.host.oldest = oldest,
            "host storage collected"
        );
        Ok(())
    }

    fn rollback(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        let start = Instant::now();
        let current = self.pointer(HostTarget::Production)?.ok_or_else(|| {
            SiteError::ReleaseNotFound { seq: to.seq, message: "the host serves no release".into() }
        })?;
        if to.seq < self.oldest_kept(current.log) {
            return Err(SiteError::ReleaseNotFound {
                seq: to.seq,
                message: format!(
                    "outside the host's retention of {} releases",
                    self.retain.unwrap_or(0)
                ),
            });
        }
        let own = self.read_manifest(to.seq)?;
        let (expected, _) = manifest_digest(to)?;
        let (actual, _) = manifest_digest(&own)?;
        if expected != actual {
            return Err(SiteError::DigestMismatch {
                path: format!("/{}", manifest_path(to.seq)),
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
        let files = self.release_index(to.seq)?;
        for (path, entry) in files.iter() {
            let object = self.object(&entry.sha256);
            let actual = std::fs::read(&object)
                .map(|bytes| file_digest(&bytes).to_string())
                .unwrap_or_else(|_| "missing".into());
            if actual != entry.sha256.as_str() {
                return Err(SiteError::DigestMismatch {
                    path: path.clone(),
                    expected: entry.sha256.to_string(),
                    actual,
                });
            }
        }
        let before = self.index(&current)?;
        let mut pointer = Pointer { files: to.seq, log: current.log, preview: None, dir: None };
        if self.materialise {
            let name = format!("releases/rollback-{}-{}", to.seq, nanos());
            pointer.dir = Some(self.materialise_dir(&pointer, &name)?);
        }
        self.switch(HostTarget::Production, &pointer)?;
        Ok(PublishReport {
            host: NAME.into(),
            target: HostTarget::Production,
            seq: to.seq,
            uploaded: files.keys().cloned().collect(),
            removed: before.keys().filter(|p| !files.contains_key(*p)).cloned().collect(),
            elapsed: start.elapsed(),
        })
    }

    fn fetch(&self, path: &str) -> Result<Option<Bytes>> {
        let Some(pointer) = self.pointer(HostTarget::Production)? else { return Ok(None) };
        if let Some(dir) = &pointer.dir {
            let full = self.root.join(dir).join(relative(path)?);
            return Ok(read_optional(&full)?.map(Bytes::from));
        }
        let key = key_of(path)?;
        let file = if key == format!("/{CHECKPOINT_PATH}") {
            Some(self.checkpoint_file(pointer.log))
        } else if let Some(name) = key.strip_prefix(&format!("/{RELEASES_PREFIX}/")) {
            name.strip_suffix(".json")
                .and_then(|n| n.parse::<u64>().ok())
                .filter(|seq| (1..=pointer.log).contains(seq))
                .map(|seq| self.manifest_file(seq))
        } else if let Some(rest) = key.strip_prefix(&format!("/{LOG_PREFIX}/")) {
            Some(self.root.join("log").join(relative(rest)?))
        } else {
            self.index(&pointer)?.get(&key).map(|entry| self.object(&entry.sha256))
        };
        match file {
            Some(file) => Ok(read_optional(&file)?.map(Bytes::from)),
            None => Ok(None),
        }
    }

    fn serving_dir(&self, target: HostTarget) -> Result<Option<PathBuf>> {
        let Some(pointer) = self.pointer(target)? else { return Ok(None) };
        let dir = match &pointer.dir {
            Some(dir) => dir.clone(),
            None => {
                let name = match &pointer.preview {
                    Some(name) => format!("previews/{name}"),
                    None => format!("releases/view-{}-{}", pointer.files, pointer.log),
                };
                self.materialise_dir(&pointer, &name)?
            }
        };
        Ok(Some(self.root.join(dir)))
    }
}

impl DirectoryHost {
    /// A host rooted at `root`, created on first publish, that materialises every release
    /// as a directory and keeps every release.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            store: Arc::new(Store {
                root: root.into(),
                materialise: true,
                retain: None,
                cache: std::sync::Mutex::new(Vec::new()),
            }),
            lock: tokio::sync::Mutex::new(()),
        }
    }

    fn with(self, change: impl FnOnce(&mut Store)) -> Self {
        let mut store = Arc::try_unwrap(self.store).unwrap_or_else(|shared| Store {
            root: shared.root.clone(),
            materialise: shared.materialise,
            retain: shared.retain,
            cache: std::sync::Mutex::new(Vec::new()),
        });
        change(&mut store);
        Self { store: Arc::new(store), lock: self.lock }
    }

    /// Whether to write a full directory per release for a static file server; default
    /// `true`. Without one a publish costs what changed, and reads resolve through the
    /// pointer, as an edge in front of object storage does (spec 3.5 §7.8.3 "Hosts at
    /// speed").
    #[must_use]
    pub fn materialise(self, materialise: bool) -> Self {
        self.with(|store| store.materialise = materialise)
    }

    /// Keeps the files of the latest `releases` releases only (at least 2); older releases
    /// cannot be rolled back to. Manifests, tiles and checkpoints are always kept.
    #[must_use]
    pub fn retain_releases(self, releases: u64) -> Self {
        self.with(|store| store.retain = Some(releases.max(2)))
    }

    /// The host's root directory.
    pub fn root(&self) -> &Path {
        &self.store.root
    }

    /// The directory `target` serves, if it serves a release. A host that does not
    /// materialise releases writes one now.
    ///
    /// # Errors
    ///
    /// [`SiteError::Io`] when the pointer or the store cannot be read.
    pub fn serving_dir(&self, target: HostTarget) -> Result<Option<PathBuf>> {
        self.store.serving_dir(target)
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || work(&store))
            .await
            .map_err(|e| SiteError::host(NAME, e.to_string()))?
    }
}

#[async_trait]
impl SiteHost for DirectoryHost {
    fn name(&self) -> &str {
        NAME
    }

    async fn publish(&self, release: &StagedRelease, target: HostTarget) -> Result<PublishReport> {
        let _guard = self.lock.lock().await;
        let release = release.clone();
        self.blocking(move |store| store.publish(&release, target)).await
    }

    async fn rollback(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        let _guard = self.lock.lock().await;
        let to = to.clone();
        self.blocking(move |store| store.rollback(&to)).await
    }

    async fn fetch(&self, path: &str) -> Result<Option<Bytes>> {
        let path = path.to_string();
        self.blocking(move |store| store.fetch(&path)).await
    }

    fn redirects_format(&self) -> RedirectsFormat {
        RedirectsFormat::Json
    }

    fn limits(&self) -> HostLimits {
        HostLimits {
            max_publishes_per_hour: None,
            delta_upload: true,
            atomic: Atomicity::PrefixPointer,
        }
    }

    async fn publish_delta(
        &self,
        delta: &StagedDelta,
        target: HostTarget,
    ) -> Result<PublishReport> {
        self.publish(delta.release(), target).await
    }
}
