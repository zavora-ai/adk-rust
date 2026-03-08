//! Audit logging for access control.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

/// Type of audit event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    /// Tool access attempt.
    ToolAccess,
    /// Agent access attempt.
    AgentAccess,
    /// Permission check.
    PermissionCheck,
}

/// Outcome of an audit event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// Access was allowed.
    Allowed,
    /// Access was denied.
    Denied,
    /// An error occurred.
    Error,
}

/// An audit event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Timestamp of the event.
    pub timestamp: DateTime<Utc>,
    /// User ID.
    pub user: String,
    /// Session ID (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Type of event.
    pub event_type: AuditEventType,
    /// Resource being accessed (tool name, agent name).
    pub resource: String,
    /// Outcome of the access attempt.
    pub outcome: AuditOutcome,
    /// Additional metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl AuditEvent {
    /// Create a new tool access event.
    pub fn tool_access(user: &str, tool_name: &str, outcome: AuditOutcome) -> Self {
        Self {
            timestamp: Utc::now(),
            user: user.to_string(),
            session_id: None,
            event_type: AuditEventType::ToolAccess,
            resource: tool_name.to_string(),
            outcome,
            metadata: None,
        }
    }

    /// Create a new agent access event.
    pub fn agent_access(user: &str, agent_name: &str, outcome: AuditOutcome) -> Self {
        Self {
            timestamp: Utc::now(),
            user: user.to_string(),
            session_id: None,
            event_type: AuditEventType::AgentAccess,
            resource: agent_name.to_string(),
            outcome,
            metadata: None,
        }
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
}

/// Trait for audit sinks.
#[async_trait::async_trait]
pub trait AuditSink: Send + Sync {
    /// Log an audit event.
    async fn log(&self, event: AuditEvent) -> Result<(), crate::AuthError>;
}

/// File-based audit sink that writes JSONL.
pub struct FileAuditSink {
    writer: Mutex<BufWriter<File>>,
    path: PathBuf,
}

impl FileAuditSink {
    /// Create a new file audit sink.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let path = path.into();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let writer = Mutex::new(BufWriter::new(file));
        Ok(Self { writer, path })
    }

    /// Get the path to the audit log file.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

#[async_trait::async_trait]
impl AuditSink for FileAuditSink {
    async fn log(&self, event: AuditEvent) -> Result<(), crate::AuthError> {
        let line = serde_json::to_string(&event)
            .map_err(|e| crate::AuthError::AuditError(e.to_string()))?;

        let mut writer = self.writer.lock().unwrap_or_else(|poisoned| {
            tracing::warn!(path = %self.path.display(), "audit writer lock poisoned, recovering");
            poisoned.into_inner()
        });
        writeln!(writer, "{}", line)?;
        writer.flush()?;

        Ok(())
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
}
