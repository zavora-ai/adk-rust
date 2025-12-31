//! Middleware for integrating access control with adk-core.
//!
//! This module provides a `ProtectedTool` wrapper that enforces permissions
//! before tool execution.

use crate::{AccessControl, Permission};
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// A tool wrapper that enforces access control.
///
/// Wraps any tool and checks permissions before execution.
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::{AccessControl, ProtectedTool, Permission, Role};
/// use std::sync::Arc;
///
/// let ac = AccessControl::builder()
///     .role(Role::new("user").allow(Permission::Tool("search".into())))
///     .build()?;
///
/// let protected_search = ProtectedTool::new(search_tool, ac);
/// ```
pub struct ProtectedTool<T: Tool> {
    inner: T,
    access_control: Arc<AccessControl>,
}

impl<T: Tool> ProtectedTool<T> {
    /// Create a new protected tool.
    pub fn new(tool: T, access_control: Arc<AccessControl>) -> Self {
        Self {
            inner: tool,
            access_control,
        }
    }
}

#[async_trait]
impl<T: Tool + Send + Sync> Tool for ProtectedTool<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let user_id = ctx.user_id();
        let permission = Permission::Tool(self.name().to_string());

        // Check permission
        self.access_control
            .check(user_id, &permission)
            .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;

        // Execute the tool
        self.inner.execute(ctx, args).await
    }
}

/// Extension trait for easily wrapping tools with access control.
pub trait ToolExt: Tool + Sized {
    /// Wrap this tool with access control.
    fn with_access_control(self, ac: Arc<AccessControl>) -> ProtectedTool<Self> {
        ProtectedTool::new(self, ac)
    }
}

impl<T: Tool> ToolExt for T {}

/// A collection of auth utilities for integrating with ADK.
pub struct AuthMiddleware {
    access_control: Arc<AccessControl>,
}

impl AuthMiddleware {
    /// Create a new auth middleware.
    pub fn new(access_control: AccessControl) -> Self {
        Self {
            access_control: Arc::new(access_control),
        }
    }

    /// Get a reference to the access control.
    pub fn access_control(&self) -> &AccessControl {
        &self.access_control
    }

    /// Wrap a tool with access control.
    pub fn protect<T: Tool>(&self, tool: T) -> ProtectedTool<T> {
        ProtectedTool::new(tool, self.access_control.clone())
    }

    /// Wrap multiple tools with access control.
    pub fn protect_all(
        &self,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Vec<Arc<dyn Tool>> {
        tools
            .into_iter()
            .map(|t| Arc::new(ProtectedToolDyn::new(t, self.access_control.clone())) as Arc<dyn Tool>)
            .collect()
    }
}

/// Dynamic version of ProtectedTool for Arc<dyn Tool>.
pub struct ProtectedToolDyn {
    inner: Arc<dyn Tool>,
    access_control: Arc<AccessControl>,
}

impl ProtectedToolDyn {
    /// Create a new protected dynamic tool.
    pub fn new(tool: Arc<dyn Tool>, access_control: Arc<AccessControl>) -> Self {
        Self {
            inner: tool,
            access_control,
        }
    }
}

#[async_trait]
impl Tool for ProtectedToolDyn {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let user_id = ctx.user_id();
        let permission = Permission::Tool(self.name().to_string());

        // Check permission
        self.access_control
            .check(user_id, &permission)
            .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;

        // Execute the tool
        self.inner.execute(ctx, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role;

    // Mock tool for testing
    struct MockTool {
        name: String,
    }

    impl MockTool {
        fn new(name: &str) -> Self {
            Self { name: name.to_string() }
        }
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Mock tool"
        }

        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            Ok(serde_json::json!({"result": "success"}))
        }
    }

    #[test]
    fn test_tool_ext() {
        let ac = AccessControl::builder()
            .role(Role::new("user").allow(Permission::Tool("mock".into())))
            .build()
            .unwrap();

        let tool = MockTool::new("mock");
        let protected = tool.with_access_control(Arc::new(ac));
        
        assert_eq!(protected.name(), "mock");
        assert_eq!(protected.description(), "Mock tool");
    }
}
