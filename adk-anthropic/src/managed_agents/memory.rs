//! Memory store management for Managed Agents.
//!
//! Memory stores give agents persistent memory that survives across sessions.
//! Each store is a collection of text documents mounted in the session sandbox
//! at `/mnt/memory/`. The agent reads and writes them with standard file tools.
//!
//! See: <https://platform.claude.com/docs/en/managed-agents/memory>

use serde::{Deserialize, Serialize};

// ─── Memory Store Types ──────────────────────────────────────────────────────

/// A memory store containing persistent agent memories.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryStore {
    /// Unique identifier (e.g., `"memstore_01ABC..."`).
    pub id: String,
    /// Human-readable name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description passed to the agent explaining what the store contains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// ISO 8601 timestamp of creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// ISO 8601 timestamp of last update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// ISO 8601 timestamp of archival (null if active).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Additional fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Parameters for creating a memory store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateMemoryStoreParams {
    /// Human-readable name.
    pub name: String,
    /// Description shown to the agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ─── Memory Types ────────────────────────────────────────────────────────────

/// A memory (document) within a memory store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Memory {
    /// Unique identifier (e.g., `"mem_01ABC..."`).
    pub id: String,
    /// Path within the store (e.g., `/preferences/formatting.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The memory content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// SHA-256 hash of the content (for optimistic concurrency).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    /// ISO 8601 timestamp of creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// ISO 8601 timestamp of last update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Additional fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Parameters for creating a memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateMemoryParams {
    /// Path within the store (e.g., `/preferences/formatting.md`).
    pub path: String,
    /// The memory content (max 100 kB).
    pub content: String,
}

/// Parameters for updating a memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpdateMemoryParams {
    /// New content (optional — omit to keep current content).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// New path (optional — omit to keep current path, acts as rename).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Optimistic concurrency: only apply if content hash matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precondition: Option<MemoryPrecondition>,
}

/// Precondition for safe memory updates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryPrecondition {
    /// Must be `"content_sha256"`.
    #[serde(rename = "type")]
    pub precondition_type: String,
    /// The expected SHA-256 hash of the current content.
    pub content_sha256: String,
}

impl MemoryPrecondition {
    /// Create a SHA-256 content precondition.
    pub fn sha256(hash: impl Into<String>) -> Self {
        Self { precondition_type: "content_sha256".to_string(), content_sha256: hash.into() }
    }
}

// ─── Memory Version Types ────────────────────────────────────────────────────

/// An immutable version of a memory (audit trail).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryVersion {
    /// Version identifier (e.g., `"memver_01ABC..."`).
    pub id: String,
    /// The memory this version belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_id: Option<String>,
    /// The operation that created this version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// Content at this version (present on retrieve, absent on list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// ISO 8601 timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Whether this version has been redacted.
    #[serde(default)]
    pub redacted: bool,
    /// Additional fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ─── Session Resource for Memory Store ───────────────────────────────────────

/// A memory store resource to attach to a session.
///
/// Used in `CreateSessionParams.resources` alongside file resources.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryStoreResource {
    /// Must be `"memory_store"`.
    #[serde(rename = "type")]
    pub resource_type: String,
    /// The memory store ID.
    pub memory_store_id: String,
    /// Access mode: `"read_write"` (default) or `"read_only"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    /// Session-specific instructions for how the agent should use this store.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl MemoryStoreResource {
    /// Create a read-write memory store resource.
    pub fn read_write(memory_store_id: impl Into<String>) -> Self {
        Self {
            resource_type: "memory_store".to_string(),
            memory_store_id: memory_store_id.into(),
            access: Some("read_write".to_string()),
            instructions: None,
        }
    }

    /// Create a read-only memory store resource.
    pub fn read_only(memory_store_id: impl Into<String>) -> Self {
        Self {
            resource_type: "memory_store".to_string(),
            memory_store_id: memory_store_id.into(),
            access: Some("read_only".to_string()),
            instructions: None,
        }
    }

    /// Add instructions for how the agent should use this store.
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }
}

/// List response wrapper.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct MemoryListResponse<T> {
    pub data: Vec<T>,
}
