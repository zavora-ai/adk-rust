//! `Snapshots`: walking, hashing, storing and collecting a workspace's snapshots
//! (spec 3.1 §7.27).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use adk_core::{ErrorCategory, ReadonlyContext, Result};
use chrono::Utc;
use serde_json::{Value, json};

use super::objects::{SnapshotObjects, object_id, root_digest, snapshot_error};
use super::store::{
    FileEntry, PlanMark, SnapshotKind, SnapshotRecord, SnapshotStore, TranscriptTail,
};
use super::walk::{EntryKind, ExcludeFilter, NotCovered, WalkOptions, WalkedFile, walk};
use super::{NOT_COVERED_LISTED, SnapshotConfig};

/// A file whose modification time is this close to the walk is hashed again next time:
/// a write in the same clock tick would leave its size and time unchanged.
const RACY_WINDOW: Duration = Duration::from_secs(2);

/// Objects younger than this are never collected: another process may be writing the
/// snapshot that references them.
const GC_GRACE: Duration = Duration::from_secs(600);

/// Most (session, run) pairs remembered so a run is snapshotted as a turn once.
const TURNS_REMEMBERED: usize = 4096;

/// What to snapshot and what to record with it.
///
/// # Example
///
/// ```rust
/// use adk_harness::snapshot::{SnapshotKind, SnapshotRequest};
///
/// let request = SnapshotRequest::new("adk-rust", "alice", "s1", "inv-1", SnapshotKind::Destructive);
/// assert_eq!(request.kind, SnapshotKind::Destructive);
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SnapshotRequest {
    /// The application.
    pub app_name: String,
    /// The user.
    pub user_id: String,
    /// The session.
    pub session_id: String,
    /// The run it is taken in.
    pub invocation_id: String,
    /// Why it is taken.
    pub kind: SnapshotKind,
    /// The plan step it precedes.
    pub step: Option<String>,
    /// Where the plan stands.
    pub plan: Option<PlanMark>,
    /// For a rewind: the events it removes.
    pub transcript: Option<TranscriptTail>,
}

impl SnapshotRequest {
    /// A snapshot of `kind` for the session, in the run `invocation_id`.
    pub fn new(
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
        invocation_id: impl Into<String>,
        kind: SnapshotKind,
    ) -> Self {
        Self {
            app_name: app_name.into(),
            user_id: user_id.into(),
            session_id: session_id.into(),
            invocation_id: invocation_id.into(),
            kind,
            step: None,
            plan: None,
            transcript: None,
        }
    }

    /// A snapshot of `kind` for the run `ctx` describes.
    pub fn for_context(ctx: &dyn ReadonlyContext, kind: SnapshotKind) -> Self {
        Self::new(ctx.app_name(), ctx.user_id(), ctx.session_id(), ctx.invocation_id(), kind)
    }

    /// Records the plan step it precedes.
    #[must_use]
    pub fn step(mut self, step: impl Into<String>) -> Self {
        self.step = Some(step.into());
        self
    }

    /// Records where the plan stands.
    #[must_use]
    pub fn plan(mut self, plan: Option<PlanMark>) -> Self {
        self.plan = plan;
        self
    }

    /// Records the events a rewind removes.
    #[must_use]
    pub fn transcript(mut self, tail: TranscriptTail) -> Self {
        self.transcript = Some(tail);
        self
    }
}

/// What a snapshot holds, as `harness.snapshot.taken` records it (spec 3.1 §9.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotInfo {
    /// The snapshot's id.
    pub id: String,
    /// Why it was taken.
    pub kind: SnapshotKind,
    /// How many files it holds.
    pub files: usize,
    /// Bytes it added to the object store.
    pub bytes_new: u64,
    /// Whether the walk stopped at `max_files`.
    pub partial: bool,
    /// How many files it left out.
    pub not_covered: usize,
}

impl SnapshotInfo {
    /// The extension fields of `harness.snapshot.taken`.
    pub fn audit_fields(&self) -> Value {
        json!({
            "snapshotId": self.id,
            "kind": self.kind.as_str(),
            "files": self.files,
            "bytesNew": self.bytes_new,
            "partial": self.partial,
            "notCovered": self.not_covered,
        })
    }
}

/// The current tree, hashed but not stored.
#[derive(Clone, Debug, Default)]
pub struct TreeScan {
    /// The files, sorted by path.
    pub files: Vec<FileEntry>,
    /// Whether the walk stopped at `max_files`.
    pub partial: bool,
    /// Files the walk saw and left out.
    pub not_covered: Vec<NotCovered>,
}

/// One file's stat and hash, so an unchanged file is not read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stat {
    len: u64,
    mtime_ns: u64,
    inode: u64,
}

#[derive(Default)]
struct StatCache {
    seeded: bool,
    entries: HashMap<String, (Stat, String)>,
}

/// A workspace's snapshots (spec 3.1 §7.27): the walker over the root, the stat cache, the
/// object store and the record store, with retention and garbage collection.
///
/// Built by [`HarnessAgentBuilder::snapshots`](crate::HarnessAgentBuilder::snapshots) and
/// reached through [`HarnessRuntime::snapshots`](crate::HarnessRuntime::snapshots) or
/// [`Harness::snapshots`](crate::Harness::snapshots). [`take`](Self::take) is the "snapshot
/// now" a governed path calls before a `Destructive` call (§7.30).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_harness::snapshot::{
///     InMemorySnapshotObjects, InMemorySnapshotStore, SnapshotConfig, SnapshotKind,
///     SnapshotRequest, Snapshots,
/// };
///
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("a.txt"), "one").unwrap();
/// let snapshots = Snapshots::new(
///     SnapshotConfig::default(),
///     &adk_devtools::Workspace::new(dir.path()),
///     Arc::new(InMemorySnapshotStore::new()),
///     Arc::new(InMemorySnapshotObjects::new()),
/// )
/// .unwrap();
/// let info = snapshots
///     .take(SnapshotRequest::new("app", "alice", "s1", "inv-1", SnapshotKind::Turn))
///     .await
///     .unwrap();
/// assert_eq!(info.files, 1);
/// # });
/// ```
pub struct Snapshots {
    config: SnapshotConfig,
    root: PathBuf,
    root_digest: String,
    exclude: Option<ExcludeFilter>,
    store: Arc<dyn SnapshotStore>,
    objects: Arc<dyn SnapshotObjects>,
    cache: Arc<Mutex<StatCache>>,
    turns: Mutex<HashSet<String>>,
    gate: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Snapshots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshots")
            .field("root", &self.root)
            .field("config", &self.config)
            .field("objects", &self.objects)
            .finish_non_exhaustive()
    }
}

fn walk_error(error: impl std::fmt::Display) -> adk_core::AdkError {
    snapshot_error(
        "harness.snapshot.walk",
        ErrorCategory::Internal,
        format!("the snapshot walk stopped: {error}"),
    )
}

impl Snapshots {
    /// Snapshots of `workspace`'s root under `config`, recorded in `store` with contents in
    /// `objects`.
    ///
    /// # Errors
    ///
    /// Returns `harness.snapshot.inside_workspace` when the objects would be kept inside
    /// the workspace (I68), or `harness.snapshot.io` when the root cannot be resolved.
    pub fn new(
        config: SnapshotConfig,
        workspace: &adk_devtools::Workspace,
        store: Arc<dyn SnapshotStore>,
        objects: Arc<dyn SnapshotObjects>,
    ) -> Result<Self> {
        let root = workspace.root().canonicalize().map_err(|error| {
            snapshot_error(
                "harness.snapshot.io",
                ErrorCategory::InvalidInput,
                format!("the workspace {} cannot be snapshotted: {error}", workspace.root().display()),
            )
        })?;
        if let Some(location) = objects.location() {
            let location = location.canonicalize().unwrap_or_else(|_| location.to_path_buf());
            if location.starts_with(&root) {
                return Err(snapshot_error(
                    "harness.snapshot.inside_workspace",
                    ErrorCategory::InvalidInput,
                    format!(
                        "snapshot objects at {} would be inside the workspace {} they snapshot; \
                         keep them beside harness.db instead (spec 3.1 I68)",
                        location.display(),
                        root.display()
                    ),
                ));
            }
        }
        let excluder = workspace.clone();
        let exclude: ExcludeFilter = Arc::new(move |path: &Path| excluder.is_excluded(path));
        Ok(Self {
            root_digest: root_digest(&root),
            root,
            config,
            exclude: Some(exclude),
            store,
            objects,
            cache: Arc::default(),
            turns: Mutex::default(),
            gate: tokio::sync::Mutex::new(()),
        })
    }

    /// The configuration.
    pub fn config(&self) -> &SnapshotConfig {
        &self.config
    }

    /// The canonical workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The root's digest, which names its object directory and keys its records.
    pub fn root_digest(&self) -> &str {
        &self.root_digest
    }

    /// The record store.
    pub fn store(&self) -> &Arc<dyn SnapshotStore> {
        &self.store
    }

    /// The object store.
    pub fn objects(&self) -> &Arc<dyn SnapshotObjects> {
        &self.objects
    }

    pub(crate) fn walk_options(&self) -> WalkOptions {
        WalkOptions {
            max_files: self.config.max_files,
            max_file_bytes: Some(self.config.max_file_bytes),
            skip_dirs: self.config.skip_dirs.clone(),
            enter: Vec::new(),
            git_entries: None,
            gitignore: true,
            exclude: self.exclude.clone(),
        }
    }

    /// Whether the run `invocation_id` of the session already has its turn snapshot;
    /// marks it when not.
    pub(crate) fn first_turn_snapshot(&self, session: &str, invocation_id: &str) -> bool {
        let mut turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        if turns.len() >= TURNS_REMEMBERED {
            turns.clear();
        }
        turns.insert(format!("{session}\u{1f}{invocation_id}"))
    }

    /// Takes a snapshot now: walks the root, stores what changed, records it and applies
    /// retention (spec 3.1 §7.27). The "snapshot now" a `Destructive` call takes (§7.30).
    ///
    /// # Errors
    ///
    /// Returns the object or record store's error; nothing is recorded then.
    pub async fn take(&self, request: SnapshotRequest) -> Result<SnapshotInfo> {
        let _gate = self.gate.lock().await;
        self.seed_cache().await;
        let created_at = Utc::now();
        let (files, partial, not_covered, bytes_new) = self.hash_tree(true).await?;
        let manifest = serde_json::to_vec(&files)
            .map_err(|error| crate::fsutil::serialize_error("snapshot manifest", &error))?;
        let files_object = object_id(&manifest);
        let objects = self.objects.clone();
        let (id_for_put, manifest_bytes) = (files_object.clone(), manifest);
        let manifest_new = tokio::task::spawn_blocking(move || objects.put(&id_for_put, &manifest_bytes))
            .await
            .map_err(walk_error)??;
        let listed: Vec<NotCovered> = not_covered.iter().take(NOT_COVERED_LISTED).cloned().collect();
        let record = SnapshotRecord {
            id: format!("snap-{}", uuid::Uuid::new_v4().simple()),
            app_name: request.app_name,
            user_id: request.user_id,
            session_id: request.session_id,
            root: self.root_digest.clone(),
            created_at,
            kind: request.kind,
            invocation_id: request.invocation_id,
            step: request.step,
            files_object,
            file_count: files.len(),
            total_bytes: files.iter().map(|file| file.len).sum(),
            bytes_new: bytes_new + manifest_new,
            partial,
            not_covered_more: not_covered.len() - listed.len(),
            not_covered: listed,
            plan: request.plan,
            transcript: request.transcript,
        };
        self.store.put(&record).await?;
        let info = SnapshotInfo {
            id: record.id.clone(),
            kind: record.kind,
            files: record.file_count,
            bytes_new: record.bytes_new,
            partial,
            not_covered: not_covered.len(),
        };
        tracing::debug!(
            snapshot.id = %info.id,
            snapshot.kind = %info.kind,
            snapshot.files = info.files,
            snapshot.bytes_new = info.bytes_new,
            snapshot.partial = info.partial,
            "snapshot taken"
        );
        if let Err(error) = self.retain(&record).await {
            tracing::warn!(error = %error, "snapshot retention did not complete");
        }
        Ok(info)
    }

    /// The current tree, hashed through the stat cache, nothing stored.
    ///
    /// # Errors
    ///
    /// Returns `harness.snapshot.walk` when the walk cannot run.
    pub async fn scan(&self) -> Result<TreeScan> {
        self.seed_cache().await;
        let (files, partial, not_covered, _) = self.hash_tree(false).await?;
        Ok(TreeScan { files, partial, not_covered })
    }

    /// The file list of `record`.
    ///
    /// # Errors
    ///
    /// Returns `harness.snapshot.missing_object` when its list was collected.
    pub async fn files(&self, record: &SnapshotRecord) -> Result<Vec<FileEntry>> {
        let objects = self.objects.clone();
        let id = record.files_object.clone();
        let bytes = tokio::task::spawn_blocking(move || objects.get(&id)).await.map_err(walk_error)??;
        serde_json::from_slice(&bytes).map_err(|error| {
            snapshot_error(
                "harness.snapshot.corrupt_object",
                ErrorCategory::Internal,
                format!("snapshot {}'s file list does not parse: {error}", record.id),
            )
        })
    }

    /// The content of the object `id`.
    ///
    /// # Errors
    ///
    /// As [`SnapshotObjects::get`].
    pub async fn object(&self, id: &str) -> Result<Vec<u8>> {
        let objects = self.objects.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || objects.get(&id)).await.map_err(walk_error)?
    }

    /// The session's snapshots, oldest first.
    ///
    /// # Errors
    ///
    /// Returns the record store's error.
    pub async fn list(&self, app: &str, user: &str, session: &str) -> Result<Vec<SnapshotRecord>> {
        Ok(self
            .store
            .list_session(app, user, session)
            .await?
            .into_iter()
            .filter(|record| record.root == self.root_digest)
            .collect())
    }

    /// Removes the session's snapshots and the objects only they referenced: a session's
    /// snapshots go with the session (spec 3.1 §7.27). Returns how many were removed.
    ///
    /// # Errors
    ///
    /// Returns the record or object store's error.
    pub async fn forget_session(&self, app: &str, user: &str, session: &str) -> Result<usize> {
        let _gate = self.gate.lock().await;
        let ids: Vec<String> =
            self.list(app, user, session).await?.into_iter().map(|record| record.id).collect();
        if ids.is_empty() {
            return Ok(0);
        }
        self.store.delete(&ids).await?;
        self.collect_garbage().await?;
        Ok(ids.len())
    }

    /// Seeds the stat cache from the workspace's latest snapshot, once per process.
    async fn seed_cache(&self) {
        if self.cache.lock().unwrap_or_else(|e| e.into_inner()).seeded {
            return;
        }
        let latest = match self.store.list_root(&self.root_digest).await {
            Ok(records) => records.into_iter().last(),
            Err(_) => None,
        };
        let mut seeded = HashMap::new();
        if let Some(latest) = latest
            && let Ok(files) = self.files(&latest).await
        {
            let walked_at = u64::try_from(latest.created_at.timestamp_nanos_opt().unwrap_or(0))
                .unwrap_or(0)
                .saturating_sub(RACY_WINDOW.as_nanos() as u64);
            for file in files {
                if file.link.is_none() && file.mtime_ns < walked_at {
                    seeded.insert(
                        file.path.clone(),
                        (Stat { len: file.len, mtime_ns: file.mtime_ns, inode: file.inode }, file.sha256),
                    );
                }
            }
        }
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if !cache.seeded {
            cache.entries.extend(seeded);
            cache.seeded = true;
        }
    }

    /// Walks and hashes the tree off the async runtime; with `store`, puts every object
    /// that is not stored yet. Returns the sorted entries, whether the walk was partial,
    /// what it left out and the bytes it wrote.
    async fn hash_tree(&self, store: bool) -> Result<(Vec<FileEntry>, bool, Vec<NotCovered>, u64)> {
        let root = self.root.clone();
        let options = self.walk_options();
        let cache = self.cache.clone();
        let objects = self.objects.clone();
        tokio::task::spawn_blocking(move || {
            let started = SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| u64::try_from(since.as_nanos()).unwrap_or(u64::MAX));
            let racy_after = started.saturating_sub(RACY_WINDOW.as_nanos() as u64);
            let tree = walk(&root, &options);
            let mut entries = Vec::with_capacity(tree.files.len());
            let mut not_covered = tree.not_covered;
            let mut bytes_new = 0;
            for file in tree.files {
                match hash_file(&root, &file, &cache, objects.as_ref(), store, racy_after) {
                    Ok((entry, written)) => {
                        bytes_new += written;
                        entries.push(entry);
                    }
                    // A file that vanished mid-walk is simply not in the snapshot.
                    Err(HashFailure::Gone) => {}
                    Err(HashFailure::Store(error)) => return Err(error),
                }
            }
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            not_covered.sort_by(|a, b| a.path.cmp(&b.path));
            Ok((entries, tree.partial, not_covered, bytes_new))
        })
        .await
        .map_err(walk_error)?
    }

    /// Retention (spec 3.1 §7.27): the session keeps its last `keep_turns` turn snapshots
    /// and what came after the oldest of them; the workspace keeps at most `max_bytes` of
    /// objects, oldest snapshots dropped first; then unreferenced objects are collected.
    async fn retain(&self, latest: &SnapshotRecord) -> Result<()> {
        let session = self
            .list(&latest.app_name, &latest.user_id, &latest.session_id)
            .await?;
        let turns: Vec<&SnapshotRecord> =
            session.iter().filter(|record| record.kind == SnapshotKind::Turn).collect();
        let keep = self.config.keep_turns.max(1) as usize;
        let mut drop: Vec<String> = Vec::new();
        if turns.len() > keep {
            let cutoff = turns[turns.len() - keep].created_at;
            drop.extend(
                session
                    .iter()
                    .filter(|record| record.created_at < cutoff && record.id != latest.id)
                    .map(|record| record.id.clone()),
            );
        }
        let objects = self.objects.clone();
        let listed = tokio::task::spawn_blocking(move || objects.list()).await.map_err(walk_error)??;
        let total: u64 = listed.iter().map(|info| info.bytes).sum();
        if total > self.config.max_bytes {
            let sizes: HashMap<&str, u64> =
                listed.iter().map(|info| (info.id.as_str(), info.bytes)).collect();
            let mut records = self.store.list_root(&self.root_digest).await?;
            records.retain(|record| !drop.contains(&record.id));
            let mut counted: HashSet<String> = HashSet::new();
            let mut used = 0_u64;
            let mut over = false;
            let mut lists: BTreeMap<String, Vec<FileEntry>> = BTreeMap::new();
            for record in records.iter().rev() {
                if over && record.id != latest.id {
                    drop.push(record.id.clone());
                    continue;
                }
                if !lists.contains_key(&record.files_object) {
                    let files = self.files(record).await.unwrap_or_default();
                    lists.insert(record.files_object.clone(), files);
                }
                let ids = std::iter::once(record.files_object.as_str())
                    .chain(lists[&record.files_object].iter().filter(|f| f.link.is_none()).map(|f| f.sha256.as_str()));
                for id in ids {
                    if counted.insert(id.to_string()) {
                        used += sizes.get(id).copied().unwrap_or(0);
                    }
                }
                if used > self.config.max_bytes && record.id != latest.id {
                    over = true;
                    drop.push(record.id.clone());
                }
            }
        }
        if drop.is_empty() {
            return Ok(());
        }
        tracing::info!(snapshots.dropped = drop.len(), "snapshot retention dropped old snapshots");
        self.store.delete(&drop).await?;
        self.collect_garbage().await
    }

    /// Removes every object no record of this workspace references, sparing recent ones.
    async fn collect_garbage(&self) -> Result<()> {
        let records = self.store.list_root(&self.root_digest).await?;
        let mut referenced: HashSet<String> = HashSet::new();
        for record in &records {
            if !referenced.insert(record.files_object.clone()) {
                continue;
            }
            // A list that cannot be read keeps everything: never collect blind.
            let files = self.files(record).await?;
            referenced.extend(files.into_iter().filter(|f| f.link.is_none()).map(|f| f.sha256));
        }
        let objects = self.objects.clone();
        let removed = tokio::task::spawn_blocking(move || -> Result<usize> {
            let now = SystemTime::now();
            let mut removed = 0;
            for info in objects.list()? {
                let recent = info
                    .modified
                    .and_then(|modified| now.duration_since(modified).ok())
                    .is_some_and(|age| age < GC_GRACE);
                if !referenced.contains(&info.id) && !recent {
                    objects.remove(&info.id)?;
                    removed += 1;
                }
            }
            Ok(removed)
        })
        .await
        .map_err(walk_error)??;
        if removed > 0 {
            tracing::debug!(objects.removed = removed, "snapshot objects collected");
        }
        Ok(())
    }
}

enum HashFailure {
    Gone,
    Store(adk_core::AdkError),
}

/// Hashes one walked file through the cache; with `store`, puts its object when missing.
fn hash_file(
    root: &Path,
    file: &WalkedFile,
    cache: &Mutex<StatCache>,
    objects: &dyn SnapshotObjects,
    store: bool,
    racy_after: u64,
) -> std::result::Result<(FileEntry, u64), HashFailure> {
    let path = root.join(&file.path);
    if file.kind == EntryKind::Symlink {
        let target = std::fs::read_link(&path).map_err(|_| HashFailure::Gone)?;
        let target = target.to_string_lossy().into_owned();
        return Ok((
            FileEntry {
                path: file.path.clone(),
                sha256: object_id(target.as_bytes()),
                len: target.len() as u64,
                mode: file.mode,
                mtime_ns: file.mtime_ns,
                inode: file.inode,
                link: Some(target),
            },
            0,
        ));
    }
    let stat = Stat { len: file.len, mtime_ns: file.mtime_ns, inode: file.inode };
    let cached = {
        let cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.entries.get(&file.path).filter(|(known, _)| *known == stat).map(|(_, sha)| sha.clone())
    };
    let mut written = 0;
    let sha256 = match cached {
        Some(sha) if !store || objects.contains(&sha).map_err(HashFailure::Store)? => sha,
        _ => {
            let bytes = std::fs::read(&path).map_err(|_| HashFailure::Gone)?;
            let sha = object_id(&bytes);
            if store {
                written = objects.put(&sha, &bytes).map_err(HashFailure::Store)?;
            }
            if file.mtime_ns < racy_after && bytes.len() as u64 == file.len {
                cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .entries
                    .insert(file.path.clone(), (stat, sha.clone()));
            }
            sha
        }
    };
    Ok((
        FileEntry {
            path: file.path.clone(),
            sha256,
            len: file.len,
            mode: file.mode,
            mtime_ns: file.mtime_ns,
            inode: file.inode,
            link: None,
        },
        written,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{InMemorySnapshotObjects, InMemorySnapshotStore};

    fn snapshots(dir: &Path, config: SnapshotConfig) -> (Snapshots, Arc<InMemorySnapshotObjects>) {
        let objects = Arc::new(InMemorySnapshotObjects::new());
        let snapshots = Snapshots::new(
            config,
            &adk_devtools::Workspace::new(dir),
            Arc::new(InMemorySnapshotStore::new()),
            objects.clone(),
        )
        .unwrap();
        (snapshots, objects)
    }

    fn request(session: &str, invocation: &str) -> SnapshotRequest {
        SnapshotRequest::new("app", "alice", session, invocation, SnapshotKind::Turn)
    }

    #[tokio::test]
    async fn an_unchanged_tree_adds_nothing_and_a_change_adds_only_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one").unwrap();
        std::fs::write(dir.path().join("b.txt"), "two").unwrap();
        let (snapshots, _) = snapshots(dir.path(), SnapshotConfig::default());
        let first = snapshots.take(request("s1", "i1")).await.unwrap();
        assert_eq!(first.files, 2);
        assert!(first.bytes_new > 0);
        let second = snapshots.take(request("s1", "i2")).await.unwrap();
        assert_eq!(second.bytes_new, 0, "identical tree, identical manifest: nothing new");
        std::fs::write(dir.path().join("b.txt"), "two, changed").unwrap();
        let third = snapshots.take(request("s1", "i3")).await.unwrap();
        assert!(third.bytes_new >= "two, changed".len() as u64);
    }

    #[tokio::test]
    async fn the_stat_cache_reads_an_unchanged_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one").unwrap();
        // Old enough to be trusted by the cache.
        let old = std::fs::File::options().write(true).open(&path).unwrap();
        old.set_modified(SystemTime::now() - Duration::from_secs(60)).unwrap();
        drop(old);
        let (snapshots, _) = snapshots(dir.path(), SnapshotConfig::default());
        let first = snapshots.scan().await.unwrap();
        // Unreadable now, yet the cached hash answers because size, time and inode match.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            let second = snapshots.scan().await.unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(second.files[0].sha256, first.files[0].sha256);
            assert_eq!(second.files[0].mode & 0o777, 0o000, "the mode is read, the content is not");
        }
        // A change of size is seen.
        std::fs::write(&path, "one more").unwrap();
        let third = snapshots.scan().await.unwrap();
        assert_ne!(third.files[0].sha256, first.files[0].sha256);
    }

    #[tokio::test]
    async fn retention_keeps_the_last_turns_and_collects_their_objects() {
        let dir = tempfile::tempdir().unwrap();
        let config = SnapshotConfig { keep_turns: 2, ..SnapshotConfig::default() };
        let (snapshots, objects) = snapshots(dir.path(), config);
        for turn in 0..4 {
            std::fs::write(dir.path().join("a.txt"), format!("version {turn}")).unwrap();
            snapshots.take(request("s1", &format!("i{turn}"))).await.unwrap();
        }
        let kept = snapshots.list("app", "alice", "s1").await.unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].invocation_id, "i2");
        // In memory, objects have no age, so collection is immediate: two manifests and two
        // versions of a.txt remain.
        assert_eq!(objects.list().unwrap().len(), 4);
        assert_eq!(snapshots.forget_session("app", "alice", "s1").await.unwrap(), 2);
        assert!(objects.list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_byte_budget_drops_the_oldest_snapshots_first() {
        let dir = tempfile::tempdir().unwrap();
        let config = SnapshotConfig { max_bytes: 3_000, ..SnapshotConfig::default() };
        let (snapshots, objects) = snapshots(dir.path(), config);
        for turn in 0..4 {
            std::fs::write(dir.path().join("a.bin"), vec![turn as u8; 1_000]).unwrap();
            snapshots.take(request("s1", &format!("i{turn}"))).await.unwrap();
        }
        let kept = snapshots.list("app", "alice", "s1").await.unwrap();
        assert!(kept.len() < 4 && !kept.is_empty(), "{}", kept.len());
        assert_eq!(kept.last().unwrap().invocation_id, "i3", "the newest is never dropped");
        let total: u64 = objects.list().unwrap().iter().map(|info| info.bytes).sum();
        assert!(total <= 3_000, "{total}");
    }

    #[tokio::test]
    async fn objects_inside_the_workspace_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let result = Snapshots::new(
            SnapshotConfig::default(),
            &adk_devtools::Workspace::new(dir.path()),
            Arc::new(InMemorySnapshotStore::new()),
            Arc::new(crate::snapshot::FileSnapshotObjects::new(dir.path().join(".snapshots"))),
        );
        assert_eq!(result.unwrap_err().code, "harness.snapshot.inside_workspace");
    }
}
