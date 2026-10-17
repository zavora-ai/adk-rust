//! # Tool Bridge Adapter
//!
//! Bridges `adk_core::Tool` implementations to the `ToolHandler` trait used by
//! `RealtimeRunner`. Also provides the `ToolContextFactory` trait for creating
//! per-invocation `ToolContext` instances scoped to the current session.

use std::sync::Arc;

use adk_core::{Tool, ToolContext};
use async_trait::async_trait;
use serde_json::Value;

use crate::config::ToolDefinition;
use crate::error::Result;
use crate::events::ToolCall;
use crate::runner::ToolHandler;

use super::context::ToolContextFactory;

/// Wraps an `Arc<dyn adk_core::Tool>` and implements [`ToolHandler`] for use
/// with the `RealtimeRunner`.
///
/// This adapter bridges the ADK tool interface to the realtime runner's
/// tool execution interface, handling context creation and error serialization. A tool error
/// is answered with [`tool_error_response`](adk_core::tool_error_response), which names the
/// error's component, category, and code and keeps its message out unless
/// [`with_exposed_error_messages`](Self::with_exposed_error_messages) is set.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_realtime::integration::tool_bridge::ToolBridgeAdapter;
///
/// let adapter = ToolBridgeAdapter::new(tool, context_factory);
/// // Register with RealtimeRunner as a ToolHandler
/// ```
pub struct ToolBridgeAdapter {
    tool: Arc<dyn Tool>,
    context_factory: Arc<dyn ToolContextFactory>,
    expose_error_messages: bool,
}

impl ToolBridgeAdapter {
    /// Creates a new `ToolBridgeAdapter` wrapping the given tool and context factory.
    ///
    /// # Arguments
    ///
    /// * `tool` - The ADK tool to bridge.
    /// * `context_factory` - Factory for creating per-invocation `ToolContext` instances.
    pub fn new(tool: Arc<dyn Tool>, context_factory: Arc<dyn ToolContextFactory>) -> Self {
        Self { tool, context_factory, expose_error_messages: false }
    }

    /// Sends the full message of the tool's errors to the model, which by default sees only
    /// their component, category, and code.
    pub fn with_exposed_error_messages(mut self, expose: bool) -> Self {
        self.expose_error_messages = expose;
        self
    }

    /// Extract a [`ToolDefinition`] from an ADK [`Tool`] for registering with the
    /// realtime provider.
    ///
    /// Maps the tool's `name()`, `description()`, and `parameters_schema()` to the
    /// realtime `ToolDefinition` format.
    ///
    /// # Arguments
    ///
    /// * `tool` - The ADK tool to extract the definition from.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let def = ToolBridgeAdapter::definition(my_tool.as_ref());
    /// assert_eq!(def.name, "my_tool");
    /// ```
    pub fn definition(tool: &dyn Tool) -> ToolDefinition {
        ToolDefinition {
            name: tool.name().to_string(),
            description: Some(tool.description().to_string()),
            parameters: tool.parameters_schema(),
        }
    }
}

#[async_trait]
impl ToolHandler for ToolBridgeAdapter {
    async fn execute(&self, call: &ToolCall) -> Result<Value> {
        let ctx: Arc<dyn ToolContext> = self.context_factory.create_context(&call.call_id);
        match self.tool.execute(ctx, call.arguments.clone()).await {
            Ok(value) => Ok(value),
            Err(e) => {
                Ok(adk_core::tool_error_response(self.tool.name(), &e, self.expose_error_messages))
            }
        }
    }
}
