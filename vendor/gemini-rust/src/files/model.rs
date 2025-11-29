use reqwest::Url;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::common::serde::*;

/// Represents a file resource in the Gemini API.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    /// The unique identifier for the file.
    pub name: String,
    /// The URI of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<Url>,
    /// The download URI of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_uri: Option<Url>,
    /// The display name of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The MIME type of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The size of the file in bytes.
    #[serde(default, with = "i64_as_string::optional")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    /// The creation time of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub create_time: Option<OffsetDateTime>,
    /// The expiration time of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expiration_time: Option<OffsetDateTime>,
    /// The last update time of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub update_time: Option<OffsetDateTime>,
    /// The SHA-256 hash of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256_hash: Option<String>,
    /// The current state of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<FileState>,
}

/// The state of a file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileState {
    /// File state is unspecified
    StateUnspecified,
    /// File is being processed
    Processing,
    /// File is active and ready for use
    Active,
    /// File processing failed
    Failed,
    /// File has been deleted
    Deleted,
}

/// Response from the Gemini API for listing files.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFilesResponse {
    /// A list of files.
    #[serde(default)]
    pub files: Vec<File>,
    /// A token to retrieve the next page of results.
    pub next_page_token: Option<String>,
}
