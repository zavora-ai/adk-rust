//! Wire envelope for the Agent Engine dispatch protocol.
//!
//! # Casing exception
//!
//! This envelope is **snake_case**, not the workspace's usual camelCase for
//! REST payloads. The platform's container contract dispatches
//! `{"class_method": ..., "input": ...}` because adk-python resolves the
//! method with `getattr` — the field names are Python identifiers on the
//! wire (confirmed by verification task V14). Do not "fix" this to
//! camelCase; it would break every host-side caller.

use serde::Deserialize;
use serde_json::{Value, json};

/// A dispatch request as delivered by the Agent Engine host to the container.
///
/// Sent to both `POST /api/reasoning_engine` (unary) and
/// `POST /api/stream_reasoning_engine` (streaming).
///
/// # Example
///
/// ```rust
/// use adk_server::agent_engine::DispatchRequest;
///
/// let req: DispatchRequest = serde_json::from_str(
///     r#"{"class_method": "async_stream_query", "input": {"user_id": "u", "message": "hi"}}"#,
/// )
/// .unwrap();
/// assert_eq!(req.class_method, "async_stream_query");
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct DispatchRequest {
    /// The operation name, matched against [`ClassMethod`](super::ClassMethod).
    pub class_method: String,
    /// Operation arguments; each class method deserializes this into its
    /// typed input struct at the dispatch boundary.
    pub input: Option<Value>,
}

/// Wraps a unary handler result in the platform's `{"output": ...}` envelope.
pub(crate) fn unary_response(output: Value) -> Value {
    json!({ "output": output })
}
