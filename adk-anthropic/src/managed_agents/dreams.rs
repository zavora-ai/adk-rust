//! Dreams API for memory curation and consolidation.
//!
//! Dreams let Claude reflect on past sessions to curate an agent's memory store:
//! merging duplicates, resolving contradictions, and surfacing new insights.
//! The input store is never modified — a new output store is produced.
//!
//! **Research Preview**: Requires the `dreaming-2026-04-21` beta header in addition
//! to the standard `managed-agents-2026-04-01` header.
//!
//! See: <https://platform.claude.com/docs/en/managed-agents/dreams>

use serde::{Deserialize, Serialize};

// ─── Dream Types ─────────────────────────────────────────────────────────────

/// A dream job that curates memory from past sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Dream {
    /// Unique identifier (e.g., `"drm_01ABC..."`).
    pub id: String,
    /// Current status: `pending`, `running`, `completed`, `failed`, `canceled`.
    pub status: DreamStatus,
    /// The inputs to the dream (memory store + sessions).
    #[serde(default)]
    pub inputs: Vec<DreamInput>,
    /// The outputs produced (memory store, populated once running/completed).
    #[serde(default)]
    pub outputs: Vec<DreamOutput>,
    /// The model used for the dream pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<serde_json::Value>,
    /// Optional instructions guiding the dream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Session ID of the underlying pipeline session (available once running).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Token usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
    /// Error details (if failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
    /// ISO 8601 timestamp of creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// ISO 8601 timestamp of completion/failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// ISO 8601 timestamp of archival.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Additional fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Dream lifecycle status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DreamStatus {
    /// Dream created and queued.
    Pending,
    /// Pipeline is processing.
    Running,
    /// Finished successfully.
    Completed,
    /// Terminated with an error.
    Failed,
    /// Canceled by the user.
    Canceled,
}

/// An input to a dream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DreamInput {
    /// A memory store to curate.
    MemoryStore { memory_store_id: String },
    /// Past session transcripts to mine for insights.
    Sessions { session_ids: Vec<String> },
}

/// An output from a dream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DreamOutput {
    /// The output memory store produced by the dream.
    MemoryStore { memory_store_id: String },
}

/// Parameters for creating a dream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateDreamParams {
    /// Inputs: a memory store and session transcripts.
    pub inputs: Vec<DreamInput>,
    /// The model to run the dream pipeline (e.g., `"claude-opus-4-8"`).
    pub model: String,
    /// Optional instructions guiding the curation (max 4096 chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl CreateDreamParams {
    /// Create dream params from a memory store and session IDs.
    pub fn new(
        memory_store_id: impl Into<String>,
        session_ids: Vec<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            inputs: vec![
                DreamInput::MemoryStore { memory_store_id: memory_store_id.into() },
                DreamInput::Sessions { session_ids },
            ],
            model: model.into(),
            instructions: None,
        }
    }

    /// Add instructions for the dream.
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }
}

impl Dream {
    /// Get the output memory store ID (if available).
    pub fn output_store_id(&self) -> Option<&str> {
        self.outputs.iter().find(|o| matches!(o, DreamOutput::MemoryStore { .. })).map(
            |o| match o {
                DreamOutput::MemoryStore { memory_store_id } => memory_store_id.as_str(),
            },
        )
    }

    /// Check if the dream has reached a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self.status, DreamStatus::Completed | DreamStatus::Failed | DreamStatus::Canceled)
    }
}

/// List response wrapper for dreams.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct DreamListResponse {
    pub data: Vec<Dream>,
}
