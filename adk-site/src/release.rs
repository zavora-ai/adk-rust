//! Releases: the build output hashed into a manifest, refused when it holds server code,
//! appended to the site log (spec 3.5 §7.7, §7.8.2 step 5, §7.8.3 "Releases at scale",
//! A-I1, A-I4; AWP 2.0 §14.1–§14.4).
//!
//! [`ReleaseBuilder::stage`] hashes every file of a build into an awp-types
//! [`ReleaseManifest`] (a snapshot, or a delta against the previous release) and refuses the
//! whole release when any file is server code. [`ReleaseBuilder::release`] then appends the
//! manifest digest to the [`SiteLog`] and attaches the tiles and checkpoint a host publishes
//! last. A preview release is staged and never logged.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_auth::signed_audit::canonicalize;
use adk_core::{Clock, SystemClock};
use awp_types::{
    Digest, FileEntry, GeneratorRef, LogEntry, Redirect, ReleaseManifest, ReleaseSource,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use sha2::{Digest as _, Sha256};

use crate::error::{Result, SiteError};
use crate::log::{LOG_PREFIX, LogPublication, SiteLog};
use crate::scan::scan_file;

/// Where release manifests are published, relative to the site root.
pub const RELEASES_PREFIX: &str = ".well-known/awp/releases";

/// The manifest path of release `seq`, relative to the site root.
pub fn manifest_path(seq: u64) -> String {
    format!("{RELEASES_PREFIX}/{seq}.json")
}

/// Whether a path (with or without a leading `/`) is under the releases or log prefix,
/// which the log authenticates and no manifest lists (AWP 2.0 §14.2).
pub fn is_log_path(path: &str) -> bool {
    let path = path.trim_start_matches('/');
    path.starts_with(&format!("{RELEASES_PREFIX}/")) || path.starts_with(&format!("{LOG_PREFIX}/"))
}

/// The media type a file is published with, from its name.
pub fn content_type(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with(".awp.json") || path.trim_start_matches('/') == ".well-known/awp.json" {
        return "application/awp+json";
    }
    if name == "_redirects" || name == "security.txt" {
        return "text/plain; charset=utf-8";
    }
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("md") => "text/markdown; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("xml") if name == "feed.xml" => "application/atom+xml",
        Some("xml") => "application/xml",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("pdf") => "application/pdf",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        _ => "application/octet-stream",
    }
}

/// The digest of a manifest: SHA-256 of its JCS form (AWP 2.0 §14.1).
///
/// # Errors
///
/// [`SiteError::Serde`] when the manifest holds a number JCS cannot carry.
pub fn manifest_digest(manifest: &ReleaseManifest) -> Result<(Digest, Bytes)> {
    let value =
        serde_json::to_value(manifest).map_err(|e| SiteError::serde("release manifest", e))?;
    let canonical = canonicalize(&value).map_err(|e| SiteError::serde("release manifest", e))?;
    let digest = Digest::from_sha256_bytes(Sha256::digest(canonical.as_bytes()).into());
    Ok((digest, Bytes::from(canonical.into_bytes())))
}

/// A file's digest.
pub fn file_digest(bytes: &[u8]) -> Digest {
    Digest::from_sha256_bytes(Sha256::digest(bytes).into())
}

/// The release a new one follows.
#[derive(Debug, Clone)]
pub struct PreviousRelease {
    /// Its manifest.
    pub manifest: ReleaseManifest,
    /// Its manifest digest.
    pub digest: Digest,
    /// Its full file set (resolved through its delta chain).
    pub files: BTreeMap<String, FileEntry>,
    /// The latest snapshot at or before it: its number and build time.
    pub last_snapshot: (u64, DateTime<Utc>),
}

impl PreviousRelease {
    /// The previous release from a staged one.
    pub fn from_staged(staged: &StagedRelease) -> Self {
        let last_snapshot = match &staged.previous_snapshot {
            Some(snapshot) if !staged.manifest.is_snapshot() => *snapshot,
            _ => (staged.manifest.seq, staged.manifest.created_at),
        };
        Self {
            manifest: staged.manifest.clone(),
            digest: staged.digest.clone(),
            files: staged.files.clone(),
            last_snapshot,
        }
    }
}

/// What a release is made of besides its files.
#[derive(Debug, Clone)]
pub struct ReleaseOptions {
    /// What produced it.
    pub source: ReleaseSource,
    /// The generator that built it.
    pub generator: GeneratorRef,
    /// The charter version in effect.
    pub charter_version: u32,
    /// Every redirect.
    pub redirects: Vec<Redirect>,
    /// Build only the files the generator wrote again; others keep the previous entries.
    pub written: Option<BTreeSet<String>>,
    /// Ask for a snapshot even when a delta would do.
    pub force_snapshot: bool,
}

/// A release ready for a host: its manifest and where its files are.
#[derive(Debug, Clone)]
pub struct StagedRelease {
    /// The manifest.
    pub manifest: ReleaseManifest,
    /// The manifest's digest.
    pub digest: Digest,
    /// The manifest's JCS bytes, published at [`manifest_path`].
    pub manifest_bytes: Bytes,
    /// The directory holding the release's files, at their paths without the leading `/`.
    pub root: PathBuf,
    /// The full file set.
    pub files: BTreeMap<String, FileEntry>,
    /// Paths added or changed since the previous release: what a host uploads.
    pub changed: BTreeSet<String>,
    /// Paths the previous release had and this one does not.
    pub removed: BTreeSet<String>,
    /// The log's tiles and checkpoint, for a logged (production) release.
    pub log: Option<LogPublication>,
    /// The last snapshot before this release, when this one is a delta.
    pub previous_snapshot: Option<(u64, DateTime<Utc>)>,
}

impl StagedRelease {
    /// The local path of a release file (`/a/b.html` or `a/b.html`).
    pub fn local_path(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    /// Reads a release file's bytes and checks them against the manifest.
    ///
    /// # Errors
    ///
    /// [`SiteError::Io`] when the file cannot be read; [`SiteError::DigestMismatch`] when it
    /// changed after staging.
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let local = self.local_path(path);
        let bytes = std::fs::read(&local).map_err(|e| SiteError::io(&local, e))?;
        if let Some(entry) = self.files.get(path) {
            let actual = file_digest(&bytes);
            if actual != entry.sha256 {
                return Err(SiteError::DigestMismatch {
                    path: path.to_string(),
                    expected: entry.sha256.to_string(),
                    actual: actual.to_string(),
                });
            }
        }
        Ok(bytes)
    }
}

/// A delta release: its manifest names a `base` and lists only changes (spec 3.5 §7.8.3).
#[derive(Debug, Clone)]
pub struct StagedDelta(StagedRelease);

impl StagedDelta {
    /// Wraps a staged delta release.
    ///
    /// # Errors
    ///
    /// [`SiteError::Sequence`] when the release is a snapshot.
    pub fn new(release: StagedRelease) -> Result<Self> {
        if release.manifest.is_snapshot() {
            return Err(SiteError::Sequence(format!(
                "release {} is a snapshot, not a delta",
                release.manifest.seq
            )));
        }
        Ok(Self(release))
    }

    /// The release.
    pub fn release(&self) -> &StagedRelease {
        &self.0
    }
}

/// Hashes builds into signed, logged releases (spec 3.5 §7.7).
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::release::{ReleaseBuilder, ReleaseOptions};
/// use awp_types::ReleaseSource;
///
/// # async fn demo(out: &std::path::Path, log: &adk_site::log::SiteLog) -> adk_site::Result<()> {
/// let builder = ReleaseBuilder::new();
/// let options = ReleaseOptions {
///     source: ReleaseSource::Onboarding,
///     generator: adk_site::generator::BuiltinGenerator::reference(),
///     charter_version: 1,
///     redirects: Vec::new(),
///     written: None,
///     force_snapshot: false,
/// };
/// let staged = builder.release(out, None, options, log).await?;
/// assert_eq!(staged.manifest.seq, 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ReleaseBuilder {
    clock: Arc<dyn Clock>,
    snapshot_every: u64,
    snapshot_interval: chrono::Duration,
}

impl Default for ReleaseBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ReleaseBuilder {
    /// A builder that writes a snapshot every 100 releases and at least daily.
    pub fn new() -> Self {
        Self {
            clock: Arc::new(SystemClock),
            snapshot_every: 100,
            snapshot_interval: chrono::Duration::days(1),
        }
    }

    /// Stamps manifests with `clock`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Writes a snapshot every `releases` releases (at least 1).
    pub fn snapshot_every(mut self, releases: u64) -> Self {
        self.snapshot_every = releases.max(1);
        self
    }

    /// Stages a release of the build in `out` without logging it: a preview, or the first
    /// half of [`release`](Self::release).
    ///
    /// # Errors
    ///
    /// [`SiteError::ServerCode`] when any file is server code (A-I1), whatever the options;
    /// [`SiteError::InvalidContent`] for a redirect with an unknown status or log paths in
    /// the build; I/O errors.
    pub async fn stage(
        &self,
        out: &Path,
        previous: Option<&PreviousRelease>,
        options: ReleaseOptions,
    ) -> Result<StagedRelease> {
        for redirect in &options.redirects {
            if !matches!(redirect.status, 301 | 302 | 307 | 308 | 410) {
                return Err(SiteError::InvalidContent {
                    path: redirect.from.clone(),
                    message: format!(
                        "redirect status {} is not 301, 302, 307, 308 or 410",
                        redirect.status
                    ),
                });
            }
        }
        let out_owned = out.to_path_buf();
        let reuse: Option<BTreeMap<String, FileEntry>> = match (&options.written, previous) {
            (Some(_), Some(previous)) => Some(previous.files.clone()),
            _ => None,
        };
        let written = options.written.clone();
        let files = tokio::task::spawn_blocking(move || {
            hash_tree(&out_owned, reuse.as_ref(), written.as_ref())
        })
        .await
        .map_err(|e| SiteError::Pipeline(format!("hashing task failed: {e}")))??;
        let seq = previous.map_or(1, |p| p.manifest.seq + 1);
        let now = self.clock.now();
        let snapshot = match previous {
            None => true,
            Some(previous) => {
                options.force_snapshot
                    || seq - previous.last_snapshot.0 >= self.snapshot_every
                    || now - previous.last_snapshot.1 >= self.snapshot_interval
            }
        };
        let empty = BTreeMap::new();
        let before = previous.map_or(&empty, |p| &p.files);
        let changed: BTreeSet<String> = files
            .iter()
            .filter(|(path, entry)| before.get(*path) != Some(entry))
            .map(|(path, _)| path.clone())
            .collect();
        let removed: BTreeSet<String> =
            before.keys().filter(|p| !files.contains_key(*p)).cloned().collect();
        let manifest = ReleaseManifest {
            seq,
            previous: previous.map(|p| p.digest.clone()),
            base: (!snapshot).then(|| seq - 1),
            created_at: now,
            files: if snapshot {
                files.clone()
            } else {
                changed.iter().map(|p| (p.clone(), files[p].clone())).collect()
            },
            removed: if snapshot { BTreeSet::new() } else { removed.clone() },
            redirects: options.redirects,
            source: options.source,
            generator: options.generator,
            charter_version: options.charter_version,
        };
        let (digest, manifest_bytes) = manifest_digest(&manifest)?;
        tracing::info!(
            site.release.seq = seq,
            site.release.files = files.len(),
            site.release.changed = changed.len(),
            snapshot,
            "release staged"
        );
        Ok(StagedRelease {
            manifest,
            digest,
            manifest_bytes,
            root: out.to_path_buf(),
            files,
            changed,
            removed,
            log: None,
            previous_snapshot: (!snapshot).then(|| previous.map(|p| p.last_snapshot)).flatten(),
        })
    }

    /// Stages a release and appends its manifest digest to `log`, attaching the tiles and
    /// signed checkpoint a host publishes after the files (spec 3.5 §7.7).
    ///
    /// # Errors
    ///
    /// As [`stage`](Self::stage); [`SiteError::Sequence`] when the log is not at the
    /// previous release; log and signing errors.
    pub async fn release(
        &self,
        out: &Path,
        previous: Option<&PreviousRelease>,
        options: ReleaseOptions,
        log: &SiteLog,
    ) -> Result<StagedRelease> {
        let mut staged = self.stage(out, previous, options).await?;
        let before = log.size().await;
        log.append(&LogEntry { seq: staged.manifest.seq, manifest: staged.digest.clone() }).await?;
        staged.log = Some(log.publication(before).await?);
        tracing::info!(site.release.seq = staged.manifest.seq, site.release.digest = %staged.digest, "release logged");
        Ok(staged)
    }
}

/// Hashes every file under `out`, scanning each for server code; entries for files not in
/// `written` are taken from `reuse` when present.
fn hash_tree(
    out: &Path,
    reuse: Option<&BTreeMap<String, FileEntry>>,
    written: Option<&BTreeSet<String>>,
) -> Result<BTreeMap<String, FileEntry>> {
    let mut files = BTreeMap::new();
    let mut findings = Vec::new();
    for relative in crate::generator::list_files(out)? {
        let key = format!("/{relative}");
        if is_log_path(&key) {
            return Err(SiteError::InvalidContent {
                path: relative,
                message: "the build output holds a release or log path; those are written by the release builder".into(),
            });
        }
        if let (Some(reuse), Some(written)) = (reuse, written)
            && !written.contains(&relative)
            && let Some(entry) = reuse.get(&key)
        {
            files.insert(key, entry.clone());
            continue;
        }
        let path = out.join(&relative);
        let bytes = std::fs::read(&path).map_err(|e| SiteError::io(&path, e))?;
        findings.extend(scan_file(&relative, &bytes));
        files.insert(
            key,
            FileEntry {
                sha256: file_digest(&bytes),
                size: bytes.len() as u64,
                content_type: content_type(&relative).into(),
            },
        );
    }
    if !findings.is_empty() {
        tracing::warn!(
            site.release.refused = findings.len(),
            "release refused: server code (A-I1)"
        );
        return Err(SiteError::ServerCode { findings });
    }
    Ok(files)
}

/// Resolves a release's full file set from its manifest and its base's full set
/// (AWP 2.0 §14.4).
pub fn full_file_set(
    manifest: &ReleaseManifest,
    base: &BTreeMap<String, FileEntry>,
) -> BTreeMap<String, FileEntry> {
    manifest.apply_to(base)
}
