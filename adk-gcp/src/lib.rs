//! Shared Google Cloud REST plumbing for ADK-Rust Vertex AI backends.
//!
//! Every Vertex integration in the workspace — sessions, memory, example
//! store, artifact storage, deployment — needs the same three pieces:
//!
//! - **[`GcpHttpClient`]** — Application Default Credentials with cached
//!   auth headers, a redirect-disabled HTTP client with connect/request
//!   timeouts, HTTPS-or-loopback endpoint validation, and bounded JSON
//!   response reads.
//! - **[`LroPoller`]** — `google.longrunning.Operation` polling with capped
//!   exponential backoff, operation identity pinning, and project/location
//!   scope validation.
//! - **[`VertexResourceName`]** — parsing and formatting of
//!   `projects/*/locations/*/reasoningEngines/*` resource names.
//!
//! Errors cross the [`adk_core::AdkError`] boundary carrying each
//! consumer's own identity: a [`GcpErrorContext`] bundles the
//! [`ErrorComponent`](adk_core::ErrorComponent), a
//! [`GcpErrorCodes`] table of `&'static str` codes, the human-readable
//! subject, and the provider tag, so errors from the shared plumbing are
//! indistinguishable from the ones each backend previously built for
//! itself.
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_core::ErrorComponent;
//! use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient, LroPoller};
//! use serde_json::json;
//!
//! const CODES: GcpErrorCodes = GcpErrorCodes {
//!     invalid_input: "memory.vertex.invalid_input",
//!     unauthorized: "memory.vertex.unauthorized",
//!     forbidden: "memory.vertex.forbidden",
//!     not_found: "memory.vertex.not_found",
//!     rate_limited: "memory.vertex.rate_limited",
//!     timeout: "memory.vertex.timeout",
//!     unavailable: "memory.vertex.unavailable",
//!     credentials_unavailable: "memory.vertex.credentials_unavailable",
//!     invalid_response: "memory.vertex.invalid_response",
//!     invalid_request: "memory.vertex.invalid_request",
//!     upstream_error: "memory.vertex.upstream_error",
//!     operation_failed: "memory.vertex.operation_failed",
//! };
//!
//! # async fn generate() -> adk_core::Result<()> {
//! let client = GcpHttpClient::builder(
//!     GcpErrorContext::new(ErrorComponent::Memory, CODES, "vertex memory"),
//!     "https://us-central1-aiplatform.googleapis.com",
//! )
//! .build()?;
//!
//! let parent = "projects/my-project/locations/us-central1/reasoningEngines/4242";
//! let request = client
//!     .request(reqwest::Method::POST, &format!("{parent}/memories:generate"))
//!     .await?
//!     .json(&json!({ "directContentsSource": { "events": [] } }));
//! let operation = client.send_value(request).await?;
//!
//! LroPoller::new()
//!     .wait_for_operation(&client, operation, "memories generate", false, "my-project", "us-central1")
//!     .await?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod client;
mod error;
mod lro;
mod resource;

pub use client::{GcpHttpClient, GcpHttpClientBuilder};
pub use error::{GcpErrorCodes, GcpErrorContext, truncate_for_error};
pub use lro::{LroPoller, Operation, OperationError};
pub use resource::{VertexResourceName, is_canonical_reasoning_engine_id, is_scoped_resource_name};
