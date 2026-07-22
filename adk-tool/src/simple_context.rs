//! Lightweight [`ToolContext`] implementation for use outside the agent loop.
//!
//! [`SimpleToolContext`] provides sensible defaults for all trait methods so
//! that callers in MCP server mode, testing, or sub-agent delegation can
//! invoke tools without constructing a full invocation context.
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_tool::SimpleToolContext;
//! use std::sync::Arc;
//!
//! let ctx = SimpleToolContext::new("my-caller");
//! let ctx: Arc<dyn adk_core::ToolContext> = Arc::new(ctx);
//! ```

use adk_core::context::{Artifacts, CallbackContext, MemoryEntry, ReadonlyContext};
use adk_core::types::Content;
use adk_core::{EventActions, Result, ToolContext};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

/// A lightweight [`ToolContext`] with sensible defaults for non-agent callers.
///
/// Implements [`ReadonlyContext`], [`CallbackContext`], and [`ToolContext`]
/// with minimal configuration. Construct via [`SimpleToolContext::new`] with
/// a caller name; all other fields use safe defaults.
pub struct SimpleToolContext {
    caller_name: String,
    session_id: String,
    invocation_id: String,
    function_call_id: String,
    user_content: Content,
    actions: Mutex<EventActions>,
}

impl SimpleToolContext {
    /// Create a new context with the given caller name.
    ///
    /// Generates unique UUIDs for `invocation_id` and `function_call_id`.
    /// The caller name is returned by both [`agent_name()`](ReadonlyContext::agent_name)
    /// and [`app_name()`](ReadonlyContext::app_name).
    pub fn new(caller_name: impl Into<String>) -> Self {
        Self {
            caller_name: caller_name.into(),
            session_id: String::new(),
            invocation_id: uuid::Uuid::new_v4().to_string(),
            function_call_id: uuid::Uuid::new_v4().to_string(),
            user_content: Content::new("user"),
            actions: Mutex::new(EventActions::default()),
        }
    }

    /// Override the default function call ID.
    ///
    /// By default a UUID is generated at construction. Use this builder
    /// method to provide a specific ID instead.
    pub fn with_function_call_id(mut self, id: impl Into<String>) -> Self {
        self.function_call_id = id.into();
        self
    }

    /// Attach the session that owns this tool call.
    ///
    /// This is useful for desktop shells, test harnesses, and other callers
    /// that execute tools outside the full agent loop but still need
    /// session-aware callbacks such as MCP elicitation.
    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = id.into();
        self
    }
}

#[async_trait]
impl ReadonlyContext for SimpleToolContext {
    fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    fn agent_name(&self) -> &str {
        &self.caller_name
    }

    fn user_id(&self) -> &str {
        "anonymous"
    }

    fn app_name(&self) -> &str {
        &self.caller_name
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn branch(&self) -> &str {
        ""
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for SimpleToolContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for SimpleToolContext {
    fn function_call_id(&self) -> &str {
        &self.function_call_id
    }

    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap_or_else(|e| e.into_inner()) = actions;
    }

    async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_empty_by_default() {
        let context = SimpleToolContext::new("test");
        assert_eq!(ReadonlyContext::session_id(&context), "");
    }

    #[test]
    fn session_id_can_be_attached_for_out_of_loop_tool_calls() {
        let context = SimpleToolContext::new("desktop").with_session_id("session-123");
        assert_eq!(ReadonlyContext::session_id(&context), "session-123");
    }
}
