//! Types for the Anthropic Files API.

use serde::{Deserialize, Serialize};

/// A file stored in Anthropic's file storage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileObject {
    /// Unique file identifier (e.g., `"file_011CNha8iCJcU1wXNR6q4V8w"`).
    pub id: String,
    /// The filename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// MIME type of the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// File size in bytes.
    #[serde(default)]
    pub size_bytes: u64,
    /// Whether the file can be downloaded (only true for files created by code execution/skills).
    #[serde(default)]
    pub downloadable: bool,
    /// ISO 8601 timestamp of creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Additional fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Response from listing files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct FileListResponse {
    pub data: Vec<FileObject>,
    #[serde(default)]
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_id: Option<String>,
}

/// Response from deleting a file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct FileDeleteResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub object_type: String,
    #[serde(default)]
    pub deleted: bool,
}
