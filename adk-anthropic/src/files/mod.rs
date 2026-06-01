//! Anthropic Files API client.
//!
//! Upload, download, list, and manage files for use with the Claude API.
//! Files are uploaded once and referenced by `file_id` in Messages requests,
//! avoiding repeated uploads of the same content.
//!
//! # Beta Header
//!
//! All Files API requests include `anthropic-beta: files-api-2025-04-14`.
//!
//! # Capabilities
//!
//! - **Upload**: Upload files (PDF, images, text, datasets) up to 500 MB
//! - **Download**: Download files created by skills or code execution
//! - **List**: Paginated listing of uploaded files
//! - **Get metadata**: Retrieve file info without downloading content
//! - **Delete**: Permanently remove files
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_anthropic::files::FilesClient;
//!
//! let client = FilesClient::from_env()?;
//!
//! // Upload a file
//! let file = client.upload_file("report.pdf", pdf_bytes).await?;
//! println!("Uploaded: {} ({})", file.id, file.filename.unwrap_or_default());
//!
//! // Use file_id in Messages API requests
//! // ...
//!
//! // Download a file (only works for files created by code execution/skills)
//! let content = client.download_file(&file.id).await?;
//!
//! // Delete when done
//! client.delete_file(&file.id).await?;
//! ```
//!
//! See: <https://docs.anthropic.com/en/docs/build-with-claude/files>

mod client;
mod types;

pub use client::*;
pub use types::*;
