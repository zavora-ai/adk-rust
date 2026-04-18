//! Native Spanner toolset for ADK agents.
//!
//! Provides tools for executing SQL queries, retrieving table schemas,
//! and listing tables via the Google Cloud Spanner API.
//!
//! Enable with the `spanner` feature flag.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_tool::spanner::SpannerToolset;
//!
//! // Application Default Credentials
//! let toolset = SpannerToolset::new("my-project", "my-instance", "my-database");
//!
//! // Via secret provider
//! let toolset = SpannerToolset::from_secret(
//!     "my-project",
//!     "my-instance",
//!     "my-database",
//!     "spanner-credentials",
//! );
//! ```

mod tools;
mod toolset;

pub use toolset::{CredentialSource, SpannerToolset};
