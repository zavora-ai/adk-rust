//! Toolset trait implementation for McpServerManager.
//!
//! This module contains the [`Toolset`](adk_core::Toolset) trait implementation
//! for [`McpServerManager`], tool name collision resolution, and the
//! [`PrefixedTool`] wrapper that delegates all [`Tool`](adk_core::Tool) methods
//! while overriding the tool name.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;
use serde_json::Value;

use super::manager::McpServerManager;
use super::status::ServerStatus;

/// Per-server tool list: each entry is `(tool_name, tool_arc)`.
type ServerToolMap = HashMap<String, Vec<(String, Arc<dyn Tool>)>>;

/// A wrapper around an `Arc<dyn Tool>` that overrides the tool name with a
/// prefixed version to resolve name collisions across multiple MCP servers.
///
/// All [`Tool`] trait methods delegate to the inner tool, except `name()` and
/// `declaration()` which use the prefixed name.
struct PrefixedTool {
    /// The original tool being wrapped.
    inner: Arc<dyn Tool>,
    /// The prefixed name in the format `{server_id}__{tool_name}`.
    prefixed_name: String,
}

#[async_trait]
impl Tool for PrefixedTool {
    fn name(&self) -> &str {
        &self.prefixed_name
    }

    fn description(&self) -> &str {
        self.inner.description()
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

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }

    fn is_builtin(&self) -> bool {
        self.inner.is_builtin()
    }

    fn declaration(&self) -> Value {
        let mut decl = self.inner.declaration();
        if let Some(obj) = decl.as_object_mut() {
            obj.insert("name".to_string(), Value::String(self.prefixed_name.clone()));
        }
        decl
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    async fn execute(&self, ctx: Arc<dyn adk_core::ToolContext>, args: Value) -> Result<Value> {
        self.inner.execute(ctx, args).await
    }
}

/// Resolve tool name collisions across multiple servers.
///
/// For tool names that appear in two or more servers, the tool is wrapped in a
/// [`PrefixedTool`] with the format `{server_id}__{tool_name}`. Tools with
/// unique names across all servers retain their original names.
fn resolve_tool_names(server_tools: &ServerToolMap) -> Vec<Arc<dyn Tool>> {
    // Step 1: Count occurrences of each tool name across all servers
    let mut name_counts: HashMap<&str, Vec<&str>> = HashMap::new();
    for (server_id, tools) in server_tools {
        for (name, _) in tools {
            name_counts.entry(name).or_default().push(server_id);
        }
    }

    // Step 2: For names appearing in multiple servers, prefix with server_id
    let mut result = Vec::new();
    for (server_id, tools) in server_tools {
        for (name, tool) in tools {
            if name_counts[name.as_str()].len() > 1 {
                result.push(Arc::new(PrefixedTool {
                    inner: tool.clone(),
                    prefixed_name: format!("{server_id}__{name}"),
                }) as Arc<dyn Tool>);
            } else {
                result.push(tool.clone());
            }
        }
    }
    result
}

#[async_trait]
impl Toolset for McpServerManager {
    fn name(&self) -> &str {
        &self.name
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        // Acquire read lock to iterate over servers
        let servers = self.servers.read().await;

        // Collect tools from each Running server
        let mut server_tools: ServerToolMap = HashMap::new();

        for (server_id, entry) in servers.iter() {
            if entry.status != ServerStatus::Running {
                continue;
            }

            let toolset = match &entry.toolset {
                Some(ts) => ts,
                None => continue,
            };

            match toolset.tools(ctx.clone()).await {
                Ok(tools) => {
                    let named_tools: Vec<(String, Arc<dyn Tool>)> =
                        tools.into_iter().map(|t| (t.name().to_string(), t)).collect();
                    server_tools.insert(server_id.clone(), named_tools);
                }
                Err(e) => {
                    tracing::warn!(
                        server.id = server_id,
                        error = %e,
                        "failed to list tools from server, skipping"
                    );
                }
            }
        }

        Ok(resolve_tool_names(&server_tools))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::ToolContext;

    /// A minimal test tool for verifying collision resolution.
    struct FakeTool {
        name: String,
        description: String,
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            &self.description
        }

        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            Ok(Value::String("ok".to_string()))
        }
    }

    fn make_tool(name: &str) -> Arc<dyn Tool> {
        Arc::new(FakeTool { name: name.to_string(), description: format!("Tool {name}") })
    }

    #[test]
    fn test_resolve_no_collisions() {
        let mut server_tools: ServerToolMap = HashMap::new();
        server_tools
            .insert("server_a".to_string(), vec![("tool_x".to_string(), make_tool("tool_x"))]);
        server_tools
            .insert("server_b".to_string(), vec![("tool_y".to_string(), make_tool("tool_y"))]);

        let result = resolve_tool_names(&server_tools);
        assert_eq!(result.len(), 2);

        let names: Vec<&str> = result.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"tool_x"));
        assert!(names.contains(&"tool_y"));
    }

    #[test]
    fn test_resolve_with_collisions() {
        let mut server_tools: ServerToolMap = HashMap::new();
        server_tools.insert(
            "server_a".to_string(),
            vec![("read_file".to_string(), make_tool("read_file"))],
        );
        server_tools.insert(
            "server_b".to_string(),
            vec![("read_file".to_string(), make_tool("read_file"))],
        );

        let result = resolve_tool_names(&server_tools);
        assert_eq!(result.len(), 2);

        let mut names: Vec<String> = result.iter().map(|t| t.name().to_string()).collect();
        names.sort();
        assert_eq!(names, vec!["server_a__read_file", "server_b__read_file"]);
    }

    #[test]
    fn test_resolve_mixed_collision_and_unique() {
        let mut server_tools: ServerToolMap = HashMap::new();
        server_tools.insert(
            "server_a".to_string(),
            vec![
                ("read_file".to_string(), make_tool("read_file")),
                ("unique_a".to_string(), make_tool("unique_a")),
            ],
        );
        server_tools.insert(
            "server_b".to_string(),
            vec![
                ("read_file".to_string(), make_tool("read_file")),
                ("unique_b".to_string(), make_tool("unique_b")),
            ],
        );

        let result = resolve_tool_names(&server_tools);
        assert_eq!(result.len(), 4);

        let mut names: Vec<String> = result.iter().map(|t| t.name().to_string()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["server_a__read_file", "server_b__read_file", "unique_a", "unique_b",]
        );
    }

    #[test]
    fn test_resolve_empty_servers() {
        let server_tools: ServerToolMap = HashMap::new();
        let result = resolve_tool_names(&server_tools);
        assert!(result.is_empty());
    }

    #[test]
    fn test_prefixed_tool_delegates_description() {
        let inner = make_tool("original");
        let prefixed =
            PrefixedTool { inner: inner.clone(), prefixed_name: "server__original".to_string() };

        assert_eq!(prefixed.name(), "server__original");
        assert_eq!(prefixed.description(), inner.description());
        assert_eq!(prefixed.is_long_running(), inner.is_long_running());
        assert_eq!(prefixed.is_read_only(), inner.is_read_only());
        assert_eq!(prefixed.is_concurrency_safe(), inner.is_concurrency_safe());
        assert_eq!(prefixed.is_builtin(), inner.is_builtin());
    }

    #[test]
    fn test_prefixed_tool_declaration_overrides_name() {
        let inner = make_tool("original");
        let prefixed = PrefixedTool { inner, prefixed_name: "server__original".to_string() };

        let decl = prefixed.declaration();
        assert_eq!(decl["name"], "server__original");
    }

    #[test]
    fn test_resolve_three_way_collision() {
        let mut server_tools: ServerToolMap = HashMap::new();
        server_tools.insert("a".to_string(), vec![("shared".to_string(), make_tool("shared"))]);
        server_tools.insert("b".to_string(), vec![("shared".to_string(), make_tool("shared"))]);
        server_tools.insert("c".to_string(), vec![("shared".to_string(), make_tool("shared"))]);

        let result = resolve_tool_names(&server_tools);
        assert_eq!(result.len(), 3);

        let mut names: Vec<String> = result.iter().map(|t| t.name().to_string()).collect();
        names.sort();
        assert_eq!(names, vec!["a__shared", "b__shared", "c__shared"]);
    }
}
