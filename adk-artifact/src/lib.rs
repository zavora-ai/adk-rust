//! # adk-artifact
//!
//! Binary artifact storage for ADK agents.
//!
//! ## Overview
//!
//! This crate provides versioned artifact storage for binary data:
//!
//! - [`InMemoryArtifactService`] - Simple in-memory storage
//! - [`FileArtifactService`] - Local filesystem storage with crash-safe writes
//! - `GcsArtifactService` - Google Cloud Storage backend (feature `gcs`),
//!   blob-name compatible with adk-python
//! - [`ArtifactService`] - Trait for custom backends
//! - [`ScopedArtifacts`] - Session-scoped artifact access
//!
//! Versions are immutable: a save naming an existing version fails with
//! `ErrorCategory::Conflict`, and the session id [`USER_SCOPE_SESSION_ID`] is reserved for
//! `user:` artifacts.
//!
//! ## Quick Start
//!
//! ```rust
//! use adk_artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
//! use adk_core::Part;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_core::Result<()> {
//! let service = InMemoryArtifactService::new();
//! let saved = service
//!     .save(SaveRequest {
//!         app_name: "my_app".into(),
//!         user_id: "user".into(),
//!         session_id: "session".into(),
//!         file_name: "notes.txt".into(),
//!         part: Part::text_part("hello"),
//!         version: None,
//!     })
//!     .await?;
//! assert_eq!(saved.version, 1);
//! # Ok(())
//! # }
//! ```
//!
//! ## Use Cases
//!
//! - Store generated images, PDFs, audio
//! - Cache intermediate results
//! - Share binary data between agent turns
#![allow(clippy::result_large_err)]

pub mod file;
#[cfg(feature = "gcs")]
pub mod gcs;
pub mod inmemory;
pub mod scoped;
pub mod service;
pub mod subject;

pub use file::FileArtifactService;
#[cfg(feature = "gcs")]
pub use gcs::GcsArtifactService;
pub use inmemory::InMemoryArtifactService;
pub use scoped::ScopedArtifacts;
pub use service::{
    ArtifactService, DeleteRequest, ListRequest, ListResponse, LoadRequest, LoadResponse,
    SaveRequest, SaveResponse, USER_SCOPE_SESSION_ID, UserArtifact, VersionsRequest,
    VersionsResponse,
};
pub use subject::ArtifactSubjectData;
