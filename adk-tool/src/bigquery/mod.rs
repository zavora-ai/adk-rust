//! Native BigQuery toolset for ADK agents.
//!
//! Provides tools for executing SQL queries, retrieving table schemas,
//! listing datasets, and listing tables via the Google BigQuery API.
//!
//! Enable with the `bigquery` feature flag.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_tool::bigquery::BigQueryToolset;
//!
//! // Application Default Credentials
//! let toolset = BigQueryToolset::new();
//!
//! // With explicit project ID
//! let toolset = BigQueryToolset::with_project("my-gcp-project");
//!
//! // Via secret provider
//! let toolset = BigQueryToolset::from_secret("bigquery-credentials");
//! ```

mod tools;
mod toolset;

pub use toolset::{BigQueryToolset, CredentialSource};
