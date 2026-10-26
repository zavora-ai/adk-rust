//! The site log: every release's manifest digest in an append-only Merkle log with signed
//! checkpoints (spec 3.5 §7.7, D-11, A-I4, A-I5; AWP 2.0 §14.5, §14.6).
//!
//! | Element | Definition |
//! |---------|------------|
//! | Entry | The JCS form of an awp-types [`LogEntry`] `{"seq", "manifest"}` |
//! | Order | Release `n` is leaf `n − 1`; one entry per release, no gaps |
//! | Hashes | RFC 9162: leaves `SHA-256(0x00 ‖ entry)`, nodes `SHA-256(0x01 ‖ l ‖ r)` (`adk-auth` merkle) |
//! | Tiles | C2SP tlog-tiles of height 8 under [`LOG_PREFIX`] |
//! | Checkpoint | C2SP checkpoint with origin `{domain}/.well-known/awp/log`, a signed note by the [`ReleaseSigner`] (`adk-auth` notes) |
//!
//! [`SiteLog`] keeps the tree in an [`AuditLogStore`] (in memory, SQLite or PostgreSQL), so it
//! survives restarts. [`SiteLog::publication`] returns the tiles and the checkpoint a host
//! publishes after the release's files, checkpoint last. A verifier holding only published
//! files uses [`TileProofs`], [`verify_checkpoint`], [`verify_release_inclusion`] and
//! [`verify_checkpoint_extends`].

mod tiles;

use std::sync::Arc;

use adk_auth::signed_audit::merkle::{leaf_hash, node_hash, verify_consistency, verify_inclusion};
use adk_auth::signed_audit::{AuditSigner, CheckpointBody, Note, NoteVerifier, sign_note};
use adk_core::audit_log::{
    AuditAppend, AuditCheckpoint, AuditHash, AuditLogEntry, AuditLogStore, AuditTreeNode,
};
use adk_core::{Clock, SystemClock};
use awp_types::{Digest, LogEntry};
use bytes::Bytes;
use tokio::sync::{Mutex, RwLock};

use crate::error::{Result, SiteError};
use crate::signer::{NoteSigner, ReleaseSigner, log_origin};

pub use tiles::{
    TILE_HEIGHT, TILE_WIDTH, TileFetch, TileProofs, decode_bundle, entry_bundle_path,
    hash_tile_path, tile_index_path,
};

/// Where the log is published, relative to the site root.
pub const LOG_PREFIX: &str = ".well-known/awp/log";
/// The published checkpoint, relative to the site root.
pub const CHECKPOINT_PATH: &str = ".well-known/awp/log/checkpoint";

/// The leaf bytes of a site-log entry: the UTF-8 JCS form of `{"seq", "manifest"}`
/// (AWP 2.0 §14.5).
///
/// # Example
///
/// ```rust
/// use adk_site::log::{entry_jcs, parse_entry};
/// use awp_types::{Digest, LogEntry};
///
/// let entry = LogEntry { seq: 1, manifest: Digest::from_sha256_bytes([0; 32]) };
/// let bytes = entry_jcs(&entry).unwrap();
/// assert!(bytes.starts_with(b"{\"manifest\":\"sha256:"));
/// assert_eq!(parse_entry(&bytes).unwrap(), entry);
/// ```
///
/// # Errors
///
/// [`SiteError::Log`] when `seq` exceeds 2^53.
pub fn entry_jcs(entry: &LogEntry) -> Result<Vec<u8>> {
    adk_awp::signing::log::entry_bytes(entry).map_err(|e| SiteError::Log(e.to_string()))
}

/// Parses leaf bytes as a site-log entry and checks they are in JCS form.
///
/// # Errors
///
/// [`SiteError::Log`] when the bytes are not an entry or not canonical.
pub fn parse_entry(bytes: &[u8]) -> Result<LogEntry> {
    let entry: LogEntry = serde_json::from_slice(bytes)
        .map_err(|e| SiteError::Log(format!("log entry is malformed: {e}")))?;
    if entry_jcs(&entry)? != bytes {
        return Err(SiteError::Log("log entry is not in JCS form".into()));
    }
    Ok(entry)
}

/// The files a host publishes for the log after a release's files and manifest: tiles,
/// then the checkpoint (AWP 2.0 §14.6).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogPublication {
    /// Changed hash tiles and entry bundles, paths relative to the site root.
    pub tiles: Vec<(String, Bytes)>,
    /// The checkpoint path and its signed note, published last.
    pub checkpoint: Option<(String, Bytes)>,
    /// The tree size the checkpoint covers: the latest release number.
    pub size: u64,
}

impl LogPublication {
    /// Every file in publication order: tiles, then the checkpoint.
    pub fn files(&self) -> impl Iterator<Item = &(String, Bytes)> {
        self.tiles.iter().chain(self.checkpoint.iter())
    }
}

/// The complete subtrees of the tree so far, left to right, as `(height, hash)`.
#[derive(Debug, Clone, Default)]
struct Frontier {
    size: u64,
    stack: Vec<(u32, AuditHash)>,
}

impl Frontier {
    /// Adds a leaf and returns the interior nodes it completes.
    fn push(&mut self, leaf: AuditHash) -> Vec<AuditTreeNode> {
        let index = self.size;
        self.size += 1;
        self.stack.push((0, leaf));
        let mut nodes = Vec::new();
        while self.stack.len() >= 2
            && self.stack[self.stack.len() - 1].0 == self.stack[self.stack.len() - 2].0
        {
            let (height, right) = self.stack.pop().expect("two entries");
            let (_, left) = self.stack.pop().expect("two entries");
            let hash = node_hash(&left, &right);
            let level = height + 1;
            nodes.push(AuditTreeNode { level: level as u8, index: index >> level, hash });
            self.stack.push((level, hash));
        }
        nodes
    }

    fn root(&self) -> AuditHash {
        let mut iter = self.stack.iter().rev();
        match iter.next() {
            None => adk_auth::signed_audit::merkle::empty_root(),
            Some((_, last)) => iter.fold(*last, |acc, (_, left)| node_hash(left, &acc)),
        }
    }
}

/// The append-only, signed log of a site's releases (spec 3.5 §7.7; AWP 2.0 §14.5).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::audit_log::InMemoryAuditLogStore;
/// use adk_site::log::SiteLog;
/// use adk_site::signer::Ed25519ReleaseSigner;
/// use awp_types::{Digest, LogEntry};
///
/// # async fn demo() -> adk_site::Result<()> {
/// let signer = Arc::new(Ed25519ReleaseSigner::generate("example.com")?);
/// let log = SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), "example.com", signer).await?;
/// log.append(&LogEntry { seq: 1, manifest: Digest::from_sha256_bytes([1; 32]) }).await?;
/// let publication = log.publication(0).await?;
/// assert_eq!(publication.size, 1);
/// # Ok(())
/// # }
/// ```
pub struct SiteLog {
    store: Arc<dyn AuditLogStore>,
    log_id: String,
    origin: String,
    signer: RwLock<Arc<dyn ReleaseSigner>>,
    clock: Arc<dyn Clock>,
    frontier: Mutex<Frontier>,
}

impl std::fmt::Debug for SiteLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteLog")
            .field("log_id", &self.log_id)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

async fn stored_hash(
    store: &dyn AuditLogStore,
    log_id: &str,
    height: u32,
    index: u64,
) -> Result<AuditHash> {
    let level =
        u8::try_from(height).map_err(|_| SiteError::Log("tree height out of range".into()))?;
    store.hash_at(log_id, level, index).await?.ok_or_else(|| {
        SiteError::Log(format!(
            "site log '{log_id}' is missing the hash at height {height}, index {index}"
        ))
    })
}

async fn stored_range(
    store: &dyn AuditLogStore,
    log_id: &str,
    range: std::ops::Range<u64>,
) -> Result<AuditHash> {
    let mut hashes = Vec::new();
    for (height, index) in tiles::range_nodes(range) {
        hashes.push(stored_hash(store, log_id, height, index).await?);
    }
    let mut iter = hashes.iter().rev();
    Ok(match iter.next() {
        None => adk_auth::signed_audit::merkle::empty_root(),
        Some(last) => iter.fold(*last, |acc, left| node_hash(left, &acc)),
    })
}

impl SiteLog {
    /// Opens the log of `domain` in `store`, signing checkpoints with `signer`, whose key
    /// name must be the checkpoint origin `{domain}/.well-known/awp/log`.
    ///
    /// # Errors
    ///
    /// [`SiteError::Log`] when the signer's key name is not the origin, or the store holds a
    /// tree that no longer matches its latest checkpoint.
    pub async fn open(
        store: Arc<dyn AuditLogStore>,
        domain: &str,
        signer: Arc<dyn ReleaseSigner>,
    ) -> Result<Self> {
        Self::open_with_clock(store, domain, signer, Arc::new(SystemClock)).await
    }

    /// As [`open`](Self::open), stamping stored checkpoints with `clock`.
    ///
    /// # Errors
    ///
    /// As [`open`](Self::open).
    pub async fn open_with_clock(
        store: Arc<dyn AuditLogStore>,
        domain: &str,
        signer: Arc<dyn ReleaseSigner>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        let origin = log_origin(domain);
        if signer.key_name() != origin {
            return Err(SiteError::Log(format!(
                "the release key is named '{}'; a site-log key must be named '{origin}'",
                signer.key_name()
            )));
        }
        let log_id = format!("awp-site-log:{domain}");
        let size = store.size(&log_id).await?;
        let mut stack = Vec::new();
        for (height, index) in tiles::range_nodes(0..size) {
            stack.push((height, stored_hash(store.as_ref(), &log_id, height, index).await?));
        }
        let frontier = Frontier { size, stack };
        if let Some(checkpoint) = store.latest_checkpoint(&log_id).await? {
            let root = stored_range(store.as_ref(), &log_id, 0..checkpoint.size).await?;
            if root != checkpoint.root_hash {
                return Err(SiteError::Log(format!(
                    "site log '{log_id}' no longer matches its checkpoint at size {}; stop publishing and verify the log",
                    checkpoint.size
                )));
            }
        }
        Ok(Self {
            store,
            log_id,
            origin,
            signer: RwLock::new(signer),
            clock,
            frontier: Mutex::new(frontier),
        })
    }

    /// The checkpoint origin.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The store's id of this log.
    pub fn log_id(&self) -> &str {
        &self.log_id
    }

    /// The verifier key of the current release key.
    pub async fn verifier_key(&self) -> String {
        self.signer.read().await.verifier_key()
    }

    /// Replaces the release key, as a new delegation in `keys.json` takes over; later
    /// checkpoints are signed with it.
    ///
    /// # Errors
    ///
    /// [`SiteError::Log`] when its key name is not the origin.
    pub async fn rotate_signer(&self, signer: Arc<dyn ReleaseSigner>) -> Result<()> {
        if signer.key_name() != self.origin {
            return Err(SiteError::Log(format!("a site-log key must be named '{}'", self.origin)));
        }
        *self.signer.write().await = signer;
        Ok(())
    }

    /// Number of entries: the latest release number.
    pub async fn size(&self) -> u64 {
        self.frontier.lock().await.size
    }

    /// Appends the entry of release `entry.seq`, which must be the next release, and signs a
    /// checkpoint covering it.
    ///
    /// # Errors
    ///
    /// [`SiteError::Sequence`] when `entry.seq` is not the log's size plus one;
    /// [`SiteError::Log`] or [`SiteError::Signing`] when the store or the key fails.
    pub async fn append(&self, entry: &LogEntry) -> Result<AuditCheckpoint> {
        let mut frontier = self.frontier.lock().await;
        if entry.seq != frontier.size + 1 {
            return Err(SiteError::Sequence(format!(
                "the site log holds {} releases; the next entry must be release {}, not {}",
                frontier.size,
                frontier.size + 1,
                entry.seq
            )));
        }
        let bytes = entry_jcs(entry)?;
        let leaf = leaf_hash(&bytes);
        let mut next = frontier.clone();
        let nodes = next.push(leaf);
        let record = String::from_utf8(bytes).map_err(|e| SiteError::Log(e.to_string()))?;
        self.store
            .append(AuditAppend {
                log_id: self.log_id.clone(),
                entries: vec![AuditLogEntry {
                    seq: frontier.size,
                    leaf_hash: leaf,
                    record: Some(record),
                }],
                nodes,
            })
            .await?;
        *frontier = next;
        self.sign_checkpoint(&frontier).await
    }

    async fn sign_checkpoint(&self, frontier: &Frontier) -> Result<AuditCheckpoint> {
        let body = CheckpointBody {
            origin: self.origin.clone(),
            size: frontier.size,
            root_hash: frontier.root(),
        };
        let signer: Arc<dyn AuditSigner> = Arc::new(NoteSigner(self.signer.read().await.clone()));
        let note = sign_note(&body.to_text(), &[signer])
            .await
            .map_err(|e| SiteError::Signing(e.to_string()))?;
        let checkpoint = AuditCheckpoint {
            log_id: self.log_id.clone(),
            size: body.size,
            root_hash: body.root_hash,
            note,
            created_at: self.clock.now(),
        };
        self.store.put_checkpoint(checkpoint.clone()).await?;
        Ok(checkpoint)
    }

    /// The latest signed checkpoint.
    ///
    /// # Errors
    ///
    /// [`SiteError::Log`] when the store fails or the log has no checkpoint yet.
    pub async fn latest_checkpoint(&self) -> Result<AuditCheckpoint> {
        self.store
            .latest_checkpoint(&self.log_id)
            .await?
            .ok_or_else(|| SiteError::Log("the site log has no checkpoint yet".into()))
    }

    /// The entry of release `seq`.
    ///
    /// # Errors
    ///
    /// [`SiteError::Log`] when the store fails.
    pub async fn entry(&self, seq: u64) -> Result<Option<LogEntry>> {
        if seq == 0 || seq > self.size().await {
            return Ok(None);
        }
        let entries = self.store.read(&self.log_id, seq - 1..seq).await?;
        match entries.into_iter().next().and_then(|e| e.record) {
            Some(record) => parse_entry(record.as_bytes()).map(Some),
            None => Err(SiteError::Log(format!("the entry of release {seq} was pruned"))),
        }
    }

    /// The tiles that changed since the log held `since` entries, and the latest
    /// checkpoint, ready for a host.
    ///
    /// # Errors
    ///
    /// [`SiteError::Log`] when a stored hash or entry is missing.
    pub async fn publication(&self, since: u64) -> Result<LogPublication> {
        let checkpoint = self.latest_checkpoint().await?;
        let size = checkpoint.size;
        let store = self.store.as_ref();
        let mut files = Vec::new();
        for tile in tiles::changed_tiles(since.min(size), size) {
            let (index, width, start) = (tile.index, tile.width, tile.positions().start);
            match tile.level {
                None => {
                    let entries = store.read(&self.log_id, start..start + width).await?;
                    let bodies = entries
                        .into_iter()
                        .map(|entry| {
                            entry.record.map(String::into_bytes).ok_or_else(|| {
                                SiteError::Log(format!("entry {} was pruned", entry.seq))
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    files.push((
                        format!("{LOG_PREFIX}/{}", entry_bundle_path(index, width)),
                        Bytes::from(tiles::encode_bundle(&bodies)?),
                    ));
                }
                Some(level) => {
                    let mut hashes = Vec::with_capacity(width as usize);
                    for position in start..start + width {
                        hashes.push(
                            stored_hash(store, &self.log_id, level * TILE_HEIGHT, position).await?,
                        );
                    }
                    files.push((
                        format!("{LOG_PREFIX}/{}", hash_tile_path(level, index, width)),
                        Bytes::from(tiles::hash_tile(&hashes)),
                    ));
                }
            }
        }
        Ok(LogPublication {
            tiles: files,
            checkpoint: Some((CHECKPOINT_PATH.to_string(), Bytes::from(checkpoint.note))),
            size,
        })
    }

    /// The RFC 9162 inclusion proof for release `seq` in the tree of `size` entries.
    ///
    /// # Errors
    ///
    /// [`SiteError::Log`] when `seq` or `size` is out of range.
    pub async fn inclusion_proof(&self, seq: u64, size: u64) -> Result<Vec<AuditHash>> {
        if seq == 0 || seq > size || size > self.size().await {
            return Err(SiteError::Log(format!("release {seq} is not in a tree of {size}")));
        }
        let mut proof = Vec::new();
        for range in tiles::inclusion_ranges(seq - 1, size) {
            proof.push(stored_range(self.store.as_ref(), &self.log_id, range).await?);
        }
        Ok(proof)
    }

    /// The RFC 9162 consistency proof from the tree of `old` entries to that of `new`.
    ///
    /// # Errors
    ///
    /// [`SiteError::Log`] when a size is out of range.
    pub async fn consistency_proof(&self, old: u64, new: u64) -> Result<Vec<AuditHash>> {
        if old > new || new > self.size().await {
            return Err(SiteError::Log(format!("sizes {old} and {new} are out of range")));
        }
        let mut proof = Vec::new();
        for range in tiles::consistency_ranges(old, new) {
            proof.push(stored_range(self.store.as_ref(), &self.log_id, range).await?);
        }
        Ok(proof)
    }
}

/// A parsed, signature-checked checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCheckpoint {
    /// The origin line.
    pub origin: String,
    /// The tree size: the latest release number.
    pub size: u64,
    /// The root hash.
    pub root_hash: AuditHash,
}

/// Parses a checkpoint note, checks its origin is `{domain}/.well-known/awp/log` and that
/// one of `trusted` (valid `release` keys) signed it; other signatures, such as witness
/// cosignatures, are ignored.
///
/// # Errors
///
/// [`SiteError::Log`] when the note is malformed, names another origin, or no trusted key
/// signed it.
pub fn verify_checkpoint(
    note: &str,
    domain: &str,
    trusted: &[NoteVerifier],
) -> Result<VerifiedCheckpoint> {
    let parsed = Note::parse(note).map_err(|e| SiteError::Log(format!("checkpoint: {e}")))?;
    let body = CheckpointBody::parse(&parsed.text)
        .map_err(|e| SiteError::Log(format!("checkpoint: {e}")))?;
    let origin = log_origin(domain);
    if body.origin != origin {
        return Err(SiteError::Log(format!(
            "checkpoint origin is '{}', not '{origin}'",
            body.origin
        )));
    }
    let valid: Vec<&NoteVerifier> = trusted.iter().filter(|v| v.name() == origin).collect();
    let valid: Vec<NoteVerifier> = valid.into_iter().cloned().collect();
    if parsed.verified_by(&valid).is_empty() {
        return Err(SiteError::Log("checkpoint is not signed by a valid release key".into()));
    }
    Ok(VerifiedCheckpoint { origin: body.origin, size: body.size, root_hash: body.root_hash })
}

/// Checks that `entry` (raw bytes from the entry bundle) is release `seq` with
/// `manifest_digest` at leaf `seq − 1` of the checkpoint's tree.
///
/// # Errors
///
/// [`SiteError::Log`] naming the broken link: the entry's form or fields, or the proof.
pub fn verify_release_inclusion(
    checkpoint: &VerifiedCheckpoint,
    entry: &[u8],
    proof: &[AuditHash],
    seq: u64,
    manifest_digest: &Digest,
) -> Result<()> {
    let parsed = parse_entry(entry)?;
    if parsed.seq != seq {
        return Err(SiteError::Log(format!(
            "the log entry records release {}, not {seq}",
            parsed.seq
        )));
    }
    if &parsed.manifest != manifest_digest {
        return Err(SiteError::Log(format!(
            "the log records manifest {} for release {seq}; the served manifest is {manifest_digest}",
            parsed.manifest
        )));
    }
    if seq == 0 {
        return Err(SiteError::Log("release numbers start at 1".into()));
    }
    verify_inclusion(seq - 1, checkpoint.size, &leaf_hash(entry), proof, &checkpoint.root_hash)
        .map_err(|e| SiteError::Log(format!("release {seq} is not in the log: {e}")))
}

/// Checks that the `new` checkpoint extends the `old` one.
///
/// # Errors
///
/// [`SiteError::Log`] when the origins differ, the log shrank, or the proof fails.
pub fn verify_checkpoint_extends(
    old: &VerifiedCheckpoint,
    new: &VerifiedCheckpoint,
    proof: &[AuditHash],
) -> Result<()> {
    if old.origin != new.origin {
        return Err(SiteError::Log(format!(
            "checkpoint origin changed from '{}' to '{}'",
            old.origin, new.origin
        )));
    }
    verify_consistency(old.size, new.size, &old.root_hash, &new.root_hash, proof).map_err(|e| {
        SiteError::Log(format!(
            "checkpoint at size {} does not extend the one at size {}: {e}",
            new.size, old.size
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frontier_matches_the_reference_root() {
        let leaves: Vec<AuditHash> = (0u8..37).map(|i| leaf_hash(&[i])).collect();
        let mut frontier = Frontier::default();
        for (n, leaf) in leaves.iter().enumerate() {
            frontier.push(*leaf);
            assert_eq!(
                frontier.root(),
                adk_auth::signed_audit::merkle::root(&leaves[..=n]),
                "size {}",
                n + 1
            );
        }
    }

    #[test]
    fn entries_are_canonical() {
        let entry = LogEntry { seq: 101, manifest: Digest::from_sha256_bytes([0xab; 32]) };
        assert_eq!(
            String::from_utf8(entry_jcs(&entry).unwrap()).unwrap(),
            format!("{{\"manifest\":\"sha256:{}\",\"seq\":101}}", "ab".repeat(32))
        );
        assert!(parse_entry(b"{\"seq\":101,\"manifest\":\"sha256:00\"}").is_err());
        let spaced = format!("{{ \"manifest\":\"sha256:{}\",\"seq\":101}}", "ab".repeat(32));
        assert!(parse_entry(spaced.as_bytes()).is_err(), "not JCS");
    }
}
