//! Append-only audit log records and the store that holds them.
//!
//! An [`AuditRecord`](crate::audit_log::AuditRecord) is one entry in a per-replica log identified by `log_id`. A
//! writer (`adk_auth::signed_audit::AuditLog`) canonicalizes each record, hashes it into
//! a Merkle tree, and signs checkpoints of the tree. The
//! [`AuditLogStore`](crate::audit_log::AuditLogStore) persists the
//! result and computes nothing itself:
//!
//! | Data | Stored as |
//! |------|-----------|
//! | Record | Canonical JSON text plus its leaf hash, addressed by `(log_id, seq)` |
//! | Tree node | Hash of a complete subtree, addressed by `(log_id, level, index)` |
//! | Checkpoint | A signed note over `(size, root)`, addressed by `(log_id, size)` |
//!
//! Each replica writes its own log, so no shared sequencer is needed. `append` is a
//! compare-and-swap on the log size: a second writer using the same `log_id` gets
//! `audit_log.seq_conflict` instead of forking the log. Pruning drops record bodies
//! and keeps their hashes, so signed checkpoints and proofs stay verifiable.
//!
//! [`InMemoryAuditLogStore`](crate::audit_log::InMemoryAuditLogStore) is the reference implementation; `adk-session` ships SQLite,
//! PostgreSQL, Redis, MongoDB, Neo4j, and Firestore ones. Every backend passes the same
//! conformance suite (`adk_core::audit_log_store_conformance_tests!`, `conformance` feature).
//!
//! The runtime writes records through an [`AuditRecorder`](crate::audit_log::AuditRecorder)
//! wrapped in an [`AuditTrail`](crate::audit_log::AuditTrail), set on
//! [`RunConfig::audit`](crate::RunConfig::audit): the governed path records every tool and
//! approval decision, the kill switch its freezes, payment tools their payments, and agents
//! every call answered with an unknown outcome.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::ops::Range;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::principal::{ActingContext, AgentId, SubjectRef};
use crate::{AdkError, ErrorCategory, ErrorComponent, Result};

/// Version of the [`AuditRecord`] schema this crate writes.
pub const AUDIT_RECORD_VERSION: u32 = 1;

/// A SHA-256 hash, written as 64 lowercase hex characters.
///
/// # Example
///
/// ```rust
/// use adk_core::audit_log::AuditHash;
///
/// let hash: AuditHash = "00".repeat(32).parse().unwrap();
/// assert_eq!(hash, AuditHash([0; 32]));
/// assert_eq!(hash.to_string(), "00".repeat(32));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AuditHash(pub [u8; 32]);

impl AuditHash {
    /// Returns the raw bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for AuditHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for AuditHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AuditHash({self})")
    }
}

impl FromStr for AuditHash {
    type Err = InvalidAuditHash;

    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return Err(InvalidAuditHash(format!(
                "expected 64 hex characters, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 32];
        for (index, pair) in bytes.chunks_exact(2).enumerate() {
            let digit = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                _ => None,
            };
            match (digit(pair[0]), digit(pair[1])) {
                (Some(high), Some(low)) => out[index] = (high << 4) | low,
                _ => {
                    return Err(InvalidAuditHash(
                        "expected lowercase hex characters (0-9, a-f)".to_string(),
                    ));
                }
            }
        }
        Ok(Self(out))
    }
}

impl TryFrom<String> for AuditHash {
    type Error = InvalidAuditHash;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<AuditHash> for String {
    fn from(value: AuditHash) -> Self {
        value.to_string()
    }
}

/// Text that is not a hex-encoded SHA-256 hash.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid audit hash: {0}")]
pub struct InvalidAuditHash(String);

/// What an [`AuditRecord`] is about. Serialized as a snake_case string; names this
/// enum does not know round-trip through [`Other`](Self::Other).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
#[non_exhaustive]
pub enum AuditRecordKind {
    /// A tool call and its governance decision.
    ToolCall,
    /// An approval requested, granted, or refused.
    Approval,
    /// A governance change such as a freeze.
    Governance,
    /// A payment.
    Payment,
    /// An access-control decision.
    Access,
    /// An authentication event.
    Authentication,
    /// A configuration change.
    Configuration,
    /// The log's checkpoint signing key changed.
    KeyRotated,
    /// Any other kind.
    Other(String),
}

impl AuditRecordKind {
    /// Returns the wire name.
    pub fn as_str(&self) -> &str {
        match self {
            Self::ToolCall => "tool_call",
            Self::Approval => "approval",
            Self::Governance => "governance",
            Self::Payment => "payment",
            Self::Access => "access",
            Self::Authentication => "authentication",
            Self::Configuration => "configuration",
            Self::KeyRotated => "key_rotated",
            Self::Other(name) => name,
        }
    }
}

impl From<String> for AuditRecordKind {
    fn from(value: String) -> Self {
        match value.as_str() {
            "tool_call" => Self::ToolCall,
            "approval" => Self::Approval,
            "governance" => Self::Governance,
            "payment" => Self::Payment,
            "access" => Self::Access,
            "authentication" => Self::Authentication,
            "configuration" => Self::Configuration,
            "key_rotated" => Self::KeyRotated,
            _ => Self::Other(value),
        }
    }
}

impl From<AuditRecordKind> for String {
    fn from(value: AuditRecordKind) -> Self {
        match value {
            AuditRecordKind::Other(name) => name,
            known => known.as_str().to_string(),
        }
    }
}

/// The authorization decision an [`AuditRecord`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditDecision {
    /// The action was allowed.
    Allowed,
    /// The action was denied.
    Denied,
    /// The action needs approval and was held.
    ApprovalRequired,
    /// An approver granted the action.
    Approved,
    /// An approver refused the action.
    Rejected,
}

/// What happened when the recorded action ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditRecordOutcome {
    /// The action completed.
    Succeeded,
    /// The action failed.
    Failed,
    /// The action may or may not have taken effect (timeout, crash).
    OutcomeUnknown,
    /// The action was not run.
    NotRun,
}

/// Who acted: the agent and the subject it acted for, taken from an [`ActingContext`].
///
/// # Example
///
/// ```rust
/// use adk_core::audit_log::AuditActor;
/// use adk_core::principal::SubjectRef;
///
/// let actor = AuditActor::subject(SubjectRef::human("alice"));
/// assert!(actor.agent.is_none());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditActor {
    /// The acting agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    /// The tenant the agent runs for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// The deployment or replica.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// The subject the agent acted on behalf of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<SubjectRef>,
    /// The delegation chain, outermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegation_chain: Vec<SubjectRef>,
    /// The invocation the action belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_id: Option<String>,
}

impl AuditActor {
    /// An actor that is a subject with no agent, such as a human administrator.
    pub fn subject(subject: SubjectRef) -> Self {
        Self { on_behalf_of: Some(subject), ..Self::default() }
    }
}

impl AuditActor {
    /// The actor for `context` acting in `invocation_id`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::audit_log::AuditActor;
    /// use adk_core::{ActingContext, SubjectKind};
    ///
    /// let acting = ActingContext::derived("shop", "cashier", "alice", SubjectKind::Human);
    /// let actor = AuditActor::new(&acting, "inv-1");
    /// assert_eq!(actor.invocation_id.as_deref(), Some("inv-1"));
    /// assert_eq!(actor.agent, Some(acting.agent.id));
    /// ```
    pub fn new(context: &ActingContext, invocation_id: impl Into<String>) -> Self {
        let invocation_id = invocation_id.into();
        Self {
            agent: Some(context.agent.id.clone()),
            tenant: context.agent.tenant.clone(),
            instance: context.agent.instance.clone(),
            on_behalf_of: context.on_behalf_of.clone(),
            delegation_chain: context.delegation_chain.clone(),
            invocation_id: (!invocation_id.is_empty()).then_some(invocation_id),
        }
    }
}

/// One entry in an audit log.
///
/// The writer fills `v`, `log_id`, `seq`, `ts`, and `prev_hash`; callers supply the
/// rest as an [`AuditRecordDraft`]. Personal or large data does not belong in the
/// record: store it elsewhere and put its digest in `payload_ref`, so deleting the
/// payload (crypto-shredding) leaves the chain verifiable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRecord {
    /// Schema version, [`AUDIT_RECORD_VERSION`].
    pub v: u32,
    /// The log this record belongs to, one per replica.
    pub log_id: String,
    /// Position in the log, from 0 with no gaps.
    pub seq: u64,
    /// When the writer appended the record.
    pub ts: DateTime<Utc>,
    /// Leaf hash of the previous record; absent for `seq` 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<AuditHash>,
    /// Who acted.
    pub actor: AuditActor,
    /// What the record is about.
    pub kind: AuditRecordKind,
    /// The action, such as a tool name or an operation.
    pub action: String,
    /// Digest of the action's canonical arguments, such as `sha256:<hex>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_digest: Option<String>,
    /// The authorization decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<AuditDecision>,
    /// What happened when the action ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AuditRecordOutcome>,
    /// The policy rule or version that decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_ref: Option<String>,
    /// Digest of a payload stored outside the log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_ref: Option<String>,
    /// Extension fields. Values must be I-JSON: integers within ±2^53.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ext: BTreeMap<String, serde_json::Value>,
}

/// The caller-supplied part of an [`AuditRecord`].
///
/// # Example
///
/// ```rust
/// use adk_core::audit_log::{AuditDecision, AuditRecordDraft, AuditRecordKind};
///
/// let draft = AuditRecordDraft::new(AuditRecordKind::ToolCall, "transfer")
///     .decision(AuditDecision::Allowed)
///     .args_digest("sha256:00");
/// assert_eq!(draft.action, "transfer");
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct AuditRecordDraft {
    /// Who acted.
    pub actor: AuditActor,
    /// What the record is about.
    pub kind: AuditRecordKind,
    /// The action.
    pub action: String,
    /// Digest of the canonical arguments.
    pub args_digest: Option<String>,
    /// The authorization decision.
    pub decision: Option<AuditDecision>,
    /// What happened.
    pub outcome: Option<AuditRecordOutcome>,
    /// The deciding policy.
    pub policy_ref: Option<String>,
    /// Digest of an out-of-log payload.
    pub payload_ref: Option<String>,
    /// Extension fields.
    pub ext: BTreeMap<String, serde_json::Value>,
}

impl AuditRecordDraft {
    /// Starts a draft for `action` of `kind` with an empty actor.
    pub fn new(kind: AuditRecordKind, action: impl Into<String>) -> Self {
        Self {
            actor: AuditActor::default(),
            kind,
            action: action.into(),
            args_digest: None,
            decision: None,
            outcome: None,
            policy_ref: None,
            payload_ref: None,
            ext: BTreeMap::new(),
        }
    }

    /// Sets the actor.
    pub fn actor(mut self, actor: AuditActor) -> Self {
        self.actor = actor;
        self
    }

    /// Sets the argument digest.
    pub fn args_digest(mut self, digest: impl Into<String>) -> Self {
        self.args_digest = Some(digest.into());
        self
    }

    /// Sets the decision.
    pub fn decision(mut self, decision: AuditDecision) -> Self {
        self.decision = Some(decision);
        self
    }

    /// Sets the outcome.
    pub fn outcome(mut self, outcome: AuditRecordOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    /// Sets the policy reference.
    pub fn policy_ref(mut self, policy_ref: impl Into<String>) -> Self {
        self.policy_ref = Some(policy_ref.into());
        self
    }

    /// Sets the payload digest.
    pub fn payload_ref(mut self, digest: impl Into<String>) -> Self {
        self.payload_ref = Some(digest.into());
        self
    }

    /// Adds an extension field.
    pub fn ext(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.ext.insert(key.into(), value);
        self
    }

    /// Completes the record with the fields the writer assigns.
    pub fn into_record(
        self,
        log_id: impl Into<String>,
        seq: u64,
        ts: DateTime<Utc>,
        prev_hash: Option<AuditHash>,
    ) -> AuditRecord {
        AuditRecord {
            v: AUDIT_RECORD_VERSION,
            log_id: log_id.into(),
            seq,
            ts,
            prev_hash,
            actor: self.actor,
            kind: self.kind,
            action: self.action,
            args_digest: self.args_digest,
            decision: self.decision,
            outcome: self.outcome,
            policy_ref: self.policy_ref,
            payload_ref: self.payload_ref,
            ext: self.ext,
        }
    }
}

/// A stored log entry: the record's canonical JSON and its leaf hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditLogEntry {
    /// Position in the log.
    pub seq: u64,
    /// `SHA-256(0x00 ‖ record)`, the RFC 9162 leaf hash.
    pub leaf_hash: AuditHash,
    /// The record's canonical JSON text; `None` once pruned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
}

/// The hash of a complete Merkle subtree covering leaves
/// `[index << level, (index + 1) << level)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditTreeNode {
    /// Height above the leaves; leaves are level 0 and are stored with their entries.
    pub level: u8,
    /// Position within the level.
    pub index: u64,
    /// The subtree hash.
    pub hash: AuditHash,
}

/// Entries to append to one log, with the tree nodes they complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditAppend {
    /// The log to append to.
    pub log_id: String,
    /// Entries with contiguous `seq`, the first equal to the log's current size.
    pub entries: Vec<AuditLogEntry>,
    /// Subtree nodes (level 1 and above) these entries complete.
    pub nodes: Vec<AuditTreeNode>,
}

/// A signed checkpoint of a log: its size and root hash in a signed note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditCheckpoint {
    /// The log this checkpoint covers.
    pub log_id: String,
    /// Number of records covered.
    pub size: u64,
    /// The RFC 9162 root hash over the first `size` records.
    pub root_hash: AuditHash,
    /// The C2SP signed note carrying the checkpoint and its signatures.
    pub note: String,
    /// When the writer signed it.
    pub created_at: DateTime<Utc>,
}

/// Persistence for append-only, Merkle-hashed audit logs.
///
/// The store holds what a writer computed and never derives hashes itself. Its
/// guarantees:
///
/// - `append` is atomic and succeeds only when the first entry's `seq` equals the log's
///   size; otherwise `audit_log.seq_conflict`. Two writers on one `log_id` cannot fork it.
/// - A checkpoint never claims more records than the log holds, and a second checkpoint
///   at the same size with a different root is refused (`audit_log.checkpoint_conflict`).
///   One with the same root replaces the note, which adds cosignatures.
/// - `prune` removes record bodies and keeps every hash.
///
/// # Example
///
/// ```rust
/// use adk_core::audit_log::{AuditAppend, AuditHash, AuditLogEntry, AuditLogStore, InMemoryAuditLogStore};
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = InMemoryAuditLogStore::new();
/// let entry = AuditLogEntry { seq: 0, leaf_hash: AuditHash([1; 32]), record: Some("{}".into()) };
/// let range = store
///     .append(AuditAppend { log_id: "replica-1".into(), entries: vec![entry], nodes: vec![] })
///     .await?;
/// assert_eq!(range, 0..1);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait AuditLogStore: Send + Sync {
    /// Appends a batch and returns the sequence range it occupies.
    ///
    /// # Errors
    ///
    /// `audit_log.seq_conflict` when the first `seq` is not the log's size;
    /// `audit_log.invalid_batch` when the batch is empty, not contiguous, holds a pruned
    /// entry, or names a node outside the appended range.
    async fn append(&self, batch: AuditAppend) -> Result<Range<u64>>;

    /// Returns the number of records in the log; 0 for a log never written.
    async fn size(&self, log_id: &str) -> Result<u64>;

    /// Returns the entries in `range`, in order.
    ///
    /// # Errors
    ///
    /// `audit_log.out_of_range` when `range` extends past the log's size.
    async fn read(&self, log_id: &str, range: Range<u64>) -> Result<Vec<AuditLogEntry>>;

    /// Returns the hash of the complete subtree at `(level, index)`, or `None` when the
    /// log has no such subtree yet. Level 0 returns leaf hashes.
    async fn hash_at(&self, log_id: &str, level: u8, index: u64) -> Result<Option<AuditHash>>;

    /// Stores a signed checkpoint.
    ///
    /// # Errors
    ///
    /// `audit_log.out_of_range` when `size` exceeds the log's size;
    /// `audit_log.checkpoint_conflict` when a checkpoint of the same size has a
    /// different root.
    async fn put_checkpoint(&self, checkpoint: AuditCheckpoint) -> Result<()>;

    /// Returns the checkpoint with the largest size.
    async fn latest_checkpoint(&self, log_id: &str) -> Result<Option<AuditCheckpoint>>;

    /// Returns every checkpoint of the log, smallest size first.
    async fn checkpoints(&self, log_id: &str) -> Result<Vec<AuditCheckpoint>>;

    /// Drops the bodies of records with `seq < before_seq`, keeping their hashes, and
    /// returns how many bodies it dropped.
    ///
    /// # Errors
    ///
    /// `audit_log.out_of_range` when `before_seq` exceeds the log's size.
    async fn prune(&self, log_id: &str, before_seq: u64) -> Result<u64>;

    /// Returns the ids of every log in the store, sorted.
    async fn logs(&self) -> Result<Vec<String>>;
}

fn audit_error(category: ErrorCategory, code: &'static str, message: String) -> AdkError {
    AdkError::new(ErrorComponent::Auth, category, code, message)
}

/// The error a store returns when `append` does not start at the log's size.
pub fn seq_conflict_error(log_id: &str, expected: u64, got: u64) -> AdkError {
    audit_error(
        ErrorCategory::Conflict,
        "audit_log.seq_conflict",
        format!(
            "audit log '{log_id}' has {expected} records but the append starts at {got}. \
             Another writer is using this log_id; give each replica its own log_id"
        ),
    )
}

/// The error a store returns when a read, prune, or checkpoint passes the log's end.
pub fn out_of_range_error(log_id: &str, size: u64, requested: u64) -> AdkError {
    audit_error(
        ErrorCategory::NotFound,
        "audit_log.out_of_range",
        format!("audit log '{log_id}' has {size} records; position {requested} is past its end"),
    )
}

/// The error a store returns when a checkpoint would equivocate.
pub fn checkpoint_conflict_error(log_id: &str, size: u64) -> AdkError {
    audit_error(
        ErrorCategory::Conflict,
        "audit_log.checkpoint_conflict",
        format!(
            "audit log '{log_id}' already has a checkpoint at size {size} with a different root; \
             the log has forked and must be investigated before it is checkpointed again"
        ),
    )
}

fn invalid_batch(log_id: &str, reason: &str) -> AdkError {
    audit_error(
        ErrorCategory::InvalidInput,
        "audit_log.invalid_batch",
        format!("audit log '{log_id}': {reason}"),
    )
}

/// Checks an append against the log's current size and returns the range it occupies.
/// Every backend calls this inside its transaction, so all of them refuse the same batches.
///
/// # Errors
///
/// `audit_log.seq_conflict` or `audit_log.invalid_batch`, as [`AuditLogStore::append`].
pub fn validate_append(batch: &AuditAppend, size: u64) -> Result<Range<u64>> {
    let log_id = batch.log_id.as_str();
    let Some(first) = batch.entries.first() else {
        return Err(invalid_batch(log_id, "the batch has no entries"));
    };
    if first.seq != size {
        return Err(seq_conflict_error(log_id, size, first.seq));
    }
    for (offset, entry) in batch.entries.iter().enumerate() {
        if entry.seq != size + offset as u64 {
            return Err(invalid_batch(log_id, "entry sequence numbers are not contiguous"));
        }
        if entry.record.is_none() {
            return Err(invalid_batch(log_id, "an appended entry has no record"));
        }
    }
    let end = size + batch.entries.len() as u64;
    for node in &batch.nodes {
        let covers = node
            .index
            .checked_add(1)
            .and_then(|next| next.checked_shl(u32::from(node.level)))
            .filter(|_| node.level >= 1 && node.level < 64);
        match covers {
            Some(node_end) if node_end <= end && node_end > size => {}
            _ => {
                return Err(invalid_batch(
                    log_id,
                    "a tree node is not completed by the appended entries",
                ));
            }
        }
    }
    Ok(size..end)
}

/// Checks a checkpoint against the log's size and an existing checkpoint at its size.
///
/// Returns `true` when an existing checkpoint should be replaced by this one.
///
/// # Errors
///
/// As [`AuditLogStore::put_checkpoint`].
pub fn validate_checkpoint(
    checkpoint: &AuditCheckpoint,
    size: u64,
    existing_root: Option<AuditHash>,
) -> Result<bool> {
    if checkpoint.size > size {
        return Err(out_of_range_error(&checkpoint.log_id, size, checkpoint.size));
    }
    match existing_root {
        Some(root) if root != checkpoint.root_hash => {
            Err(checkpoint_conflict_error(&checkpoint.log_id, checkpoint.size))
        }
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

#[derive(Debug, Default)]
struct MemoryLog {
    entries: Vec<AuditLogEntry>,
    nodes: HashMap<(u8, u64), AuditHash>,
    checkpoints: BTreeMap<u64, AuditCheckpoint>,
}

/// In-memory [`AuditLogStore`] for tests and single-process development.
///
/// Clones share the same logs.
#[derive(Debug, Clone, Default)]
pub struct InMemoryAuditLogStore {
    logs: Arc<tokio::sync::RwLock<HashMap<String, MemoryLog>>>,
}

impl InMemoryAuditLogStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AuditLogStore for InMemoryAuditLogStore {
    async fn append(&self, batch: AuditAppend) -> Result<Range<u64>> {
        let mut logs = self.logs.write().await;
        let size = logs.get(&batch.log_id).map_or(0, |log| log.entries.len() as u64);
        let range = validate_append(&batch, size)?;
        let log = logs.entry(batch.log_id).or_default();
        log.entries.extend(batch.entries);
        for node in batch.nodes {
            log.nodes.insert((node.level, node.index), node.hash);
        }
        Ok(range)
    }

    async fn size(&self, log_id: &str) -> Result<u64> {
        Ok(self.logs.read().await.get(log_id).map_or(0, |log| log.entries.len() as u64))
    }

    async fn read(&self, log_id: &str, range: Range<u64>) -> Result<Vec<AuditLogEntry>> {
        let logs = self.logs.read().await;
        let entries = logs.get(log_id).map_or(&[][..], |log| log.entries.as_slice());
        let size = entries.len() as u64;
        if range.end > size {
            return Err(out_of_range_error(log_id, size, range.end));
        }
        if range.start >= range.end {
            return Ok(Vec::new());
        }
        Ok(entries[range.start as usize..range.end as usize].to_vec())
    }

    async fn hash_at(&self, log_id: &str, level: u8, index: u64) -> Result<Option<AuditHash>> {
        let logs = self.logs.read().await;
        let Some(log) = logs.get(log_id) else {
            return Ok(None);
        };
        if level == 0 {
            return Ok(usize::try_from(index)
                .ok()
                .and_then(|at| log.entries.get(at))
                .map(|entry| entry.leaf_hash));
        }
        Ok(log.nodes.get(&(level, index)).copied())
    }

    async fn put_checkpoint(&self, checkpoint: AuditCheckpoint) -> Result<()> {
        let mut logs = self.logs.write().await;
        let log = logs.get(&checkpoint.log_id);
        let size = log.map_or(0, |log| log.entries.len() as u64);
        let existing =
            log.and_then(|log| log.checkpoints.get(&checkpoint.size)).map(|c| c.root_hash);
        validate_checkpoint(&checkpoint, size, existing)?;
        logs.entry(checkpoint.log_id.clone())
            .or_default()
            .checkpoints
            .insert(checkpoint.size, checkpoint);
        Ok(())
    }

    async fn latest_checkpoint(&self, log_id: &str) -> Result<Option<AuditCheckpoint>> {
        let logs = self.logs.read().await;
        Ok(logs.get(log_id).and_then(|log| log.checkpoints.values().next_back().cloned()))
    }

    async fn checkpoints(&self, log_id: &str) -> Result<Vec<AuditCheckpoint>> {
        let logs = self.logs.read().await;
        Ok(logs
            .get(log_id)
            .map(|log| log.checkpoints.values().cloned().collect())
            .unwrap_or_default())
    }

    async fn prune(&self, log_id: &str, before_seq: u64) -> Result<u64> {
        let mut logs = self.logs.write().await;
        let size = logs.get(log_id).map_or(0, |log| log.entries.len() as u64);
        if before_seq > size {
            return Err(out_of_range_error(log_id, size, before_seq));
        }
        let Some(log) = logs.get_mut(log_id) else {
            return Ok(0);
        };
        let mut pruned = 0;
        for entry in &mut log.entries[..before_seq as usize] {
            if entry.record.take().is_some() {
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    async fn logs(&self) -> Result<Vec<String>> {
        let mut ids: Vec<String> = self.logs.read().await.keys().cloned().collect();
        ids.sort();
        Ok(ids)
    }
}

// ---------------------------------------------------------------------------
// Runtime recording
// ---------------------------------------------------------------------------

/// What the runtime does when its [`AuditRecorder`] cannot record an action.
///
/// # Example
///
/// ```rust
/// use adk_core::audit_log::AuditFailureMode;
///
/// assert_eq!(AuditFailureMode::default(), AuditFailureMode::Block);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AuditFailureMode {
    /// Refuse the action: an action whose record cannot be written does not happen.
    #[default]
    Block,
    /// Log the failure and continue without a record.
    Warn,
}

/// Where the runtime writes audit records: governed tool decisions, approval decisions,
/// kill-switch changes, payments, and calls whose outcome is unknown.
///
/// `adk_auth::signed_audit::AuditLog` implements it with a signed, Merkle-hashed log;
/// [`InMemoryAuditRecorder`] keeps drafts in memory for tests. The trait carries no
/// cryptography, so `adk-core` stays free of it: the recorder also computes the argument
/// digests that records carry in place of arguments.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, AuditRecorder, InMemoryAuditRecorder};
///
/// # futures::executor::block_on(async {
/// let recorder = InMemoryAuditRecorder::new();
/// let digest = recorder.digest(&serde_json::json!({ "amount": 5 }));
/// recorder
///     .record(AuditRecordDraft::new(AuditRecordKind::ToolCall, "transfer").args_digest(digest))
///     .await?;
/// assert_eq!(recorder.records().len(), 1);
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[async_trait]
pub trait AuditRecorder: Send + Sync {
    /// Digest of `value`'s canonical JSON, such as `sha256:<hex>`, for
    /// [`AuditRecord::args_digest`].
    fn digest(&self, value: &serde_json::Value) -> String;

    /// Appends one record.
    ///
    /// # Errors
    ///
    /// Returns the recorder's error when the record cannot be written.
    async fn record(&self, draft: AuditRecordDraft) -> Result<()>;
}

/// An [`AuditRecorder`] with the [`AuditFailureMode`] that governs it.
///
/// Set on [`RunConfig::audit`](crate::RunConfig::audit) (usually through the runner
/// builder's `audit_log`), attached to a [`GovernanceControl`](crate::GovernanceControl),
/// or given to a payment caller. Clones share the recorder.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::audit_log::{AuditFailureMode, AuditTrail, InMemoryAuditRecorder};
///
/// let trail = AuditTrail::new(Arc::new(InMemoryAuditRecorder::new()))
///     .with_failure_mode(AuditFailureMode::Warn);
/// assert_eq!(trail.failure_mode(), AuditFailureMode::Warn);
/// ```
#[derive(Clone)]
pub struct AuditTrail {
    recorder: Arc<dyn AuditRecorder>,
    failure_mode: AuditFailureMode,
}

impl fmt::Debug for AuditTrail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuditTrail").field("failure_mode", &self.failure_mode).finish()
    }
}

impl AuditTrail {
    /// Records through `recorder`, refusing actions it cannot record
    /// ([`AuditFailureMode::Block`]).
    pub fn new(recorder: Arc<dyn AuditRecorder>) -> Self {
        Self { recorder, failure_mode: AuditFailureMode::Block }
    }

    /// Sets what happens when the recorder fails.
    #[must_use]
    pub fn with_failure_mode(mut self, mode: AuditFailureMode) -> Self {
        self.failure_mode = mode;
        self
    }

    /// The failure mode.
    pub fn failure_mode(&self) -> AuditFailureMode {
        self.failure_mode
    }

    /// The recorder.
    pub fn recorder(&self) -> &Arc<dyn AuditRecorder> {
        &self.recorder
    }

    /// Digest of `value` by the recorder.
    pub fn digest(&self, value: &serde_json::Value) -> String {
        self.recorder.digest(value)
    }

    /// Records `draft`, applying the failure mode.
    ///
    /// # Errors
    ///
    /// Under [`AuditFailureMode::Block`], `audit.record_failed` when the recorder fails;
    /// the caller refuses the action. Under [`AuditFailureMode::Warn`], never.
    pub async fn record(&self, draft: AuditRecordDraft) -> Result<()> {
        let action = draft.action.clone();
        let Err(error) = self.recorder.record(draft).await else {
            return Ok(());
        };
        match self.failure_mode {
            AuditFailureMode::Block => {
                tracing::error!(audit.action = %action, error = %error, "audit record failed; refusing the action");
                Err(AdkError::new(
                    ErrorComponent::Auth,
                    ErrorCategory::Unavailable,
                    "audit.record_failed",
                    format!(
                        "'{action}' refused: its audit record could not be written ({error}). \
                         Restore the audit log, or set AuditFailureMode::Warn to continue \
                         without a record"
                    ),
                ))
            }
            AuditFailureMode::Warn => {
                tracing::warn!(audit.action = %action, error = %error, "audit record failed; continuing without a record");
                Ok(())
            }
        }
    }
}

impl AuditActor {
    /// The actor for an agent of `app_name` running in `invocation_id` for `user_id`,
    /// when the run configures no acting context: the agent id is
    /// `urn:adk:agent:<app>/<agent>` and the subject is `user_id` as a human.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::audit_log::AuditActor;
    /// use adk_core::principal::SubjectRef;
    ///
    /// let actor = AuditActor::for_invocation("shop", "cashier", "alice", "inv-1");
    /// assert_eq!(actor.agent.unwrap().as_str(), "urn:adk:agent:shop/cashier");
    /// assert_eq!(actor.on_behalf_of, Some(SubjectRef::human("alice")));
    /// ```
    pub fn for_invocation(
        app_name: &str,
        agent_name: &str,
        user_id: &str,
        invocation_id: &str,
    ) -> Self {
        Self::new(
            &ActingContext::derived(app_name, agent_name, user_id, crate::SubjectKind::Human),
            invocation_id,
        )
    }

    /// The actor of the agent running in `ctx`: its
    /// [acting context](crate::ReadonlyContext::acting_context), which is the run's
    /// configured agent principal and subject, or the derived identity of
    /// [`for_invocation`](Self::for_invocation) when the run configures none.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let actor = AuditActor::for_context(tool_ctx.as_ref());
    /// ```
    pub fn for_context(ctx: &dyn crate::ReadonlyContext) -> Self {
        Self::new(&ctx.acting_context(), ctx.invocation_id())
    }
}

/// An [`AuditRecorder`] that keeps drafts in memory, for tests.
///
/// Its digest is the non-cryptographic [`json_digest`](crate::json_digest); a
/// production recorder uses SHA-256.
///
/// # Example
///
/// ```rust
/// use adk_core::audit_log::InMemoryAuditRecorder;
///
/// let recorder = InMemoryAuditRecorder::new();
/// assert!(recorder.records().is_empty());
/// ```
#[derive(Debug, Clone, Default)]
pub struct InMemoryAuditRecorder {
    records: Arc<std::sync::Mutex<Vec<AuditRecordDraft>>>,
}

impl InMemoryAuditRecorder {
    /// Creates an empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// The drafts recorded so far, in order.
    pub fn records(&self) -> Vec<AuditRecordDraft> {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl AuditRecorder for InMemoryAuditRecorder {
    fn digest(&self, value: &serde_json::Value) -> String {
        crate::json_digest(value)
    }

    async fn record(&self, draft: AuditRecordDraft) -> Result<()> {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).push(draft);
        Ok(())
    }
}

/// An [`AuditRecorder`] that appends each record to a file as one line of JSON, so the
/// records survive a restart (spec 3.5 §7.9, §9.3).
///
/// Each line is an [`AuditRecord`] whose `seq` counts the file's records from 0 and whose
/// `ts` is the time it was appended. Several processes may append to one file: each append
/// takes an exclusive lock on it and counts the records another process added since its
/// last append, so `seq` has no gap and no repeat. A line a crash left unfinished is cut
/// before the next append, and [`read`](Self::read) leaves it out. Every append is flushed
/// to disk before [`record`](AuditRecorder::record) returns.
///
/// The records are not hash-chained or signed; `adk_auth::signed_audit::AuditLog` over a
/// durable [`AuditLogStore`] keeps a tamper-evident log. The digest is `sha256:` and the hex
/// SHA-256 of the value's canonical JSON.
///
/// # Example
///
/// ```rust
/// use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, AuditRecorder, FileAuditRecorder};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let dir = std::env::temp_dir().join(format!("adk-audit-{}", uuid::Uuid::new_v4()));
/// let path = dir.join("audit.jsonl");
/// let recorder = FileAuditRecorder::open(&path)?;
/// recorder.record(AuditRecordDraft::new(AuditRecordKind::Other("site".into()), "site.release.built")).await?;
///
/// let records = FileAuditRecorder::read(&path)?;
/// assert_eq!((records[0].seq, records[0].action.as_str()), (0, "site.release.built"));
/// # std::fs::remove_dir_all(dir).ok();
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FileAuditRecorder {
    path: std::path::PathBuf,
    log_id: String,
    state: Arc<std::sync::Mutex<FileAuditState>>,
}

/// What the recorder knows of its file: the length it last saw and the records before it.
#[derive(Debug, Default)]
struct FileAuditState {
    len: u64,
    records: u64,
}

impl fmt::Debug for FileAuditRecorder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileAuditRecorder")
            .field("path", &self.path)
            .field("log_id", &self.log_id)
            .finish_non_exhaustive()
    }
}

fn file_audit_error(path: &std::path::Path, action: &str, error: impl fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Auth,
        ErrorCategory::Unavailable,
        "audit.file_failed",
        format!(
            "could not {action} the audit file {}: {error}. Check that the directory exists \
             and is writable, and that the disk has space",
            path.display()
        ),
    )
}

/// The number of complete lines in `bytes` and the length up to the last one.
fn complete_lines(bytes: &[u8]) -> (u64, u64) {
    let lines = bytes.iter().filter(|byte| **byte == b'\n').count() as u64;
    let end = bytes.iter().rposition(|byte| *byte == b'\n').map_or(0, |at| at as u64 + 1);
    (lines, end)
}

impl FileAuditRecorder {
    /// Appends to the file at `path`, creating it and its directory when missing; the log
    /// id is `file:` and the path.
    ///
    /// # Errors
    ///
    /// `audit.file_failed` when the directory cannot be created or the file opened.
    pub fn open(path: impl Into<std::path::PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|error| file_audit_error(&path, "create the directory of", error))?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| file_audit_error(&path, "open", error))?;
        Ok(Self {
            log_id: format!("file:{}", path.display()),
            path,
            state: Arc::new(std::sync::Mutex::new(FileAuditState::default())),
        })
    }

    /// Names the log the records belong to, such as the site's domain.
    #[must_use]
    pub fn with_log_id(mut self, log_id: impl Into<String>) -> Self {
        self.log_id = log_id.into();
        self
    }

    /// The file.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The records in the file at `path`, in order; none when it does not exist.
    ///
    /// # Errors
    ///
    /// `audit.file_failed` when the file cannot be read or a complete line is not a record.
    pub fn read(path: impl AsRef<std::path::Path>) -> Result<Vec<AuditRecord>> {
        let path = path.as_ref();
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(file_audit_error(path, "read", error)),
        };
        let (_, end) = complete_lines(&bytes);
        bytes[..end as usize]
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .enumerate()
            .map(|(index, line)| {
                serde_json::from_slice(line).map_err(|error| {
                    file_audit_error(path, &format!("read record {index} of"), error)
                })
            })
            .collect()
    }

    /// The records this recorder's file holds, in order.
    ///
    /// # Errors
    ///
    /// As [`read`](Self::read).
    pub async fn records(&self) -> Result<Vec<AuditRecord>> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || Self::read(path))
            .await
            .map_err(|error| file_audit_error(&self.path, "read", error))?
    }

    /// Appends `draft` as the next record under the file's lock.
    fn append(&self, draft: AuditRecordDraft) -> Result<()> {
        use std::io::{Read as _, Seek as _, SeekFrom, Write as _};

        let path = &self.path;
        // Written at the end under the exclusive lock rather than in append mode: an
        // append-only handle cannot cut an unfinished line on Windows.
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| file_audit_error(path, "open", error))?;
        file.lock().map_err(|error| file_audit_error(path, "lock", error))?;
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let len = file.metadata().map_err(|error| file_audit_error(path, "inspect", error))?.len();
        if len != state.len {
            // Another process appended, or the file is new to this recorder: count the
            // records from where this recorder last stopped, or from the start.
            let from = if len > state.len { state.len } else { 0 };
            if from == 0 {
                state.records = 0;
            }
            let mut tail = Vec::new();
            file.seek(SeekFrom::Start(from))
                .and_then(|_| file.read_to_end(&mut tail))
                .map_err(|error| file_audit_error(path, "read", error))?;
            let (lines, end) = complete_lines(&tail);
            state.records += lines;
            state.len = from + end;
            if state.len < len {
                // A line a crash left unfinished never completed; cut it.
                file.set_len(state.len).map_err(|error| file_audit_error(path, "repair", error))?;
            }
        }
        let record = AuditRecord {
            v: AUDIT_RECORD_VERSION,
            log_id: self.log_id.clone(),
            seq: state.records,
            ts: Utc::now(),
            prev_hash: None,
            actor: draft.actor,
            kind: draft.kind,
            action: draft.action,
            args_digest: draft.args_digest,
            decision: draft.decision,
            outcome: draft.outcome,
            policy_ref: draft.policy_ref,
            payload_ref: draft.payload_ref,
            ext: draft.ext,
        };
        let mut line = serde_json::to_vec(&record)
            .map_err(|error| file_audit_error(path, "encode a record for", error))?;
        line.push(b'\n');
        file.seek(SeekFrom::Start(state.len))
            .and_then(|_| file.write_all(&line))
            .and_then(|()| file.sync_data())
            .map_err(|error| file_audit_error(path, "append to", error))?;
        state.len += line.len() as u64;
        state.records += 1;
        Ok(())
    }
}

#[async_trait]
impl AuditRecorder for FileAuditRecorder {
    fn digest(&self, value: &serde_json::Value) -> String {
        use sha2::{Digest as _, Sha256};
        use std::fmt::Write as _;

        let mut canonical = String::new();
        crate::context::write_canonical(value, &mut canonical);
        let mut digest = String::with_capacity(7 + 64);
        digest.push_str("sha256:");
        for byte in Sha256::digest(canonical.as_bytes()) {
            let _ = write!(digest, "{byte:02x}");
        }
        digest
    }

    async fn record(&self, draft: AuditRecordDraft) -> Result<()> {
        let recorder = self.clone();
        tokio::task::spawn_blocking(move || recorder.append(draft))
            .await
            .map_err(|error| file_audit_error(&self.path, "append to", error))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(action: &str) -> AuditRecordDraft {
        AuditRecordDraft::new(AuditRecordKind::Other("site".into()), action)
            .ext("releaseSeq", serde_json::json!(7))
    }

    #[tokio::test]
    async fn a_file_recorder_keeps_its_records_across_a_reopen() {
        let dir = std::env::temp_dir().join(format!("adk-audit-{}", uuid::Uuid::new_v4()));
        let path = dir.join("nested").join("audit.jsonl");
        let recorder = FileAuditRecorder::open(&path).unwrap().with_log_id("example.com");
        recorder.record(draft("site.release.built")).await.unwrap();
        recorder.record(draft("site.release.signed")).await.unwrap();
        drop(recorder);

        // A second process opens the same file, and the first appends again after it.
        let first = FileAuditRecorder::open(&path).unwrap().with_log_id("example.com");
        let second = FileAuditRecorder::open(&path).unwrap().with_log_id("example.com");
        first.record(draft("awp.keys.rotated")).await.unwrap();
        second.record(draft("awp.keys.delegated")).await.unwrap();
        first.record(draft("site.content.published")).await.unwrap();

        let records = first.records().await.unwrap();
        let read: Vec<(u64, &str)> = records.iter().map(|r| (r.seq, r.action.as_str())).collect();
        assert_eq!(
            read,
            vec![
                (0, "site.release.built"),
                (1, "site.release.signed"),
                (2, "awp.keys.rotated"),
                (3, "awp.keys.delegated"),
                (4, "site.content.published"),
            ]
        );
        assert!(records.iter().all(|r| r.log_id == "example.com" && r.v == AUDIT_RECORD_VERSION));
        assert_eq!(records[0].ext["releaseSeq"], 7);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn an_unfinished_line_is_cut_and_never_read() {
        use std::io::Write as _;

        let dir = std::env::temp_dir().join(format!("adk-audit-{}", uuid::Uuid::new_v4()));
        let path = dir.join("audit.jsonl");
        let recorder = FileAuditRecorder::open(&path).unwrap();
        recorder.record(draft("site.content.approved")).await.unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"v\":1,\"logId\":\"torn")
            .unwrap();
        assert_eq!(FileAuditRecorder::read(&path).unwrap().len(), 1, "the torn line is left out");

        let reopened = FileAuditRecorder::open(&path).unwrap();
        reopened.record(draft("site.content.withdrawn")).await.unwrap();
        let records = FileAuditRecorder::read(&path).unwrap();
        assert_eq!(
            records.iter().map(|r| (r.seq, r.action.as_str())).collect::<Vec<_>>(),
            vec![(0, "site.content.approved"), (1, "site.content.withdrawn")]
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_file_digest_is_sha256_of_the_canonical_json() {
        let dir = std::env::temp_dir().join(format!("adk-audit-{}", uuid::Uuid::new_v4()));
        let recorder = FileAuditRecorder::open(dir.join("audit.jsonl")).unwrap();
        let a = recorder.digest(&serde_json::json!({ "b": 1, "a": [1, 2] }));
        let b = recorder.digest(&serde_json::json!({ "a": [1, 2], "b": 1 }));
        assert_eq!(a, b);
        assert!(a.starts_with("sha256:") && a.len() == 7 + 64, "{a}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn hash_rejects_bad_text() {
        assert!("ab".parse::<AuditHash>().is_err());
        assert!("G".repeat(64).parse::<AuditHash>().is_err());
        assert!("AB".repeat(32).parse::<AuditHash>().is_err());
    }

    #[test]
    fn kind_round_trips_unknown_names() {
        let kind: AuditRecordKind = serde_json::from_str("\"deploy\"").unwrap();
        assert_eq!(kind, AuditRecordKind::Other("deploy".into()));
        assert_eq!(serde_json::to_string(&AuditRecordKind::KeyRotated).unwrap(), "\"key_rotated\"");
        assert_eq!(serde_json::to_string(&kind).unwrap(), "\"deploy\"");
    }
}
