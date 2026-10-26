//! Object storage hosts: S3-compatible stores, Google Cloud Storage, Azure Blob Storage and
//! Cloudflare R2 behind one [`SiteHost`] (spec 3.5 §7.8 "Hosts", §7.8.3 "Hosts at speed",
//! Q16).
//!
//! An [`ObjectStore`] is a bucket or container: put, get, server-side copy and delete.
//! [`ObjectStoreHost`] lays releases out in it in one of two ways:
//!
//! | Layout | Served from | Atomicity | Needs |
//! |--------|-------------|-----------|-------|
//! | [`ObjectStoreHost::prefix_pointer`] | `releases/<seq>/`, named by the `current.pointer` object | [`Atomicity::PrefixPointer`] | An edge that reads the pointer and rewrites `/path` to `<pointer>/path` (a Cloudflare Worker, Lambda@Edge, any proxy) |
//! | [`ObjectStoreHost::in_place`] | The live store's root, written in place | [`Atomicity::None`] | A website endpoint on the store (S3 website hosting, Azure `$web`, a GCS website bucket); batch windows of a minute or more |
//!
//! Both keep every release in its own prefix — in the store itself, or in a private
//! archive store for the in-place layout — with an index of its files and digests, so a
//! publish uploads only changed files and copies the rest server-side, and a rollback
//! serves an earlier release's exact files. A [`CachePurge`] hook purges the CDN after the
//! switch.
//!
//! | Key | Holds |
//! |-----|-------|
//! | `releases/<seq>/…` | A production release: its files, every manifest so far and the log |
//! | `releases/rollback-<seq>-<n>/…` | A rollback: release `<seq>`'s files with the log as it stands |
//! | `previews/<seq>-<digest>/…` | A preview release |
//! | `<dir>.index.json` | The digest of every file under `<dir>/`, written after them |
//! | `current.pointer`, `preview.pointer` | The directory each target serves, written last |

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use awp_types::{Atomicity, Digest, FileEntry, HostLimits, ReleaseManifest};
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};

use super::keys::site_key;
use super::{HostTarget, PublishReport, SiteHost};
use crate::error::{Result, SiteError};
use crate::log::CHECKPOINT_PATH;
use crate::redirects::RedirectsFormat;
use crate::release::{
    StagedDelta, StagedRelease, content_type, file_digest, is_log_path, manifest_digest,
    manifest_path,
};

/// The pointer object the production site follows.
pub const PRODUCTION_POINTER: &str = "current.pointer";
/// The pointer object previews follow.
pub const PREVIEW_POINTER: &str = "preview.pointer";
/// The `Cache-Control` the pointers are written with, so an edge re-reads them.
pub const POINTER_CACHE_CONTROL: &str = "no-cache, max-age=0";

/// How an object is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    /// The media type it is served with.
    pub content_type: String,
    /// The `Cache-Control` it is served with, if any.
    pub cache_control: Option<String>,
}

/// A bucket or container a host stores releases in (spec 3.5 §7.8 "Hosts").
///
/// Keys are relative to the store's own prefix and never start with `/`. Implementations:
/// `S3Store` (`host-s3`, also Cloudflare R2), `GcsStore` (`host-gcs`), `AzureBlobStore`
/// (`host-azure`), and [`InMemoryObjectStore`] for tests.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_site::host::object::{InMemoryObjectStore, ObjectMeta, ObjectStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_site::Result<()> {
/// let store = InMemoryObjectStore::new();
/// let meta = ObjectMeta { content_type: "text/plain".into(), cache_control: None };
/// store.put("a.txt", "hello".into(), &meta).await?;
/// store.copy("a.txt", "b.txt").await?;
/// assert_eq!(store.get("b.txt").await?.as_deref(), Some(b"hello".as_slice()));
/// store.delete("a.txt").await?;
/// assert_eq!(store.get("a.txt").await?, None);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait ObjectStore: Send + Sync + fmt::Debug {
    /// The store's name for reports and errors.
    fn name(&self) -> &str;

    /// Writes an object, replacing any object at `key`.
    ///
    /// # Errors
    ///
    /// [`SiteError::Host`] when the store refuses or cannot be reached.
    async fn put(&self, key: &str, body: Bytes, meta: &ObjectMeta) -> Result<()>;

    /// Reads an object; `None` when there is none at `key`.
    ///
    /// # Errors
    ///
    /// [`SiteError::Host`] when the store refuses or cannot be reached.
    async fn get(&self, key: &str) -> Result<Option<Bytes>>;

    /// Copies an object inside the store without downloading it, keeping its metadata.
    ///
    /// # Errors
    ///
    /// [`SiteError::Host`] when `from` does not exist or the store refuses.
    async fn copy(&self, from: &str, to: &str) -> Result<()>;

    /// Deletes an object; deleting a missing object succeeds.
    ///
    /// # Errors
    ///
    /// [`SiteError::Host`] when the store refuses or cannot be reached.
    async fn delete(&self, key: &str) -> Result<()>;
}

/// Purges a CDN's cached copies of changed paths after a host switches releases (spec 3.5
/// §7.8.2 step 6 "The host swaps atomically and purges its cache").
///
/// A purge runs after the release is live; a failed purge is logged and does not fail the
/// publish, because the previous release is no longer served.
///
/// # Example
///
/// ```rust
/// use adk_site::host::object::CachePurge;
///
/// #[derive(Debug)]
/// struct Recorder;
///
/// #[async_trait::async_trait]
/// impl CachePurge for Recorder {
///     async fn purge(&self, paths: &[String]) -> adk_site::Result<()> {
///         tracing::info!(paths = paths.len(), "purged");
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait CachePurge: Send + Sync + fmt::Debug {
    /// Purges `paths`: site paths with a leading `/`, each file once.
    ///
    /// # Errors
    ///
    /// [`SiteError::Host`] when the CDN refuses.
    async fn purge(&self, paths: &[String]) -> Result<()>;
}

/// An [`ObjectStore`] in memory, for tests and examples.
#[derive(Debug, Default)]
pub struct InMemoryObjectStore {
    objects: tokio::sync::RwLock<BTreeMap<String, (Bytes, ObjectMeta)>>,
}

impl InMemoryObjectStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every key, in order.
    pub async fn keys(&self) -> Vec<String> {
        self.objects.read().await.keys().cloned().collect()
    }

    /// The metadata of the object at `key`.
    pub async fn meta(&self, key: &str) -> Option<ObjectMeta> {
        self.objects.read().await.get(key).map(|(_, meta)| meta.clone())
    }
}

#[async_trait]
impl ObjectStore for InMemoryObjectStore {
    fn name(&self) -> &str {
        "memory"
    }

    async fn put(&self, key: &str, body: Bytes, meta: &ObjectMeta) -> Result<()> {
        self.objects.write().await.insert(key.to_string(), (body, meta.clone()));
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        Ok(self.objects.read().await.get(key).map(|(bytes, _)| bytes.clone()))
    }

    async fn copy(&self, from: &str, to: &str) -> Result<()> {
        let mut objects = self.objects.write().await;
        let object = objects
            .get(from)
            .cloned()
            .ok_or_else(|| SiteError::host("memory", format!("copy source '{from}' is missing")))?;
        objects.insert(to.to_string(), object);
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.objects.write().await.remove(key);
        Ok(())
    }
}

/// The digest of every file under a release directory, by site path.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirIndex {
    files: BTreeMap<String, Digest>,
}

impl DirIndex {
    fn site_files(&self) -> impl Iterator<Item = (&String, &Digest)> {
        self.files.iter().filter(|(path, _)| !is_log_path(path))
    }
}

#[derive(Debug)]
enum Layout {
    PrefixPointer,
    InPlace { live: Arc<dyn ObjectStore> },
}

/// A [`SiteHost`] over an [`ObjectStore`] (spec 3.5 §7.8 "Hosts", §7.8.3 `PrefixPointer`).
///
/// See the [module documentation](self) for the two layouts.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_site::host::object::{InMemoryObjectStore, ObjectStoreHost};
/// use adk_site::host::SiteHost;
/// use awp_types::Atomicity;
///
/// let host = ObjectStoreHost::prefix_pointer(Arc::new(InMemoryObjectStore::new()))
///     .with_concurrency(32);
/// assert_eq!(host.limits().atomic, Atomicity::PrefixPointer);
///
/// let website = ObjectStoreHost::in_place(
///     Arc::new(InMemoryObjectStore::new()),
///     Arc::new(InMemoryObjectStore::new()),
/// );
/// assert_eq!(website.limits().atomic, Atomicity::None);
/// ```
#[derive(Debug)]
pub struct ObjectStoreHost {
    name: String,
    store: Arc<dyn ObjectStore>,
    layout: Layout,
    purge: Option<Arc<dyn CachePurge>>,
    redirects: RedirectsFormat,
    concurrency: usize,
    cache_control: Option<String>,
    max_publishes_per_hour: Option<u32>,
    lock: tokio::sync::Mutex<()>,
}

fn nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

fn index_key(dir: &str) -> String {
    format!("{dir}.index.json")
}

fn in_dir(dir: &str, key: &str) -> String {
    format!("{dir}/{key}")
}

/// The key of a site path that the release builder produced (`/a/b.html`).
fn release_key(path: &str) -> &str {
    path.trim_start_matches('/')
}

fn meta_for(path: &str, cache_control: Option<&String>) -> ObjectMeta {
    let content_type = if path.trim_start_matches('/') == CHECKPOINT_PATH {
        "text/plain; charset=utf-8"
    } else {
        content_type(path)
    };
    ObjectMeta { content_type: content_type.into(), cache_control: cache_control.cloned() }
}

impl ObjectStoreHost {
    fn new(store: Arc<dyn ObjectStore>, layout: Layout) -> Self {
        Self {
            name: store.name().to_string(),
            store,
            layout,
            purge: None,
            redirects: RedirectsFormat::HtmlMeta,
            concurrency: 16,
            cache_control: None,
            max_publishes_per_hour: None,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Releases under their own prefixes in `store`, switched by a pointer written last
    /// ([`Atomicity::PrefixPointer`]). The edge in front of the store reads
    /// [`PRODUCTION_POINTER`] and serves `/path` from `<pointer>/path`.
    pub fn prefix_pointer(store: Arc<dyn ObjectStore>) -> Self {
        Self::new(store, Layout::PrefixPointer)
    }

    /// The site written in place at the root of `live` — a website endpoint — with every
    /// release kept in the private `archive` for exact rollback ([`Atomicity::None`]).
    ///
    /// `archive` must not be served: a file there that is not in the latest release would
    /// be drift (A-I4).
    pub fn in_place(live: Arc<dyn ObjectStore>, archive: Arc<dyn ObjectStore>) -> Self {
        let mut host = Self::new(archive, Layout::InPlace { live: live.clone() });
        host.name = live.name().to_string();
        host
    }

    /// Names the host in reports.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Purges `purge`'s CDN after each switch.
    #[must_use]
    pub fn with_cache_purge(mut self, purge: Arc<dyn CachePurge>) -> Self {
        self.purge = Some(purge);
        self
    }

    /// The redirect format the edge in front of the store reads; [`RedirectsFormat::HtmlMeta`]
    /// by default, which needs no edge rules.
    #[must_use]
    pub fn with_redirects_format(mut self, format: RedirectsFormat) -> Self {
        self.redirects = format;
        self
    }

    /// How many requests run at once; 16 by default.
    #[must_use]
    pub fn with_concurrency(mut self, requests: usize) -> Self {
        self.concurrency = requests.max(1);
        self
    }

    /// The `Cache-Control` site files are written with.
    #[must_use]
    pub fn with_cache_control(mut self, value: impl Into<String>) -> Self {
        self.cache_control = Some(value.into());
        self
    }

    /// Declares the provider's publication quota in [`limits`](SiteHost::limits).
    #[must_use]
    pub fn with_max_publishes_per_hour(mut self, publishes: u32) -> Self {
        self.max_publishes_per_hour = Some(publishes);
        self
    }

    fn pointer_key(target: HostTarget) -> &'static str {
        match target {
            HostTarget::Production => PRODUCTION_POINTER,
            HostTarget::Preview => PREVIEW_POINTER,
        }
    }

    /// The release directory `target` serves, if any.
    ///
    /// # Errors
    ///
    /// [`SiteError::Host`] when the store cannot be read.
    pub async fn serving_dir(&self, target: HostTarget) -> Result<Option<String>> {
        let Some(bytes) = self.store.get(Self::pointer_key(target)).await? else {
            return Ok(None);
        };
        let dir = String::from_utf8_lossy(&bytes).trim().to_string();
        if dir.is_empty() || dir.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
            return Err(SiteError::host(
                &self.name,
                format!("pointer holds an invalid directory '{dir}'"),
            ));
        }
        Ok(Some(dir))
    }

    async fn read_index(&self, dir: &str) -> Result<Option<DirIndex>> {
        let Some(bytes) = self.store.get(&index_key(dir)).await? else { return Ok(None) };
        serde_json::from_slice(&bytes).map(Some).map_err(|e| SiteError::serde(index_key(dir), e))
    }

    async fn write_index(&self, dir: &str, index: &DirIndex) -> Result<()> {
        let bytes = serde_json::to_vec(index).map_err(|e| SiteError::serde(index_key(dir), e))?;
        let meta = ObjectMeta { content_type: "application/json".into(), cache_control: None };
        self.store.put(&index_key(dir), bytes.into(), &meta).await
    }

    async fn switch(&self, target: HostTarget, dir: &str) -> Result<()> {
        let meta = ObjectMeta {
            content_type: "text/plain; charset=utf-8".into(),
            cache_control: Some(POINTER_CACHE_CONTROL.into()),
        };
        self.store.put(Self::pointer_key(target), Bytes::from(dir.to_string()), &meta).await
    }

    /// Runs `futures` with at most [`concurrency`](Self::with_concurrency) at once.
    async fn each<'a, R>(&self, futures: Vec<BoxFuture<'a, Result<R>>>) -> Result<Vec<R>> {
        futures::stream::iter(futures).buffer_unordered(self.concurrency).try_collect().await
    }

    /// Writes a release into `dir`: changed files uploaded, unchanged ones copied from the
    /// serving directory, then the manifest, the log carried over and its new tiles, then
    /// the checkpoint and the index. Returns the uploaded paths in that order.
    async fn stage_dir(
        &self,
        release: &StagedRelease,
        dir: &str,
        current: Option<(&str, &DirIndex)>,
    ) -> Result<(Vec<String>, DirIndex)> {
        let mut index = DirIndex::default();
        let files: Vec<(&String, &FileEntry)> = release.files.iter().collect();
        let uploaded = self
            .each(
                files
                    .into_iter()
                    .map(|(path, entry)| {
                        async move {
                            let key = release_key(path);
                            let reuse = current
                                .filter(|(_, index)| index.files.get(path) == Some(&entry.sha256))
                                .map(|(cur, _)| cur);
                            match reuse {
                                Some(cur) => {
                                    self.store.copy(&in_dir(cur, key), &in_dir(dir, key)).await?;
                                    Ok(None)
                                }
                                None => {
                                    let bytes = release.read(path)?;
                                    let meta = meta_for(path, self.cache_control.as_ref());
                                    self.store.put(&in_dir(dir, key), bytes.into(), &meta).await?;
                                    Ok(Some(path.clone()))
                                }
                            }
                        }
                        .boxed()
                    })
                    .collect(),
            )
            .await?;
        let uploaded: BTreeSet<String> = uploaded.into_iter().flatten().collect();
        let mut order: Vec<String> =
            release.files.keys().filter(|p| uploaded.contains(*p)).cloned().collect();
        for (path, entry) in &release.files {
            index.files.insert(path.clone(), entry.sha256.clone());
        }

        let manifest = format!("/{}", manifest_path(release.manifest.seq));
        let log_files: Vec<(String, Bytes)> = release
            .log
            .as_ref()
            .map(|log| log.files().map(|(p, b)| (format!("/{p}"), b.clone())).collect())
            .unwrap_or_default();
        let written: BTreeSet<&str> = log_files
            .iter()
            .map(|(p, _)| p.as_str())
            .chain(std::iter::once(manifest.as_str()))
            .collect();
        if let Some((cur, cur_index)) = current {
            let carried: Vec<(&String, &Digest)> = cur_index
                .files
                .iter()
                .filter(|(p, _)| is_log_path(p) && !written.contains(p.as_str()))
                .collect();
            for (path, digest) in &carried {
                index.files.insert((*path).clone(), (*digest).clone());
            }
            self.each(
                carried
                    .into_iter()
                    .map(|(path, _)| {
                        async move {
                            let key = release_key(path);
                            self.store.copy(&in_dir(cur, key), &in_dir(dir, key)).await
                        }
                        .boxed()
                    })
                    .collect(),
            )
            .await?;
        }

        let manifest_meta = meta_for(&manifest, None);
        self.store
            .put(
                &in_dir(dir, release_key(&manifest)),
                release.manifest_bytes.clone(),
                &manifest_meta,
            )
            .await?;
        index.files.insert(manifest.clone(), file_digest(&release.manifest_bytes));
        order.push(manifest);

        let (checkpoint, tiles): (Vec<_>, Vec<_>) =
            log_files.into_iter().partition(|(p, _)| p.trim_start_matches('/') == CHECKPOINT_PATH);
        for (path, bytes) in tiles.iter().chain(checkpoint.iter()) {
            index.files.insert(path.clone(), file_digest(bytes));
        }
        self.each(
            tiles
                .clone()
                .into_iter()
                .map(|(path, bytes)| {
                    async move {
                        self.store
                            .put(&in_dir(dir, release_key(&path)), bytes, &meta_for(&path, None))
                            .await
                    }
                    .boxed()
                })
                .collect(),
        )
        .await?;
        order.extend(tiles.into_iter().map(|(p, _)| p));
        for (path, bytes) in checkpoint {
            self.store.put(&in_dir(dir, release_key(&path)), bytes, &meta_for(&path, None)).await?;
            order.push(path);
        }
        self.write_index(dir, &index).await?;
        Ok((order, index))
    }

    /// The bytes of `path` in the release directory `dir`, read from `release` where it
    /// holds them, else from the store.
    async fn dir_bytes(
        &self,
        dir: &str,
        release: Option<&StagedRelease>,
        path: &str,
    ) -> Result<Bytes> {
        if let Some(release) = release {
            if release.files.contains_key(path) {
                return release.read(path).map(Bytes::from);
            }
            if path.trim_start_matches('/') == manifest_path(release.manifest.seq) {
                return Ok(release.manifest_bytes.clone());
            }
            if let Some((_, bytes)) = release
                .log
                .iter()
                .flat_map(|log| log.files())
                .find(|(p, _)| p.as_str() == path.trim_start_matches('/'))
            {
                return Ok(bytes.clone());
            }
        }
        self.store.get(&in_dir(dir, release_key(path))).await?.ok_or_else(|| {
            SiteError::host(&self.name, format!("'{path}' is missing from '{dir}/'"))
        })
    }

    /// Writes the files of the release directory `dir` (indexed by `next`) that differ from
    /// `previous` to the live store in place: files, then manifests, tiles and the
    /// checkpoint, then removals. Returns the written and removed paths.
    async fn mirror_live(
        &self,
        live: &Arc<dyn ObjectStore>,
        previous: Option<&DirIndex>,
        next: &DirIndex,
        dir: &str,
        release: Option<&StagedRelease>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let differs = |path: &String, digest: &Digest| {
            previous.and_then(|p| p.files.get(path)) != Some(digest)
        };
        let site: Vec<String> =
            next.site_files().filter(|(p, d)| differs(p, d)).map(|(p, _)| p.clone()).collect();
        let (checkpoint, log): (Vec<String>, Vec<String>) = next
            .files
            .iter()
            .filter(|(p, d)| is_log_path(p) && differs(p, d))
            .map(|(p, _)| p.clone())
            .partition(|p| p.trim_start_matches('/') == CHECKPOINT_PATH);
        let (manifests, tiles): (Vec<String>, Vec<String>) = log
            .into_iter()
            .partition(|p| p.trim_start_matches('/').starts_with(crate::release::RELEASES_PREFIX));
        let mut written = Vec::new();
        for batch in [site, manifests, tiles, checkpoint] {
            self.each(
                batch
                    .iter()
                    .map(|path| {
                        async move {
                            let bytes = self.dir_bytes(dir, release, path).await?;
                            let cache =
                                if is_log_path(path) { None } else { self.cache_control.as_ref() };
                            live.put(release_key(path), bytes, &meta_for(path, cache)).await
                        }
                        .boxed()
                    })
                    .collect(),
            )
            .await?;
            written.extend(batch);
        }
        let removed: Vec<String> = previous
            .map(|p| {
                p.site_files()
                    .filter(|(path, _)| !next.files.contains_key(*path))
                    .map(|(p, _)| p.clone())
                    .collect()
            })
            .unwrap_or_default();
        self.each(removed.iter().map(|path| live.delete(release_key(path)).boxed()).collect())
            .await?;
        Ok((written, removed))
    }

    async fn purge(&self, report: &PublishReport) {
        let Some(purge) = &self.purge else { return };
        let paths: Vec<String> =
            report.uploaded.iter().chain(report.removed.iter()).cloned().collect();
        if paths.is_empty() {
            return;
        }
        if let Err(error) = purge.purge(&paths).await {
            tracing::warn!(host = %self.name, site.release.seq = report.seq, error = %error, "cache purge failed; the release is live");
        }
    }

    async fn publish_locked(
        &self,
        release: &StagedRelease,
        target: HostTarget,
    ) -> Result<PublishReport> {
        let start = Instant::now();
        let seq = release.manifest.seq;
        let dir = match target {
            HostTarget::Production => format!("releases/{seq}"),
            HostTarget::Preview => format!("previews/{seq}-{}", &release.digest.hex()[..12]),
        };
        let report = |uploaded, removed| PublishReport {
            host: self.name.clone(),
            target,
            seq,
            uploaded,
            removed,
            elapsed: start.elapsed(),
        };
        if self.read_index(&dir).await?.is_some() {
            if target == HostTarget::Preview {
                self.switch(target, &dir).await?;
                return Ok(report(Vec::new(), Vec::new()));
            }
            return Err(SiteError::Sequence(format!(
                "release {seq} is already published on this host"
            )));
        }
        let current_dir = self.serving_dir(target).await?;
        let current_index = match &current_dir {
            Some(cur) => self.read_index(cur).await?,
            None => None,
        };
        if target == HostTarget::Production
            && let Some(cur) = &current_dir
            && let Some(previous) = release.manifest.previous.as_ref()
            && let Some(bytes) = self.store.get(&in_dir(cur, &manifest_path(seq - 1))).await?
            && file_digest(&bytes) != *previous
        {
            return Err(SiteError::Sequence(format!(
                "release {seq} follows a manifest this host does not serve as release {}",
                seq - 1
            )));
        }
        let current = current_dir.as_deref().zip(current_index.as_ref());
        let (uploaded, index) = self.stage_dir(release, &dir, current).await?;
        let (uploaded, removed) = match (&self.layout, target) {
            (Layout::InPlace { live }, HostTarget::Production) => {
                self.mirror_live(live, current_index.as_ref(), &index, &dir, Some(release)).await?
            }
            _ => (uploaded, release.removed.iter().cloned().collect()),
        };
        self.switch(target, &dir).await?;
        let report = report(uploaded, removed);
        if target == HostTarget::Production {
            self.purge(&report).await;
        }
        tracing::info!(host = %self.name, site.release.seq = seq, uploaded = report.uploaded.len(), "release published");
        Ok(report)
    }

    async fn rollback_locked(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        let start = Instant::now();
        let stored = format!("releases/{}", to.seq);
        let not_found = || SiteError::ReleaseNotFound {
            seq: to.seq,
            message: format!("not under '{stored}/' in {}", self.store.name()),
        };
        self.read_index(&stored).await?.ok_or_else(not_found)?;
        let read_manifest = |seq: u64| {
            let stored = &stored;
            async move {
                let key = in_dir(stored, &manifest_path(seq));
                let bytes = self.store.get(&key).await?.ok_or_else(not_found)?;
                serde_json::from_slice::<ReleaseManifest>(&bytes)
                    .map_err(|e| SiteError::serde(key, e))
            }
        };
        let own = read_manifest(to.seq).await?;
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
            chain.push(read_manifest(base).await?);
        }
        let mut files: BTreeMap<String, FileEntry> = BTreeMap::new();
        for manifest in chain.iter().rev() {
            files = manifest.apply_to(&files);
        }
        let stored_ref = stored.as_str();
        self.each(
            files
                .iter()
                .map(|(path, entry)| {
                    async move {
                        let actual = self
                            .store
                            .get(&in_dir(stored_ref, release_key(path)))
                            .await?
                            .map_or_else(|| "missing".to_string(), |b| file_digest(&b).to_string());
                        if actual == entry.sha256.as_str() {
                            Ok(())
                        } else {
                            Err(SiteError::DigestMismatch {
                                path: path.clone(),
                                expected: entry.sha256.to_string(),
                                actual,
                            })
                        }
                    }
                    .boxed()
                })
                .collect(),
        )
        .await?;

        let current_dir = self.serving_dir(HostTarget::Production).await?.ok_or_else(not_found)?;
        let current_index = self.read_index(&current_dir).await?.unwrap_or_default();
        let dir = format!("releases/rollback-{}-{}", to.seq, nanos());
        let mut index = DirIndex::default();
        let dir_ref = dir.as_str();
        self.each(
            files
                .keys()
                .map(|path| {
                    async move {
                        let key = release_key(path);
                        self.store.copy(&in_dir(stored_ref, key), &in_dir(dir_ref, key)).await
                    }
                    .boxed()
                })
                .collect(),
        )
        .await?;
        for (path, entry) in &files {
            index.files.insert(path.clone(), entry.sha256.clone());
        }
        let log: Vec<(&String, &Digest)> =
            current_index.files.iter().filter(|(p, _)| is_log_path(p)).collect();
        let current_ref = current_dir.as_str();
        self.each(
            log.clone()
                .into_iter()
                .map(|(path, _)| {
                    async move {
                        let key = release_key(path);
                        self.store.copy(&in_dir(current_ref, key), &in_dir(dir_ref, key)).await
                    }
                    .boxed()
                })
                .collect(),
        )
        .await?;
        for (path, digest) in log {
            index.files.insert(path.clone(), digest.clone());
        }
        self.write_index(&dir, &index).await?;
        let removed: Vec<String> = current_index
            .site_files()
            .filter(|(p, _)| !files.contains_key(*p))
            .map(|(p, _)| p.clone())
            .collect();
        if let Layout::InPlace { live } = &self.layout {
            self.mirror_live(live, Some(&current_index), &index, &dir, None).await?;
        }
        self.switch(HostTarget::Production, &dir).await?;
        let report = PublishReport {
            host: self.name.clone(),
            target: HostTarget::Production,
            seq: to.seq,
            uploaded: files.keys().cloned().collect(),
            removed,
            elapsed: start.elapsed(),
        };
        self.purge(&report).await;
        tracing::warn!(host = %self.name, site.rollback.to = to.seq, "rolled back");
        Ok(report)
    }
}

#[async_trait]
impl SiteHost for ObjectStoreHost {
    fn name(&self) -> &str {
        &self.name
    }

    async fn publish(&self, release: &StagedRelease, target: HostTarget) -> Result<PublishReport> {
        let _guard = self.lock.lock().await;
        self.publish_locked(release, target).await
    }

    async fn rollback(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        let _guard = self.lock.lock().await;
        self.rollback_locked(to).await
    }

    async fn fetch(&self, path: &str) -> Result<Option<Bytes>> {
        let key = site_key(&self.name, path)?;
        match &self.layout {
            Layout::InPlace { live } => live.get(&key).await,
            Layout::PrefixPointer => {
                let Some(dir) = self.serving_dir(HostTarget::Production).await? else {
                    return Ok(None);
                };
                self.store.get(&in_dir(&dir, &key)).await
            }
        }
    }

    fn redirects_format(&self) -> RedirectsFormat {
        self.redirects
    }

    fn limits(&self) -> HostLimits {
        HostLimits {
            max_publishes_per_hour: self.max_publishes_per_hour,
            delta_upload: true,
            atomic: match self.layout {
                Layout::PrefixPointer => Atomicity::PrefixPointer,
                Layout::InPlace { .. } => Atomicity::None,
            },
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
