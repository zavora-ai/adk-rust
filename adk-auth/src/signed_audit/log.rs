//! The signed audit log writer.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{
    AuditActor, AuditAppend, AuditCheckpoint, AuditDecision, AuditHash, AuditLogEntry,
    AuditLogStore, AuditRecord, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome,
};
use adk_core::principal::SubjectRef;
use adk_core::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::jcs::canonicalize;
use super::merkle::{
    Frontier, consistency_ranges, fold_range, inclusion_ranges, leaf_hash, range_nodes,
};
use super::note::{AuditSigner, CheckpointBody, sign_note};
use crate::audit::{AuditEvent, AuditEventType, AuditOutcome, AuditSink};

/// Default number of records between checkpoints.
pub const DEFAULT_CHECKPOINT_EVERY: u64 = 256;
/// Default time between checkpoints while records keep arriving.
pub const DEFAULT_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(60);

fn audit_error(category: ErrorCategory, code: &'static str, message: String) -> AdkError {
    AdkError::new(ErrorComponent::Auth, category, code, message)
}

/// Returns `sha256:<hex>` of the RFC 8785 canonical form of `value`, the digest format
/// for `args_digest` and `payload_ref`.
///
/// # Example
///
/// ```rust
/// use adk_auth::signed_audit::json_sha256;
/// use serde_json::json;
///
/// let a = json_sha256(&json!({ "to": "acct-1", "amount": 5 })).unwrap();
/// let b = json_sha256(&json!({ "amount": 5, "to": "acct-1" })).unwrap();
/// assert_eq!(a, b);
/// assert!(a.starts_with("sha256:"));
/// ```
///
/// # Errors
///
/// `audit_log.not_canonical` when `value` holds an integer outside ±2^53.
pub fn json_sha256(value: &serde_json::Value) -> Result<String> {
    let canonical = canonicalize(value).map_err(|e| {
        audit_error(ErrorCategory::InvalidInput, "audit_log.not_canonical", e.to_string())
    })?;
    Ok(format!("sha256:{}", AuditHash(Sha256::digest(canonical.as_bytes()).into())))
}

/// Publishes signed checkpoints somewhere the log's operator cannot rewrite: a witness,
/// a transparency log, a timestamp authority, or a write-once file.
///
/// Anchors are optional. A failing anchor is logged and does not fail the append.
#[async_trait]
pub trait CheckpointAnchor: Send + Sync {
    /// A short name for logs.
    fn name(&self) -> &str;

    /// Publishes `checkpoint`.
    ///
    /// # Errors
    ///
    /// Returns an error when the checkpoint could not be published.
    async fn anchor(&self, checkpoint: &AuditCheckpoint) -> Result<()>;
}

/// A [`CheckpointAnchor`] that appends each checkpoint as a JSON line to a file, such as
/// one on a write-once mount or a separately administered host.
///
/// [`read`](Self::read) returns the checkpoints for [`verify`](super::verify), which
/// detects a log rewritten after the anchored checkpoints were written.
///
/// # Example
///
/// ```rust,no_run
/// use adk_auth::signed_audit::FileCheckpointAnchor;
///
/// let anchor = FileCheckpointAnchor::new("/mnt/worm/audit-checkpoints.jsonl");
/// let checkpoints = FileCheckpointAnchor::read("/mnt/worm/audit-checkpoints.jsonl").unwrap();
/// # let _ = (anchor, checkpoints);
/// ```
#[derive(Debug)]
pub struct FileCheckpointAnchor {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileCheckpointAnchor {
    /// Anchors to the file at `path`, creating it on first write.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Mutex::new(()) }
    }

    /// Reads every checkpoint the anchor wrote.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or a line is not a checkpoint.
    pub fn read(path: impl AsRef<std::path::Path>) -> Result<Vec<AuditCheckpoint>> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            audit_error(ErrorCategory::Unavailable, "audit_log.anchor_failed", e.to_string())
        })?;
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line).map_err(|e| {
                    audit_error(
                        ErrorCategory::InvalidInput,
                        "audit_log.anchor_failed",
                        format!("anchor line is not a checkpoint: {e}"),
                    )
                })
            })
            .collect()
    }
}

#[async_trait]
impl CheckpointAnchor for FileCheckpointAnchor {
    fn name(&self) -> &str {
        "file"
    }

    async fn anchor(&self, checkpoint: &AuditCheckpoint) -> Result<()> {
        use std::io::Write;
        let line = serde_json::to_string(checkpoint).map_err(|e| {
            audit_error(ErrorCategory::Internal, "audit_log.anchor_failed", e.to_string())
        })?;
        let _guard = self.lock.lock().await;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut file| {
                writeln!(file, "{line}")?;
                file.sync_data()
            })
            .map_err(|e| {
                audit_error(ErrorCategory::Unavailable, "audit_log.anchor_failed", e.to_string())
            })
    }
}

struct WriterState {
    frontier: Frontier,
    last_leaf: Option<AuditHash>,
    signer: Arc<dyn AuditSigner>,
    checkpointed_size: u64,
    checkpointed_at: DateTime<Utc>,
}

/// A signed, Merkle-hashed, append-only audit log for one replica.
///
/// Each appended record is canonicalized with RFC 8785, linked to the previous record
/// through `prev_hash`, and added as a leaf to an RFC 9162 Merkle tree. Every
/// `checkpoint_every` records, or once `checkpoint_interval` has passed since the last
/// checkpoint, the log signs a C2SP checkpoint of the tree, stores it, and hands it to
/// the configured anchors.
///
/// `AuditLog` is an [`AuditSink`], so it can back [`ProtectedTool`](crate::ProtectedTool)
/// and [`ScopedTool`](crate::ScopedTool) directly.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_auth::signed_audit::{AuditLog, Ed25519Signer};
/// use adk_core::audit_log::{AuditDecision, AuditRecordDraft, AuditRecordKind, InMemoryAuditLogStore};
///
/// # async fn demo() -> adk_core::Result<()> {
/// let signer = Arc::new(Ed25519Signer::generate("audit.example.com").unwrap());
/// let log = AuditLog::builder(Arc::new(InMemoryAuditLogStore::new()), "replica-1", signer)
///     .checkpoint_every(100)
///     .open()
///     .await?;
/// log.append(AuditRecordDraft::new(AuditRecordKind::ToolCall, "transfer").decision(AuditDecision::Allowed))
///     .await?;
/// let checkpoint = log.checkpoint().await?;
/// assert_eq!(checkpoint.size, 1);
/// # Ok(())
/// # }
/// ```
pub struct AuditLog {
    store: Arc<dyn AuditLogStore>,
    log_id: String,
    origin: String,
    clock: Arc<dyn Clock>,
    checkpoint_every: u64,
    checkpoint_interval: Duration,
    anchors: Vec<Arc<dyn CheckpointAnchor>>,
    state: Mutex<WriterState>,
}

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditLog")
            .field("log_id", &self.log_id)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

/// Builds an [`AuditLog`].
pub struct AuditLogBuilder {
    store: Arc<dyn AuditLogStore>,
    log_id: String,
    signer: Arc<dyn AuditSigner>,
    origin: Option<String>,
    clock: Arc<dyn Clock>,
    checkpoint_every: u64,
    checkpoint_interval: Duration,
    anchors: Vec<Arc<dyn CheckpointAnchor>>,
}

impl AuditLogBuilder {
    /// Sets the checkpoint origin line. Defaults to `adk-audit/<log_id>`.
    pub fn origin(mut self, origin: impl Into<String>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    /// Sets the clock that stamps records and paces checkpoints.
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Signs a checkpoint after this many records (minimum 1).
    pub fn checkpoint_every(mut self, records: u64) -> Self {
        self.checkpoint_every = records.max(1);
        self
    }

    /// Signs a checkpoint on the first append after this much time.
    pub fn checkpoint_interval(mut self, interval: Duration) -> Self {
        self.checkpoint_interval = interval;
        self
    }

    /// Adds an anchor that receives every checkpoint.
    pub fn anchor(mut self, anchor: Arc<dyn CheckpointAnchor>) -> Self {
        self.anchors.push(anchor);
        self
    }

    /// Opens the log, resuming from what the store holds.
    ///
    /// # Errors
    ///
    /// `audit_log.invalid_config` for an origin with a newline; `audit_log.corrupt` when
    /// a stored subtree hash is missing or the latest checkpoint's root does not match
    /// the stored tree; store errors otherwise.
    pub async fn open(self) -> Result<AuditLog> {
        let origin = self.origin.unwrap_or_else(|| format!("adk-audit/{}", self.log_id));
        if origin.is_empty() || origin.contains('\n') || self.log_id.is_empty() {
            return Err(audit_error(
                ErrorCategory::InvalidInput,
                "audit_log.invalid_config",
                "log_id and origin must be non-empty and the origin a single line".into(),
            ));
        }
        let size = self.store.size(&self.log_id).await?;
        let mut hashes = Vec::new();
        for (level, index) in range_nodes(0..size) {
            hashes.push(stored_hash(self.store.as_ref(), &self.log_id, level, index).await?);
        }
        let last_leaf = match size {
            0 => None,
            _ => Some(stored_hash(self.store.as_ref(), &self.log_id, 0, size - 1).await?),
        };
        let latest = self.store.latest_checkpoint(&self.log_id).await?;
        if let Some(checkpoint) = &latest {
            let root = subtree_root(self.store.as_ref(), &self.log_id, 0..checkpoint.size).await?;
            if root != checkpoint.root_hash {
                return Err(audit_error(
                    ErrorCategory::Internal,
                    "audit_log.corrupt",
                    format!(
                        "audit log '{}' no longer matches its checkpoint at size {}; \
                         stop writing and verify the log",
                        self.log_id, checkpoint.size
                    ),
                ));
            }
        }
        let now = self.clock.now();
        Ok(AuditLog {
            state: Mutex::new(WriterState {
                frontier: Frontier { size, hashes },
                last_leaf,
                signer: self.signer,
                checkpointed_size: latest.as_ref().map_or(0, |c| c.size),
                checkpointed_at: latest.map_or(now, |c| c.created_at),
            }),
            store: self.store,
            log_id: self.log_id,
            origin,
            clock: self.clock,
            checkpoint_every: self.checkpoint_every,
            checkpoint_interval: self.checkpoint_interval,
            anchors: self.anchors,
        })
    }
}

async fn stored_hash(
    store: &dyn AuditLogStore,
    log_id: &str,
    level: u8,
    index: u64,
) -> Result<AuditHash> {
    store.hash_at(log_id, level, index).await?.ok_or_else(|| {
        audit_error(
            ErrorCategory::Internal,
            "audit_log.corrupt",
            format!(
                "audit log '{log_id}' is missing the subtree hash at level {level}, index {index}"
            ),
        )
    })
}

async fn subtree_root(
    store: &dyn AuditLogStore,
    log_id: &str,
    range: std::ops::Range<u64>,
) -> Result<AuditHash> {
    let mut hashes = Vec::new();
    for (level, index) in range_nodes(range) {
        hashes.push(stored_hash(store, log_id, level, index).await?);
    }
    Ok(fold_range(&hashes))
}

impl AuditLog {
    /// Starts building a log over `store` with id `log_id`, signing with `signer`.
    ///
    /// Give every replica its own `log_id`; a second writer on the same id is refused
    /// with `audit_log.seq_conflict`.
    pub fn builder(
        store: Arc<dyn AuditLogStore>,
        log_id: impl Into<String>,
        signer: Arc<dyn AuditSigner>,
    ) -> AuditLogBuilder {
        AuditLogBuilder {
            store,
            log_id: log_id.into(),
            signer,
            origin: None,
            clock: Arc::new(SystemClock),
            checkpoint_every: DEFAULT_CHECKPOINT_EVERY,
            checkpoint_interval: DEFAULT_CHECKPOINT_INTERVAL,
            anchors: Vec::new(),
        }
    }

    /// The log id.
    pub fn log_id(&self) -> &str {
        &self.log_id
    }

    /// The checkpoint origin line.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The store the log writes to.
    pub fn store(&self) -> &Arc<dyn AuditLogStore> {
        &self.store
    }

    /// Number of records in the log.
    pub async fn size(&self) -> u64 {
        self.state.lock().await.frontier.size
    }

    /// The verifier key of the current signer.
    pub async fn verifier_key(&self) -> String {
        self.state.lock().await.signer.verifier_key()
    }

    /// Appends one record and signs a checkpoint when one is due.
    ///
    /// # Errors
    ///
    /// `audit_log.not_canonical` when an extension value is not I-JSON; store errors,
    /// including `audit_log.seq_conflict` when another writer uses this `log_id`.
    pub async fn append(&self, draft: AuditRecordDraft) -> Result<AuditRecord> {
        let mut records = self.append_batch(vec![draft]).await?;
        Ok(records.pop().expect("one draft yields one record"))
    }

    /// Appends records atomically and signs a checkpoint when one is due.
    ///
    /// # Errors
    ///
    /// As [`append`](Self::append).
    pub async fn append_batch(&self, drafts: Vec<AuditRecordDraft>) -> Result<Vec<AuditRecord>> {
        let mut state = self.state.lock().await;
        let records = self.append_locked(&mut state, drafts).await?;
        let due = state.frontier.size - state.checkpointed_size >= self.checkpoint_every
            || self.interval_elapsed(&state);
        if due && state.frontier.size > state.checkpointed_size {
            let signers = vec![state.signer.clone()];
            if let Err(error) = self.checkpoint_locked(&mut state, &signers).await {
                // The records are durable; the next append or `checkpoint` retries.
                tracing::warn!(audit.log_id = %self.log_id, error = %error, "audit checkpoint failed");
            }
        }
        Ok(records)
    }

    fn interval_elapsed(&self, state: &WriterState) -> bool {
        let elapsed = self.clock.now().signed_duration_since(state.checkpointed_at);
        elapsed.to_std().is_ok_and(|elapsed| elapsed >= self.checkpoint_interval)
    }

    async fn append_locked(
        &self,
        state: &mut WriterState,
        drafts: Vec<AuditRecordDraft>,
    ) -> Result<Vec<AuditRecord>> {
        if drafts.is_empty() {
            return Ok(Vec::new());
        }
        let ts = self.clock.now();
        let mut frontier = state.frontier.clone();
        let mut prev = state.last_leaf;
        let mut records = Vec::with_capacity(drafts.len());
        let mut entries = Vec::with_capacity(drafts.len());
        let mut nodes = Vec::new();
        for draft in drafts {
            let record = draft.into_record(&self.log_id, frontier.size, ts, prev);
            let value = serde_json::to_value(&record).map_err(|e| {
                audit_error(ErrorCategory::Internal, "audit_log.not_canonical", e.to_string())
            })?;
            let canonical = canonicalize(&value).map_err(|e| {
                audit_error(ErrorCategory::InvalidInput, "audit_log.not_canonical", e.to_string())
            })?;
            let leaf = leaf_hash(canonical.as_bytes());
            entries.push(AuditLogEntry {
                seq: record.seq,
                leaf_hash: leaf,
                record: Some(canonical),
            });
            nodes.extend(frontier.push(leaf));
            prev = Some(leaf);
            records.push(record);
        }
        self.store.append(AuditAppend { log_id: self.log_id.clone(), entries, nodes }).await?;
        state.frontier = frontier;
        state.last_leaf = prev;
        Ok(records)
    }

    async fn checkpoint_locked(
        &self,
        state: &mut WriterState,
        signers: &[Arc<dyn AuditSigner>],
    ) -> Result<AuditCheckpoint> {
        let body = CheckpointBody {
            origin: self.origin.clone(),
            size: state.frontier.size,
            root_hash: state.frontier.root(),
        };
        let note = sign_note(&body.to_text(), signers).await.map_err(|e| {
            audit_error(ErrorCategory::Unavailable, "audit_log.signing_failed", e.to_string())
        })?;
        let checkpoint = AuditCheckpoint {
            log_id: self.log_id.clone(),
            size: body.size,
            root_hash: body.root_hash,
            note,
            created_at: self.clock.now(),
        };
        self.store.put_checkpoint(checkpoint.clone()).await?;
        state.checkpointed_size = checkpoint.size;
        state.checkpointed_at = checkpoint.created_at;
        for anchor in &self.anchors {
            if let Err(error) = anchor.anchor(&checkpoint).await {
                tracing::warn!(
                    audit.log_id = %self.log_id,
                    audit.anchor = anchor.name(),
                    error = %error,
                    "audit checkpoint anchor failed"
                );
            }
        }
        Ok(checkpoint)
    }

    /// Signs and stores a checkpoint of the current tree, even when nothing changed.
    ///
    /// # Errors
    ///
    /// `audit_log.signing_failed` when the signer fails; store errors otherwise.
    pub async fn checkpoint(&self) -> Result<AuditCheckpoint> {
        let mut state = self.state.lock().await;
        let signers = vec![state.signer.clone()];
        self.checkpoint_locked(&mut state, &signers).await
    }

    /// Signs a checkpoint when records were appended since the last one and the
    /// interval has passed. Returns the checkpoint when one was signed.
    ///
    /// # Errors
    ///
    /// As [`checkpoint`](Self::checkpoint).
    pub async fn checkpoint_if_due(&self) -> Result<Option<AuditCheckpoint>> {
        let mut state = self.state.lock().await;
        if state.frontier.size == state.checkpointed_size || !self.interval_elapsed(&state) {
            return Ok(None);
        }
        let signers = vec![state.signer.clone()];
        self.checkpoint_locked(&mut state, &signers).await.map(Some)
    }

    /// Spawns a task that calls [`checkpoint_if_due`](Self::checkpoint_if_due) every
    /// `checkpoint_interval`, so an idle log's last records are still checkpointed.
    pub fn spawn_checkpoint_timer(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let log = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval(log.checkpoint_interval.max(Duration::from_millis(10)));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if let Err(error) = log.checkpoint_if_due().await {
                    tracing::warn!(audit.log_id = %log.log_id, error = %error, "audit checkpoint failed");
                }
            }
        })
    }

    /// Rotates the checkpoint signing key.
    ///
    /// Appends a `key_rotated` record naming both verifier keys, signs the next
    /// checkpoint with the old and the new key, and signs later checkpoints with the
    /// new key only. A verifier that trusts the old key learns the new one from that
    /// dual-signed checkpoint.
    ///
    /// # Errors
    ///
    /// As [`append`](Self::append) and [`checkpoint`](Self::checkpoint). When the
    /// checkpoint fails, the old key stays active; call `rotate_signer` again.
    pub async fn rotate_signer(
        &self,
        next: Arc<dyn AuditSigner>,
        actor: AuditActor,
    ) -> Result<AuditCheckpoint> {
        let mut state = self.state.lock().await;
        let previous = state.signer.clone();
        let draft = AuditRecordDraft::new(AuditRecordKind::KeyRotated, "rotate_checkpoint_key")
            .actor(actor)
            .ext("previousKey", serde_json::Value::String(previous.verifier_key()))
            .ext("nextKey", serde_json::Value::String(next.verifier_key()));
        self.append_locked(&mut state, vec![draft]).await?;
        let checkpoint = self.checkpoint_locked(&mut state, &[previous, next.clone()]).await?;
        state.signer = next;
        Ok(checkpoint)
    }

    /// The RFC 9162 inclusion proof for record `seq` in the tree of `size` records.
    ///
    /// # Errors
    ///
    /// `audit_log.out_of_range` when `seq >= size` or `size` exceeds the log.
    pub async fn inclusion_proof(&self, seq: u64, size: u64) -> Result<Vec<AuditHash>> {
        self.check_size(size).await?;
        if seq >= size {
            return Err(adk_core::audit_log::out_of_range_error(&self.log_id, size, seq));
        }
        let mut proof = Vec::new();
        for range in inclusion_ranges(seq, size) {
            proof.push(subtree_root(self.store.as_ref(), &self.log_id, range).await?);
        }
        Ok(proof)
    }

    /// The RFC 9162 consistency proof from the tree of `old` records to that of `new`.
    ///
    /// # Errors
    ///
    /// `audit_log.out_of_range` when `old > new` or `new` exceeds the log.
    pub async fn consistency_proof(&self, old: u64, new: u64) -> Result<Vec<AuditHash>> {
        self.check_size(new).await?;
        if old > new {
            return Err(adk_core::audit_log::out_of_range_error(&self.log_id, new, old));
        }
        let mut proof = Vec::new();
        for range in consistency_ranges(old, new) {
            proof.push(subtree_root(self.store.as_ref(), &self.log_id, range).await?);
        }
        Ok(proof)
    }

    /// The root hash of the tree over the first `size` records.
    ///
    /// # Errors
    ///
    /// `audit_log.out_of_range` when `size` exceeds the log.
    pub async fn root_at(&self, size: u64) -> Result<AuditHash> {
        self.check_size(size).await?;
        subtree_root(self.store.as_ref(), &self.log_id, 0..size).await
    }

    async fn check_size(&self, size: u64) -> Result<()> {
        let current = self.size().await;
        if size > current {
            return Err(adk_core::audit_log::out_of_range_error(&self.log_id, current, size));
        }
        Ok(())
    }
}

/// Maps an [`AuditEvent`] onto a record draft. The event's type becomes the record kind,
/// its user the `on_behalf_of` subject, and its context fields extension fields.
fn draft_from_event(event: AuditEvent) -> AuditRecordDraft {
    let kind = match &event.event_type {
        AuditEventType::ToolAccess
        | AuditEventType::AgentAccess
        | AuditEventType::PermissionCheck => AuditRecordKind::Access,
        AuditEventType::Authentication => AuditRecordKind::Authentication,
        AuditEventType::ConfigChanged => AuditRecordKind::Configuration,
        AuditEventType::PaymentExecuted => AuditRecordKind::Payment,
        AuditEventType::PolicyEvaluated | AuditEventType::Authorization => {
            AuditRecordKind::Governance
        }
        AuditEventType::ResourceCreated => AuditRecordKind::Other("resource_created".into()),
        AuditEventType::ResourceUpdated => AuditRecordKind::Other("resource_updated".into()),
        AuditEventType::ResourceDeleted => AuditRecordKind::Other("resource_deleted".into()),
        AuditEventType::SecretAccessed => AuditRecordKind::Other("secret_accessed".into()),
        AuditEventType::SecretRotated => AuditRecordKind::Other("secret_rotated".into()),
        AuditEventType::SessionStarted => AuditRecordKind::Other("session_started".into()),
        AuditEventType::SessionEnded => AuditRecordKind::Other("session_ended".into()),
        AuditEventType::Custom(name) => AuditRecordKind::Other(name.clone()),
    };
    let (decision, outcome) = match event.outcome {
        AuditOutcome::Allowed => (Some(AuditDecision::Allowed), None),
        AuditOutcome::Denied | AuditOutcome::Blocked => (Some(AuditDecision::Denied), None),
        AuditOutcome::Paused | AuditOutcome::Escalated => {
            (Some(AuditDecision::ApprovalRequired), None)
        }
        AuditOutcome::Error => (None, Some(AuditRecordOutcome::Failed)),
        AuditOutcome::Created | AuditOutcome::Updated | AuditOutcome::Deleted => {
            (None, Some(AuditRecordOutcome::Succeeded))
        }
    };
    let mut draft =
        AuditRecordDraft::new(kind, event.action.clone().unwrap_or_else(|| event.resource.clone()))
            .actor(AuditActor::subject(SubjectRef::human(event.user)));
    draft.decision = decision;
    draft.outcome = outcome;
    draft.ext.insert("resource".into(), serde_json::Value::String(event.resource));
    let optional = [
        ("eventOutcome", serde_json::to_value(&event.outcome).ok()),
        ("sessionId", event.session_id.map(serde_json::Value::String)),
        ("workspaceId", event.workspace_id.map(serde_json::Value::String)),
        ("tenantId", event.tenant_id.map(serde_json::Value::String)),
        ("requestId", event.request_id.map(serde_json::Value::String)),
        ("resourceId", event.resource_id.map(serde_json::Value::String)),
        ("ipAddress", event.ip_address.map(serde_json::Value::String)),
        ("metadata", event.metadata),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            draft.ext.insert(key.into(), value);
        }
    }
    draft
}

/// The runtime's audit hook: the runner's `audit_log` setting and
/// [`AuditTrail`](adk_core::audit_log::AuditTrail) record governed actions here.
///
/// Digests are [`json_sha256`]; a value with an integer outside ±2^53, which RFC 8785
/// cannot canonicalize, is digested as `sha256-json:<hex>` over its compact JSON text.
#[async_trait]
impl adk_core::audit_log::AuditRecorder for AuditLog {
    fn digest(&self, value: &serde_json::Value) -> String {
        json_sha256(value).unwrap_or_else(|_| {
            let text = serde_json::to_string(value).unwrap_or_default();
            format!("sha256-json:{}", AuditHash(Sha256::digest(text.as_bytes()).into()))
        })
    }

    async fn record(&self, draft: AuditRecordDraft) -> Result<()> {
        self.append(draft).await.map(drop)
    }
}

#[async_trait]
impl AuditSink for AuditLog {
    async fn log(&self, event: AuditEvent) -> std::result::Result<(), crate::AuthError> {
        self.append(draft_from_event(event))
            .await
            .map(drop)
            .map_err(|e| crate::AuthError::AuditError(e.to_string()))
    }

    async fn log_batch(
        &self,
        events: Vec<AuditEvent>,
    ) -> std::result::Result<(), crate::AuthError> {
        self.append_batch(events.into_iter().map(draft_from_event).collect())
            .await
            .map(drop)
            .map_err(|e| crate::AuthError::AuditError(e.to_string()))
    }

    /// Signs a checkpoint when records were appended since the last one.
    async fn flush(&self) -> std::result::Result<(), crate::AuthError> {
        let mut state = self.state.lock().await;
        if state.frontier.size == state.checkpointed_size {
            return Ok(());
        }
        let signers = vec![state.signer.clone()];
        self.checkpoint_locked(&mut state, &signers)
            .await
            .map(drop)
            .map_err(|e| crate::AuthError::AuditError(e.to_string()))
    }
}
