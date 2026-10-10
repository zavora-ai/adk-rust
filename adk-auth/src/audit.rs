//! Enterprise audit logging for access control and platform operations.
//!
//! Provides structured audit events with multi-tenant context, extensible event
//! types, multiple sink backends, batch logging, cryptographic chaining, and
//! query/retention APIs.
//!
//! # Enterprise Features
//!
//! - **Multi-tenant context**: `workspace_id`, `tenant_id`, `request_id` fields
//! - **Extensible event types**: 15 built-in types + `Custom(String)` for platform extensions
//! - **Multiple outcomes**: 9 outcome variants covering full lifecycle
//! - **Batch logging**: `log_batch()` for high-throughput scenarios
//! - **Cryptographic chaining**: Optional SHA-256 hash chain for append-only integrity
//! - **Query interface**: `AuditFilter` for searching audit logs from the UI
//! - **Retention API**: `purge_before()` for compliance-driven data lifecycle
//! - **Multiple sinks**: File (JSONL), PostgreSQL, OpenTelemetry export

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use zeroize::Zeroizing;

/// Type of audit event.
///
/// Covers access control, resource lifecycle, configuration changes, and
/// platform-specific operations. Use `Custom(String)` for platform extensions
/// without forking the crate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    /// Tool access attempt.
    ToolAccess,
    /// Agent access attempt.
    AgentAccess,
    /// Permission check.
    PermissionCheck,
    /// Authentication event (login, token refresh, logout).
    Authentication,
    /// Authorization decision (role/policy evaluation).
    Authorization,
    /// Resource created (agent, workspace, project).
    ResourceCreated,
    /// Resource updated.
    ResourceUpdated,
    /// Resource deleted.
    ResourceDeleted,
    /// Configuration changed (settings, feature flags).
    ConfigChanged,
    /// Secret accessed (read from vault).
    SecretAccessed,
    /// Secret rotated (key rotation event).
    SecretRotated,
    /// Payment executed (billing event).
    PaymentExecuted,
    /// Policy evaluated (guardrail, rate limit).
    PolicyEvaluated,
    /// Session started.
    SessionStarted,
    /// Session ended.
    SessionEnded,
    /// Platform-specific custom event type.
    Custom(String),
}

/// Outcome of an audit event.
///
/// Covers the full lifecycle of access decisions and resource operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// Access was allowed.
    Allowed,
    /// Access was denied.
    Denied,
    /// An error occurred during the operation.
    Error,
    /// Resource was created successfully.
    Created,
    /// Resource was updated successfully.
    Updated,
    /// Resource was deleted successfully.
    Deleted,
    /// Operation was blocked by a policy or guardrail.
    Blocked,
    /// Operation was paused (awaiting approval).
    Paused,
    /// Operation was escalated to a higher authority.
    Escalated,
}

/// An audit event with enterprise multi-tenant context.
///
/// All fields beyond `timestamp`, `user`, `event_type`, `resource`, and `outcome`
/// are optional for backward compatibility. Enterprise platforms populate the
/// additional context fields for multi-tenancy, tracing, and compliance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Timestamp of the event.
    pub timestamp: DateTime<Utc>,
    /// User ID (email, subject, or system identifier).
    pub user: String,
    /// Session ID (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Type of event.
    pub event_type: AuditEventType,
    /// Resource being accessed (tool name, agent name, or descriptive path).
    pub resource: String,
    /// Outcome of the operation.
    pub outcome: AuditOutcome,
    /// Additional metadata (arbitrary JSON).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,

    // ── Enterprise context fields ───────────────────────────────
    /// Workspace ID for multi-tenant scoping.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Tenant ID for multi-tenant scoping (higher level than workspace).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Request ID for distributed tracing correlation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Client IP address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    /// Resource UUID (distinct from the human-readable `resource` name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    /// Action verb (e.g., "read", "write", "delete", "execute").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// SHA-256 hash of the previous event (for cryptographic chaining).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<String>,
}

impl AuditEvent {
    /// Create a new audit event with the given type, user, resource, and outcome.
    pub fn new(
        event_type: AuditEventType,
        user: impl Into<String>,
        resource: impl Into<String>,
        outcome: AuditOutcome,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            user: user.into(),
            session_id: None,
            event_type,
            resource: resource.into(),
            outcome,
            metadata: None,
            workspace_id: None,
            tenant_id: None,
            request_id: None,
            ip_address: None,
            resource_id: None,
            action: None,
            prev_hash: None,
        }
    }

    /// Create a new tool access event.
    pub fn tool_access(user: &str, tool_name: &str, outcome: AuditOutcome) -> Self {
        Self::new(AuditEventType::ToolAccess, user, tool_name, outcome)
    }

    /// Create a new agent access event.
    pub fn agent_access(user: &str, agent_name: &str, outcome: AuditOutcome) -> Self {
        Self::new(AuditEventType::AgentAccess, user, agent_name, outcome)
    }

    /// Create an authentication event.
    pub fn authentication(user: &str, outcome: AuditOutcome) -> Self {
        Self::new(AuditEventType::Authentication, user, "auth", outcome)
    }

    /// Create a resource lifecycle event.
    pub fn resource_event(
        event_type: AuditEventType,
        user: &str,
        resource: &str,
        resource_id: &str,
        outcome: AuditOutcome,
    ) -> Self {
        Self::new(event_type, user, resource, outcome).with_resource_id(resource_id)
    }

    /// Create a secret access event.
    pub fn secret_accessed(user: &str, secret_name: &str, outcome: AuditOutcome) -> Self {
        Self::new(AuditEventType::SecretAccessed, user, secret_name, outcome)
    }

    /// Create a configuration change event.
    pub fn config_changed(user: &str, config_key: &str, outcome: AuditOutcome) -> Self {
        Self::new(AuditEventType::ConfigChanged, user, config_key, outcome)
    }

    /// Create a custom event type for platform extensions.
    pub fn custom(event_type: &str, user: &str, resource: &str, outcome: AuditOutcome) -> Self {
        Self::new(AuditEventType::Custom(event_type.to_string()), user, resource, outcome)
    }

    /// Set the session ID.
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Set metadata.
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Set workspace ID for multi-tenant scoping.
    pub fn with_workspace(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    /// Set tenant ID for multi-tenant scoping.
    pub fn with_tenant(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Set request ID for distributed tracing.
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Set client IP address.
    pub fn with_ip_address(mut self, ip: impl Into<String>) -> Self {
        self.ip_address = Some(ip.into());
        self
    }

    /// Set resource UUID.
    pub fn with_resource_id(mut self, id: impl Into<String>) -> Self {
        self.resource_id = Some(id.into());
        self
    }

    /// Set action verb.
    pub fn with_action(mut self, action: impl Into<String>) -> Self {
        self.action = Some(action.into());
        self
    }

    /// Compute and set the cryptographic hash chain link.
    ///
    /// The hash is SHA-256 of the JSON-serialized previous event.
    /// Call this before logging to maintain an append-only chain.
    pub fn with_prev_hash(mut self, prev_event_json: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(prev_event_json.as_bytes());
        self.prev_hash = Some(hex::encode(hasher.finalize()));
        self
    }

    /// Serialize this event to JSON (for hash chaining).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Filter for querying audit events.
///
/// All fields are optional — only non-None fields are used as filter criteria.
/// Multiple fields are combined with AND logic.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditFilter {
    /// Filter by user ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Filter by workspace ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Filter by tenant ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Filter by event type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_type: Option<AuditEventType>,
    /// Filter by outcome.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AuditOutcome>,
    /// Filter by resource name (substring match).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// Filter by resource UUID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    /// Events after this timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<DateTime<Utc>>,
    /// Events before this timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<DateTime<Utc>>,
    /// Maximum number of results to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Offset for pagination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

impl AuditFilter {
    /// Check if an event matches this filter.
    pub fn matches(&self, event: &AuditEvent) -> bool {
        if let Some(ref user) = self.user
            && &event.user != user
        {
            return false;
        }
        if let Some(ref ws) = self.workspace_id
            && event.workspace_id.as_ref() != Some(ws)
        {
            return false;
        }
        if let Some(ref tid) = self.tenant_id
            && event.tenant_id.as_ref() != Some(tid)
        {
            return false;
        }
        if let Some(ref et) = self.event_type
            && &event.event_type != et
        {
            return false;
        }
        if let Some(ref oc) = self.outcome
            && &event.outcome != oc
        {
            return false;
        }
        if let Some(ref res) = self.resource
            && !event.resource.contains(res.as_str())
        {
            return false;
        }
        if let Some(ref rid) = self.resource_id
            && event.resource_id.as_ref() != Some(rid)
        {
            return false;
        }
        if let Some(after) = self.after
            && event.timestamp <= after
        {
            return false;
        }
        if let Some(before) = self.before
            && event.timestamp >= before
        {
            return false;
        }
        true
    }
}

/// Trait for audit sinks.
///
/// Implementations persist audit events to a storage backend. The trait supports
/// single-event logging, batch logging for high throughput, querying for UI
/// display, and retention management for compliance.
#[async_trait::async_trait]
pub trait AuditSink: Send + Sync {
    /// Log a single audit event.
    async fn log(&self, event: AuditEvent) -> Result<(), crate::AuthError>;

    /// Log a batch of audit events atomically.
    ///
    /// Default implementation logs events sequentially. Override for
    /// high-throughput backends that support batch inserts.
    async fn log_batch(&self, events: Vec<AuditEvent>) -> Result<(), crate::AuthError> {
        for event in events {
            self.log(event).await?;
        }
        Ok(())
    }

    /// Query audit events matching the given filter.
    ///
    /// Default implementation returns an empty vec (not all sinks support queries).
    /// Override for queryable backends (PostgreSQL, in-memory).
    async fn query(&self, _filter: &AuditFilter) -> Result<Vec<AuditEvent>, crate::AuthError> {
        Ok(Vec::new())
    }

    /// Purge events older than the given cutoff timestamp.
    ///
    /// Returns the number of events purged. Default returns 0 (not all sinks
    /// support retention management).
    async fn purge_before(&self, _cutoff: DateTime<Utc>) -> Result<u64, crate::AuthError> {
        Ok(0)
    }

    /// Flush any buffered events to the underlying storage.
    ///
    /// Default is a no-op. Override for buffered sinks.
    async fn flush(&self) -> Result<(), crate::AuthError> {
        Ok(())
    }
}

/// What a guarded tool does when its audit sink fails to record an access decision.
///
/// [`ProtectedTool`](crate::ProtectedTool), [`ScopedTool`](crate::ScopedTool), and the
/// guards that build them default to [`Block`](Self::Block).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuditFailureMode {
    /// Refuse the call: an action whose decision cannot be recorded does not run.
    #[default]
    Block,
    /// Log the failure and apply the access decision without a record.
    Warn,
}

/// Records one access decision, refusing the call when the sink fails in `Block` mode.
pub(crate) async fn record_decision(
    sink: &dyn AuditSink,
    event: AuditEvent,
    mode: AuditFailureMode,
    tool_name: &str,
) -> adk_core::Result<()> {
    let Err(error) = sink.log(event).await else {
        return Ok(());
    };
    match mode {
        AuditFailureMode::Block => {
            tracing::error!(tool.name = %tool_name, error = %error, "audit sink failed; refusing the call");
            Err(adk_core::AdkError::new(
                adk_core::ErrorComponent::Auth,
                adk_core::ErrorCategory::Unavailable,
                "auth.audit_failed",
                format!(
                    "tool '{tool_name}' refused: the access decision could not be audited ({error}). \
                     Restore the audit sink, or set AuditFailureMode::Warn to run without a record"
                ),
            ))
        }
        AuditFailureMode::Warn => {
            tracing::warn!(tool.name = %tool_name, error = %error, "audit sink failed; continuing without a record");
            Ok(())
        }
    }
}

/// Link from one chained line to the next: SHA-256, or HMAC-SHA256 under `key`, of the
/// previous line's exact text, hex-encoded.
fn chain_link(key: Option<&[u8]>, previous_line: &str) -> String {
    match key {
        Some(key) => {
            let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
                .expect("HMAC-SHA256 accepts keys of any length");
            mac.update(previous_line.as_bytes());
            hex::encode(mac.finalize().into_bytes())
        }
        None => hex::encode(Sha256::digest(previous_line.as_bytes())),
    }
}

/// Reads the last non-empty line of `path` without reading the whole file.
fn read_last_line(path: &Path) -> std::io::Result<Option<String>> {
    const BLOCK: u64 = 8 * 1024;
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut end = file.metadata()?.len();
    let mut tail: Vec<u8> = Vec::new();
    while end > 0 {
        let start = end.saturating_sub(BLOCK);
        let mut block = vec![0u8; usize::try_from(end - start).unwrap_or(0)];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut block)?;
        block.extend_from_slice(&tail);
        tail = block;
        end = start;
        let content_end =
            tail.iter().rposition(|byte| !matches!(byte, b'\n' | b'\r')).map_or(0, |at| at + 1);
        tail.truncate(content_end);
        if let Some(newline) = tail.iter().rposition(|byte| *byte == b'\n') {
            tail.drain(..=newline);
            break;
        }
    }
    if tail.is_empty() {
        return Ok(None);
    }
    String::from_utf8(tail)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// File-based audit sink that writes JSONL (one JSON line per event).
///
/// With chaining, each event carries in `prev_hash` the SHA-256 (or, with
/// [`with_hmac_chaining`](Self::with_hmac_chaining), the HMAC-SHA256) of the previous
/// line's exact text. Opening a chained file resumes from its last line, so the chain
/// survives restarts, and [`verify`](Self::verify) detects a line that was edited,
/// inserted, reordered, or removed before the newest line. Without a key anyone who can
/// write the file can recompute the chain; the HMAC chain cannot be forged without the
/// key. Neither detects an edit to the newest line or lines cut from the end; anchor the
/// newest line elsewhere for that.
///
/// # Example
///
/// ```rust,no_run
/// use adk_auth::{AuditEvent, AuditOutcome, AuditSink, FileAuditSink};
///
/// # async fn demo() -> Result<(), adk_auth::AuthError> {
/// let sink = FileAuditSink::with_hmac_chaining("/var/log/adk/audit.jsonl", b"audit-key".to_vec())?;
/// sink.log(AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed)).await?;
/// let events = sink.verify()?;
/// # let _ = events;
/// # Ok(())
/// # }
/// ```
pub struct FileAuditSink {
    writer: Mutex<BufWriter<File>>,
    path: PathBuf,
    /// Exact text of the last line written, which the next event links to.
    last_event_json: Mutex<Option<String>>,
    /// Whether to enable cryptographic hash chaining.
    chain_enabled: bool,
    /// Key for the HMAC chain; `None` chains with plain SHA-256.
    chain_key: Option<Zeroizing<Vec<u8>>>,
}

impl FileAuditSink {
    /// Create a new file audit sink.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        Self::open(path.into(), false, None)
    }

    /// Create a new file audit sink with SHA-256 hash chaining enabled.
    ///
    /// Each event carries in `prev_hash` the SHA-256 of the previous line, resuming
    /// from the file's last line when it already exists.
    pub fn with_chaining(path: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        Self::open(path.into(), true, None)
    }

    /// Create a new file audit sink whose chain links are HMAC-SHA256 under `key`.
    ///
    /// The chain cannot be recomputed without `key`, so an edit by someone who can write
    /// the file but does not hold the key fails [`verify`](Self::verify).
    pub fn with_hmac_chaining(
        path: impl Into<PathBuf>,
        key: impl Into<Vec<u8>>,
    ) -> Result<Self, std::io::Error> {
        Self::open(path.into(), true, Some(Zeroizing::new(key.into())))
    }

    fn open(
        path: PathBuf,
        chain_enabled: bool,
        chain_key: Option<Zeroizing<Vec<u8>>>,
    ) -> Result<Self, std::io::Error> {
        let last_event_json = if chain_enabled { read_last_line(&path)? } else { None };
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            writer: Mutex::new(BufWriter::new(file)),
            path,
            last_event_json: Mutex::new(last_event_json),
            chain_enabled,
            chain_key,
        })
    }

    /// Get the path to the audit log file.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Verifies this sink's chain and returns the number of events in the file.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::AuditError`](crate::AuthError::AuditError) naming the first
    /// line whose link does not match, or an I/O error when the file cannot be read.
    pub fn verify(&self) -> Result<u64, crate::AuthError> {
        self.writer.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).flush()?;
        Self::verify_file(&self.path, self.chain_key.as_deref().map(Vec::as_slice))
    }

    /// Verifies a chained audit file without opening it for writing.
    ///
    /// Pass the key the file was written with, or `None` for a SHA-256 chain. The first
    /// event must carry no `prev_hash`; every later event must link to the line before it.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::AuditError`](crate::AuthError::AuditError) naming the first
    /// line that is not an audit event or whose link does not match, or an I/O error.
    pub fn verify_file(
        path: impl AsRef<Path>,
        hmac_key: Option<&[u8]>,
    ) -> Result<u64, crate::AuthError> {
        let reader = BufReader::new(File::open(path)?);
        let mut previous: Option<String> = None;
        let mut events = 0u64;
        for (index, line) in reader.lines().enumerate() {
            let line = line?;
            let number = index + 1;
            let event: AuditEvent = serde_json::from_str(&line).map_err(|error| {
                crate::AuthError::AuditError(format!(
                    "audit chain broken at line {number}: not an audit event ({error})"
                ))
            })?;
            let expected = previous.as_deref().map(|previous| chain_link(hmac_key, previous));
            if event.prev_hash != expected {
                return Err(crate::AuthError::AuditError(format!(
                    "audit chain broken at line {number}: prev_hash does not match the line before it"
                )));
            }
            previous = Some(line);
            events += 1;
        }
        Ok(events)
    }
}

#[async_trait::async_trait]
impl AuditSink for FileAuditSink {
    async fn log(&self, mut event: AuditEvent) -> Result<(), crate::AuthError> {
        // Held across the write so concurrent events link in the order they are written.
        let mut last = self.last_event_json.lock().unwrap_or_else(|p| p.into_inner());
        if self.chain_enabled {
            event.prev_hash = last
                .as_deref()
                .map(|previous| chain_link(self.chain_key.as_deref().map(Vec::as_slice), previous));
        }
        let line = serde_json::to_string(&event)
            .map_err(|e| crate::AuthError::AuditError(e.to_string()))?;

        let mut writer = self.writer.lock().unwrap_or_else(|poisoned| {
            tracing::warn!(path = %self.path.display(), "audit writer lock poisoned, recovering");
            poisoned.into_inner()
        });
        writeln!(writer, "{line}")?;
        writer.flush()?;
        if self.chain_enabled {
            // Advanced only after the write, so a failed write does not break the chain.
            *last = Some(line);
        }
        Ok(())
    }
}

/// In-memory audit sink for testing and development.
///
/// Stores all events in a `Vec` behind a `RwLock`. Supports querying and
/// purging. Not suitable for production — use `FileAuditSink` or a database
/// sink instead.
pub struct InMemoryAuditSink {
    events: tokio::sync::RwLock<Vec<AuditEvent>>,
}

impl InMemoryAuditSink {
    /// Create a new in-memory audit sink.
    pub fn new() -> Self {
        Self { events: tokio::sync::RwLock::new(Vec::new()) }
    }

    /// Get the number of stored events.
    pub async fn len(&self) -> usize {
        self.events.read().await.len()
    }

    /// Check if the sink is empty.
    pub async fn is_empty(&self) -> bool {
        self.events.read().await.is_empty()
    }
}

impl Default for InMemoryAuditSink {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AuditSink for InMemoryAuditSink {
    async fn log(&self, event: AuditEvent) -> Result<(), crate::AuthError> {
        self.events.write().await.push(event);
        Ok(())
    }

    async fn log_batch(&self, events: Vec<AuditEvent>) -> Result<(), crate::AuthError> {
        self.events.write().await.extend(events);
        Ok(())
    }

    async fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditEvent>, crate::AuthError> {
        let events = self.events.read().await;
        let mut results: Vec<AuditEvent> =
            events.iter().filter(|e| filter.matches(e)).cloned().collect();

        // Apply offset
        if let Some(offset) = filter.offset {
            if offset < results.len() {
                results = results[offset..].to_vec();
            } else {
                results.clear();
            }
        }

        // Apply limit
        if let Some(limit) = filter.limit {
            results.truncate(limit);
        }

        Ok(results)
    }

    async fn purge_before(&self, cutoff: DateTime<Utc>) -> Result<u64, crate::AuthError> {
        let mut events = self.events.write().await;
        let before_len = events.len();
        events.retain(|e| e.timestamp >= cutoff);
        Ok((before_len - events.len()) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_event_serialization() {
        let event = AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"user\":\"alice\""));
        assert!(json.contains("\"resource\":\"search\""));
        assert!(json.contains("\"outcome\":\"allowed\""));
    }

    #[test]
    fn test_audit_event_with_session() {
        let event = AuditEvent::tool_access("bob", "exec", AuditOutcome::Denied)
            .with_session("session-123");
        assert_eq!(event.session_id, Some("session-123".to_string()));
    }

    #[test]
    fn test_enterprise_context_fields() {
        let event = AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed)
            .with_workspace("ws_123")
            .with_tenant("tenant_abc")
            .with_request_id("req_456")
            .with_ip_address("192.168.1.1")
            .with_resource_id("550e8400-e29b-41d4-a716-446655440000")
            .with_action("execute");

        assert_eq!(event.workspace_id, Some("ws_123".to_string()));
        assert_eq!(event.tenant_id, Some("tenant_abc".to_string()));
        assert_eq!(event.request_id, Some("req_456".to_string()));
        assert_eq!(event.ip_address, Some("192.168.1.1".to_string()));
        assert_eq!(event.resource_id, Some("550e8400-e29b-41d4-a716-446655440000".to_string()));
        assert_eq!(event.action, Some("execute".to_string()));

        // Verify JSON serialization includes enterprise fields
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"workspace_id\":\"ws_123\""));
        assert!(json.contains("\"tenant_id\":\"tenant_abc\""));
        assert!(json.contains("\"request_id\":\"req_456\""));
    }

    #[test]
    fn test_custom_event_type() {
        let event =
            AuditEvent::custom("deployment_triggered", "ci-bot", "agent-v2", AuditOutcome::Created);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("deployment_triggered"));
        assert!(json.contains("\"outcome\":\"created\""));
    }

    #[test]
    fn test_new_outcomes() {
        for outcome in [
            AuditOutcome::Created,
            AuditOutcome::Updated,
            AuditOutcome::Deleted,
            AuditOutcome::Blocked,
            AuditOutcome::Paused,
            AuditOutcome::Escalated,
        ] {
            let event =
                AuditEvent::new(AuditEventType::ResourceCreated, "user", "res", outcome.clone());
            let json = serde_json::to_string(&event).unwrap();
            let deserialized: AuditEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.outcome, outcome);
        }
    }

    #[test]
    fn test_new_event_types() {
        let types = vec![
            AuditEventType::Authentication,
            AuditEventType::Authorization,
            AuditEventType::ResourceCreated,
            AuditEventType::ResourceUpdated,
            AuditEventType::ResourceDeleted,
            AuditEventType::ConfigChanged,
            AuditEventType::SecretAccessed,
            AuditEventType::SecretRotated,
            AuditEventType::PaymentExecuted,
            AuditEventType::PolicyEvaluated,
            AuditEventType::SessionStarted,
            AuditEventType::SessionEnded,
            AuditEventType::Custom("my_event".to_string()),
        ];

        for event_type in types {
            let event =
                AuditEvent::new(event_type.clone(), "user", "resource", AuditOutcome::Allowed);
            let json = serde_json::to_string(&event).unwrap();
            let deserialized: AuditEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.event_type, event_type);
        }
    }

    #[test]
    fn last_line_is_read_across_block_boundaries() {
        let path = std::env::temp_dir().join(format!(
            "adk-audit-last-line-{}-{}.jsonl",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        assert_eq!(read_last_line(&path).unwrap(), None);

        std::fs::write(&path, "").unwrap();
        assert_eq!(read_last_line(&path).unwrap(), None);

        let long = "x".repeat(20_000);
        std::fs::write(&path, format!("first\n{long}\n\n")).unwrap();
        assert_eq!(read_last_line(&path).unwrap(), Some(long.clone()));

        std::fs::write(&path, &long).unwrap();
        assert_eq!(read_last_line(&path).unwrap(), Some(long));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_hash_chaining() {
        let event1 = AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed);
        let json1 = event1.to_json().unwrap();

        let event2 =
            AuditEvent::tool_access("bob", "exec", AuditOutcome::Denied).with_prev_hash(&json1);

        assert!(event2.prev_hash.is_some());
        // SHA-256 hex is 64 chars
        assert_eq!(event2.prev_hash.as_ref().unwrap().len(), 64);
    }

    #[test]
    fn test_audit_filter_matches() {
        let event = AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed)
            .with_workspace("ws_1");

        let filter = AuditFilter { user: Some("alice".to_string()), ..Default::default() };
        assert!(filter.matches(&event));

        let filter = AuditFilter { user: Some("bob".to_string()), ..Default::default() };
        assert!(!filter.matches(&event));

        let filter = AuditFilter { workspace_id: Some("ws_1".to_string()), ..Default::default() };
        assert!(filter.matches(&event));

        let filter = AuditFilter { workspace_id: Some("ws_2".to_string()), ..Default::default() };
        assert!(!filter.matches(&event));
    }

    #[test]
    fn test_backward_compatible_serialization() {
        // Old-style event without enterprise fields should still deserialize
        let old_json = r#"{"timestamp":"2026-01-01T00:00:00Z","user":"alice","event_type":"tool_access","resource":"search","outcome":"allowed"}"#;
        let event: AuditEvent = serde_json::from_str(old_json).unwrap();
        assert_eq!(event.user, "alice");
        assert_eq!(event.workspace_id, None);
        assert_eq!(event.tenant_id, None);
    }

    #[tokio::test]
    async fn test_in_memory_sink_query() {
        let sink = InMemoryAuditSink::new();

        sink.log(
            AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed)
                .with_workspace("ws_1"),
        )
        .await
        .unwrap();
        sink.log(
            AuditEvent::tool_access("bob", "exec", AuditOutcome::Denied).with_workspace("ws_1"),
        )
        .await
        .unwrap();
        sink.log(
            AuditEvent::tool_access("alice", "deploy", AuditOutcome::Allowed)
                .with_workspace("ws_2"),
        )
        .await
        .unwrap();

        // Query by user
        let results = sink
            .query(&AuditFilter { user: Some("alice".to_string()), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(results.len(), 2);

        // Query by workspace
        let results = sink
            .query(&AuditFilter { workspace_id: Some("ws_1".to_string()), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(results.len(), 2);

        // Query by outcome
        let results = sink
            .query(&AuditFilter { outcome: Some(AuditOutcome::Denied), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].user, "bob");
    }

    #[tokio::test]
    async fn test_in_memory_sink_purge() {
        let sink = InMemoryAuditSink::new();

        sink.log(AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed)).await.unwrap();
        sink.log(AuditEvent::tool_access("bob", "exec", AuditOutcome::Denied)).await.unwrap();

        assert_eq!(sink.len().await, 2);

        // Purge everything before now+1s (should purge all)
        let cutoff = Utc::now() + chrono::Duration::seconds(1);
        let purged = sink.purge_before(cutoff).await.unwrap();
        assert_eq!(purged, 2);
        assert!(sink.is_empty().await);
    }

    #[tokio::test]
    async fn test_in_memory_sink_batch() {
        let sink = InMemoryAuditSink::new();

        let events = vec![
            AuditEvent::tool_access("alice", "a", AuditOutcome::Allowed),
            AuditEvent::tool_access("bob", "b", AuditOutcome::Denied),
            AuditEvent::tool_access("carol", "c", AuditOutcome::Allowed),
        ];

        sink.log_batch(events).await.unwrap();
        assert_eq!(sink.len().await, 3);
    }
}
