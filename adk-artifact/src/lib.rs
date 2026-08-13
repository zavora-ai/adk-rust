//! # adk-artifact
#![allow(clippy::result_large_err)]
//!
//! Binary artifact storage for ADK agents.
//!
//! ## Overview
//!
//! This crate provides artifact storage for binary data:
//!
//! - [`InMemoryArtifactService`] - Simple in-memory storage
//! - [`ArtifactService`] - Trait for custom backends
//! - [`ScopedArtifacts`] - Session-scoped artifact access
//! - `GcsArtifactService` - Google Cloud Storage backend (feature `gcs`),
//!   blob-name compatible with adk-python
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_artifact::InMemoryArtifactService;
//!
//! let service = InMemoryArtifactService::new();
//!
//! // Artifacts are stored with app/user/session scope
//! // Supports versioning and MIME type detection
//! ```
//!
//! ## Use Cases
//!
//! - Store generated images, PDFs, audio
//! - Cache intermediate results
//! - Share binary data between agent turns

pub mod file;
#[cfg(feature = "gcs")]
pub mod gcs;
pub mod inmemory;
pub mod scoped;
pub mod service;

pub use file::FileArtifactService;
#[cfg(feature = "gcs")]
pub use gcs::GcsArtifactService;
pub use inmemory::InMemoryArtifactService;
pub use scoped::ScopedArtifacts;
pub use service::{
    ArtifactService, DeleteRequest, ListRequest, ListResponse, LoadRequest, LoadResponse,
    SaveRequest, SaveResponse, VersionsRequest, VersionsResponse,
};
