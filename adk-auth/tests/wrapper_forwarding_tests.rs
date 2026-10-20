//! The RBAC and scope wrappers keep the wrapped tool's identity (spec 3.1 §10 F2).
//!
//! A wrapped MCP tool must report the same annotations, origin, effect, and declaration
//! digest as the bare tool, so its hints reach policy and an approval recorded against
//! the bare tool still matches the wrapped one.

use std::sync::{Arc, LazyLock};

use adk_auth::{
    AccessControl, AuthMiddleware, ContextScopeResolver, Permission, ProtectedTool, Role,
    ScopeGuard,
};
use adk_core::{Tool, ToolContext, ToolEffect, ToolOrigin};
use async_trait::async_trait;
use serde_json::{Value, json};

static HINTS: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "title": "Delete file",
        "readOnlyHint": false,
        "destructiveHint": true,
        "idempotentHint": true,
        "openWorldHint": false,
    })
});

/// Shaped like `adk_tool::McpTool`: hints from the server, an `mcp:{toolset}` origin, and
/// an effect derived from the hints.
struct McpLikeTool;

#[async_trait]
impl Tool for McpLikeTool {
    fn name(&self) -> &str {
        "delete_file"
    }

    fn description(&self) -> &str {
        "Deletes a file in the workspace"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        }))
    }

    fn annotations(&self) -> Option<&Value> {
        Some(&HINTS)
    }

    fn origin(&self) -> &str {
        "mcp:filesystem"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(json!({ "deleted": true }))
    }
}

fn access_control() -> Arc<AccessControl> {
    Arc::new(
        AccessControl::builder()
            .role(Role::new("operator").allow(Permission::Tool("delete_file".into())))
            .build()
            .unwrap(),
    )
}

/// Everything a policy or an approval reads from a tool's identity.
fn identity(tool: &dyn Tool) -> (Option<Value>, String, ToolEffect, ToolOrigin) {
    (tool.annotations().cloned(), tool.origin().to_string(), tool.effect(), ToolOrigin::of(tool))
}

#[test]
fn every_auth_wrapper_keeps_the_wrapped_tools_identity() {
    let bare = identity(&McpLikeTool);
    assert_eq!(bare.0.as_ref(), Some(&*HINTS));
    assert_eq!(bare.3.source, "mcp:filesystem");

    let wrapped: Vec<(&str, Arc<dyn Tool>)> = vec![
        ("ProtectedTool", Arc::new(ProtectedTool::new(McpLikeTool, access_control()))),
        (
            "ProtectedToolDyn",
            AuthMiddleware::new(AccessControl::builder().build().unwrap())
                .protect_all(vec![Arc::new(McpLikeTool)])
                .remove(0),
        ),
        ("ScopedTool", Arc::new(ScopeGuard::new(ContextScopeResolver).protect(McpLikeTool))),
        (
            "ScopedToolDyn",
            ScopeGuard::new(ContextScopeResolver)
                .protect_all(vec![Arc::new(McpLikeTool)])
                .remove(0),
        ),
    ];

    for (wrapper, tool) in wrapped {
        assert_eq!(identity(tool.as_ref()), bare, "{wrapper} changes the tool's identity");
    }
}

#[test]
fn a_wrapper_over_a_tool_without_hints_reports_none() {
    struct Plain;

    #[async_trait]
    impl Tool for Plain {
        fn name(&self) -> &str {
            "plain"
        }
        fn description(&self) -> &str {
            "No hints"
        }
        async fn execute(
            &self,
            _ctx: Arc<dyn ToolContext>,
            _args: Value,
        ) -> adk_core::Result<Value> {
            Ok(Value::Null)
        }
    }

    let protected = ProtectedTool::new(Plain, access_control());
    assert_eq!(protected.annotations(), None);
    assert_eq!(ToolOrigin::of(&protected), ToolOrigin::of(&Plain));
}
