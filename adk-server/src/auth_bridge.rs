//! Auth middleware bridge for flowing authenticated identity into agent execution.
//!
//! This module defines the [`RequestContextExtractor`] trait that server operators
//! implement to extract identity from HTTP requests, and the [`RequestContextError`]
//! enum for extraction failures.
//!
//! The extracted [`RequestContext`] (re-exported from `adk-core`) carries user_id,
//! scopes, and metadata into the `InvocationContext`, making scopes available
//! to tools via `ToolContext::user_scopes()`.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_server::auth_bridge::{RequestContextExtractor, RequestContextError};
//! use adk_core::RequestContext;
//! use async_trait::async_trait;
//!
//! struct MyExtractor;
//!
//! #[async_trait]
//! impl RequestContextExtractor for MyExtractor {
//!     async fn extract(
//!         &self,
//!         parts: &axum::http::request::Parts,
//!     ) -> Result<RequestContext, RequestContextError> {
//!         let auth = parts.headers
//!             .get("authorization")
//!             .and_then(|v| v.to_str().ok())
//!             .ok_or(RequestContextError::MissingAuth)?;
//!         // ... validate token, build RequestContext ...
//!         # todo!()
//!     }
//! }
//! ```

pub use adk_core::RequestContext;
use async_trait::async_trait;

/// Extracts authenticated identity from HTTP request headers.
///
/// Implementations typically parse a Bearer token from the `Authorization`
/// header, validate it, and map claims to a [`RequestContext`].
#[async_trait]
pub trait RequestContextExtractor: Send + Sync {
    /// Extract identity from the request parts (headers, URI, etc.).
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError>;
}

/// Errors that can occur during request context extraction.
#[derive(Debug, thiserror::Error)]
pub enum RequestContextError {
    /// The `Authorization` header is missing from the request.
    #[error("missing authorization header")]
    MissingAuth,
    /// The token was present but failed validation.
    #[error("invalid token: {0}")]
    InvalidToken(String),
    /// An internal error occurred during extraction.
    #[error("extraction failed: {0}")]
    ExtractionFailed(String),
}
