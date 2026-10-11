//! Offline verification of an exported audit log.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use adk_core::audit_log::{
    AUDIT_RECORD_VERSION, AuditCheckpoint, AuditHash, AuditLogEntry, AuditLogStore, AuditRecord,
    AuditRecordKind,
};
use serde::{Deserialize, Serialize};

use super::jcs::canonicalize;
use super::merkle::{leaf_hash, root};
use super::note::{CheckpointBody, Note, NoteVerifier};

/// Why a log failed verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VerifyFailureKind {
    /// An entry or checkpoint could not be parsed.
    Malformed,
    /// Entries are missing, duplicated, or out of order.
    SequenceBroken,
    /// A record's text does not hash to its stored leaf hash.
    LeafHashMismatch,
    /// A record is not in RFC 8785 canonical form.
    NotCanonical,
    /// A record's `prev_hash` does not match the previous record's leaf hash.
    ChainBroken,
    /// Records name different logs.
    LogIdMismatch,
    /// A record uses a schema version this verifier does not know.
    UnsupportedVersion,
    /// A checkpoint carries no valid signature from a trusted key.
    UntrustedCheckpoint,
    /// A checkpoint's root does not match the records.
    RootMismatch,
    /// A checkpoint covers more records than were supplied.
    Truncated,
    /// Two checkpoints of the same size have different roots.
    Fork,
    /// Checkpoints carry different origins.
    OriginMismatch,
    /// The checkpoint after a key rotation is not signed by both keys.
    RotationNotDualSigned,
    /// Records exist but no checkpoint signs them.
    Unanchored,
}

/// One verification failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyFailure {
    /// The failure kind.
    pub kind: VerifyFailureKind,
    /// The record position, when the failure concerns one record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// The checkpoint size, when the failure concerns one checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_size: Option<u64>,
    /// What was found.
    pub message: String,
}

impl fmt::Display for VerifyFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = serde_json::to_value(self.kind).ok().and_then(|v| v.as_str().map(String::from));
        write!(f, "[{}]", kind.unwrap_or_default())?;
        if let Some(seq) = self.seq {
            write!(f, " record {seq}:")?;
        }
        if let Some(size) = self.checkpoint_size {
            write!(f, " checkpoint {size}:")?;
        }
        write!(f, " {}", self.message)
    }
}

/// The result of [`verify`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    /// The log id the records carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_id: Option<String>,
    /// The checkpoint origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Number of entries supplied.
    pub records: u64,
    /// Entries whose body was pruned; only their hashes were checked.
    pub pruned: u64,
    /// Checkpoints whose signature and root verified.
    pub checkpoints_verified: u64,
    /// Largest verified checkpoint size; records up to it are signed.
    pub verified_size: u64,
    /// Root hash at `verified_size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_root: Option<AuditHash>,
    /// Records after the last verified checkpoint. Removing them is not detectable
    /// until a later checkpoint or an anchor covers them.
    pub unsigned_tail: u64,
    /// Verifier keys learned from dual-signed rotation checkpoints.
    pub learned_keys: Vec<String>,
    /// Every failure found.
    pub failures: Vec<VerifyFailure>,
}

impl VerifyReport {
    /// Whether the log verified with no failures.
    pub fn is_valid(&self) -> bool {
        self.failures.is_empty()
    }

    fn fail(
        &mut self,
        kind: VerifyFailureKind,
        seq: Option<u64>,
        size: Option<u64>,
        message: String,
    ) {
        self.failures.push(VerifyFailure { kind, seq, checkpoint_size: size, message });
    }
}

impl fmt::Display for VerifyReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "log:                  {}", self.log_id.as_deref().unwrap_or("-"))?;
        writeln!(f, "origin:               {}", self.origin.as_deref().unwrap_or("-"))?;
        writeln!(f, "records:              {} ({} pruned)", self.records, self.pruned)?;
        writeln!(f, "checkpoints verified: {}", self.checkpoints_verified)?;
        writeln!(f, "signed through:       {}", self.verified_size)?;
        if let Some(root) = &self.verified_root {
            writeln!(f, "root:                 {root}")?;
        }
        writeln!(f, "unsigned tail:        {}", self.unsigned_tail)?;
        for key in &self.learned_keys {
            writeln!(f, "learned key:          {key}")?;
        }
        if self.is_valid() {
            write!(f, "result:               VALID")
        } else {
            writeln!(f, "result:               INVALID ({} failures)", self.failures.len())?;
            for failure in &self.failures {
                writeln!(f, "  {failure}")?;
            }
            Ok(())
        }
    }
}

struct Rotation {
    seq: u64,
    previous: Option<NoteVerifier>,
    next: Option<NoteVerifier>,
}

/// Verifies an audit log against its signed checkpoints and the keys you trust.
///
/// Checks, in order:
///
/// 1. Entries run from seq 0 with no gaps, each record hashes to its leaf hash, is in
///    canonical form, names one log, and links to the previous leaf through `prev_hash`.
/// 2. Each checkpoint is signed by a trusted key, its root equals the tree over the
///    first `size` records, it covers no more records than were supplied, and no two
///    checkpoints of one size disagree.
/// 3. A `key_rotated` record is followed by a checkpoint signed by both keys; the next
///    key is then trusted and the previous one retired.
///
/// Flipping a byte, deleting, reordering, or truncating covered records, and forking the
/// log under one key, each produce a failure. Records after the last checkpoint are
/// reported as `unsigned_tail`.
///
/// # Example
///
/// ```rust
/// use adk_auth::signed_audit::verify;
///
/// let report = verify(&[], &[], &[]);
/// assert!(report.is_valid());
/// ```
pub fn verify(
    entries: &[AuditLogEntry],
    checkpoints: &[AuditCheckpoint],
    trusted_keys: &[NoteVerifier],
) -> VerifyReport {
    use VerifyFailureKind as Kind;
    let mut report = VerifyReport { records: entries.len() as u64, ..VerifyReport::default() };
    let mut leaves = Vec::with_capacity(entries.len());
    let mut previous_leaf: Option<AuditHash> = None;
    let mut rotations = Vec::new();

    for (position, entry) in entries.iter().enumerate() {
        let position = position as u64;
        if entry.seq != position {
            report.fail(
                Kind::SequenceBroken,
                Some(position),
                None,
                format!("entry at position {position} has seq {}", entry.seq),
            );
        }
        let Some(text) = &entry.record else {
            report.pruned += 1;
            leaves.push(entry.leaf_hash);
            previous_leaf = Some(entry.leaf_hash);
            continue;
        };
        let leaf = leaf_hash(text.as_bytes());
        if leaf != entry.leaf_hash {
            report.fail(
                Kind::LeafHashMismatch,
                Some(position),
                None,
                "record text does not hash to its stored leaf hash".into(),
            );
        }
        leaves.push(leaf);
        let value: serde_json::Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(error) => {
                report.fail(Kind::Malformed, Some(position), None, format!("not JSON: {error}"));
                previous_leaf = Some(leaf);
                continue;
            }
        };
        if canonicalize(&value).ok().as_deref() != Some(text.as_str()) {
            report.fail(
                Kind::NotCanonical,
                Some(position),
                None,
                "record is not RFC 8785 canonical".into(),
            );
        }
        let record: AuditRecord = match serde_json::from_value(value) {
            Ok(record) => record,
            Err(error) => {
                report.fail(
                    Kind::Malformed,
                    Some(position),
                    None,
                    format!("not an audit record: {error}"),
                );
                previous_leaf = Some(leaf);
                continue;
            }
        };
        if record.v != AUDIT_RECORD_VERSION {
            report.fail(
                Kind::UnsupportedVersion,
                Some(position),
                None,
                format!("record version {} is not {AUDIT_RECORD_VERSION}", record.v),
            );
        }
        if record.seq != position {
            report.fail(
                Kind::SequenceBroken,
                Some(position),
                None,
                format!("record at position {position} claims seq {}", record.seq),
            );
        }
        match &report.log_id {
            None => report.log_id = Some(record.log_id.clone()),
            Some(log_id) if *log_id != record.log_id => report.fail(
                Kind::LogIdMismatch,
                Some(position),
                None,
                format!("record names log '{}', expected '{log_id}'", record.log_id),
            ),
            Some(_) => {}
        }
        if record.prev_hash != previous_leaf {
            report.fail(
                Kind::ChainBroken,
                Some(position),
                None,
                "prev_hash does not match the previous record's leaf hash".into(),
            );
        }
        if record.kind == AuditRecordKind::KeyRotated {
            let key = |name: &str| {
                record
                    .ext
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .and_then(|k| k.parse().ok())
            };
            rotations.push(Rotation {
                seq: position,
                previous: key("previousKey"),
                next: key("nextKey"),
            });
        }
        previous_leaf = Some(leaf);
    }

    let mut trusted: Vec<NoteVerifier> = trusted_keys.to_vec();
    let mut retired: HashSet<NoteVerifier> = HashSet::new();
    let mut ordered: Vec<&AuditCheckpoint> = checkpoints.iter().collect();
    ordered.sort_by_key(|checkpoint| checkpoint.size);
    let mut roots_by_size: BTreeMap<u64, AuditHash> = BTreeMap::new();
    let mut rotations = rotations.into_iter().peekable();

    for checkpoint in ordered {
        let size = Some(checkpoint.size);
        let note = match Note::parse(&checkpoint.note) {
            Ok(note) => note,
            Err(error) => {
                report.fail(Kind::Malformed, None, size, error.to_string());
                continue;
            }
        };
        let body = match CheckpointBody::parse(&note.text) {
            Ok(body) => body,
            Err(error) => {
                report.fail(Kind::Malformed, None, size, error.to_string());
                continue;
            }
        };
        if body.size != checkpoint.size || body.root_hash != checkpoint.root_hash {
            report.fail(
                Kind::Malformed,
                None,
                size,
                "stored size or root differs from the signed note".into(),
            );
            continue;
        }
        match &report.origin {
            None => report.origin = Some(body.origin.clone()),
            Some(origin) if *origin != body.origin => {
                report.fail(
                    Kind::OriginMismatch,
                    None,
                    size,
                    format!("origin '{}' differs", body.origin),
                );
                continue;
            }
            Some(_) => {}
        }
        if let Some(previous_root) = roots_by_size.insert(body.size, body.root_hash)
            && previous_root != body.root_hash
        {
            report.fail(
                Kind::Fork,
                None,
                size,
                "two signed checkpoints of this size disagree".into(),
            );
        }
        let active: Vec<NoteVerifier> =
            trusted.iter().filter(|key| !retired.contains(*key)).cloned().collect();
        if note.verified_by(&active).is_empty() {
            report.fail(
                Kind::UntrustedCheckpoint,
                None,
                size,
                "no valid signature from a trusted key".into(),
            );
            continue;
        }
        while let Some(rotation) = rotations.next_if(|rotation| rotation.seq < body.size) {
            let (Some(previous), Some(next)) = (rotation.previous, rotation.next) else {
                report.fail(
                    Kind::Malformed,
                    Some(rotation.seq),
                    None,
                    "key_rotated record lacks valid previousKey and nextKey".into(),
                );
                continue;
            };
            let both = [previous.clone(), next.clone()];
            if note.verified_by(&both).len() == 2 && active.contains(&previous) {
                if !trusted.contains(&next) {
                    report.learned_keys.push(next.to_string());
                    trusted.push(next);
                }
                retired.insert(previous);
            } else {
                report.fail(
                    Kind::RotationNotDualSigned,
                    Some(rotation.seq),
                    size,
                    "the first checkpoint after the rotation is not signed by the trusted previous key and the next key"
                        .into(),
                );
            }
        }
        if body.size > leaves.len() as u64 {
            report.fail(
                Kind::Truncated,
                None,
                size,
                format!(
                    "checkpoint covers {} records but {} were supplied",
                    body.size,
                    leaves.len()
                ),
            );
            continue;
        }
        if root(&leaves[..body.size as usize]) != body.root_hash {
            report.fail(
                Kind::RootMismatch,
                None,
                size,
                "root hash does not match the records".into(),
            );
            continue;
        }
        report.checkpoints_verified += 1;
        if body.size >= report.verified_size {
            report.verified_size = body.size;
            report.verified_root = Some(body.root_hash);
        }
    }

    if !entries.is_empty() && report.checkpoints_verified == 0 && report.failures.is_empty() {
        report.fail(Kind::Unanchored, None, None, "no checkpoint signs these records".into());
    }
    report.unsigned_tail = (leaves.len() as u64).saturating_sub(report.verified_size);
    report
}

/// One line of an audit export: an entry or a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ExportLine {
    /// A log entry.
    Entry(AuditLogEntry),
    /// A signed checkpoint.
    Checkpoint(AuditCheckpoint),
}

/// A log's entries and checkpoints, as read from a store or an export file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditExport {
    /// Entries in file order.
    pub entries: Vec<AuditLogEntry>,
    /// Checkpoints in file order.
    pub checkpoints: Vec<AuditCheckpoint>,
}

impl AuditExport {
    /// Reads every entry and checkpoint of `log_id` from `store`.
    ///
    /// # Errors
    ///
    /// Store errors.
    pub async fn from_store(store: &dyn AuditLogStore, log_id: &str) -> adk_core::Result<Self> {
        let size = store.size(log_id).await?;
        Ok(Self {
            entries: store.read(log_id, 0..size).await?,
            checkpoints: store.checkpoints(log_id).await?,
        })
    }

    /// Writes the export as JSON lines: entries first, then checkpoints.
    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        let lines = self
            .entries
            .iter()
            .cloned()
            .map(ExportLine::Entry)
            .chain(self.checkpoints.iter().cloned().map(ExportLine::Checkpoint));
        for line in lines {
            out.push_str(&serde_json::to_string(&line).expect("export lines serialize"));
            out.push('\n');
        }
        out
    }

    /// Parses an export written by [`to_jsonl`](Self::to_jsonl).
    ///
    /// # Errors
    ///
    /// Returns the 1-based line number and the parse error of the first bad line.
    pub fn from_jsonl(text: &str) -> Result<Self, (usize, serde_json::Error)> {
        let mut export = Self::default();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line).map_err(|e| (index + 1, e))? {
                ExportLine::Entry(entry) => export.entries.push(entry),
                ExportLine::Checkpoint(checkpoint) => export.checkpoints.push(checkpoint),
            }
        }
        Ok(export)
    }

    /// Verifies the export with [`verify`].
    pub fn verify(&self, trusted_keys: &[NoteVerifier]) -> VerifyReport {
        verify(&self.entries, &self.checkpoints, trusted_keys)
    }
}
