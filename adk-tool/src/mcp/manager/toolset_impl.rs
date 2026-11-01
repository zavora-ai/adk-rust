//! Toolset trait implementation for McpServerManager.
//!
//! This module contains the [`Toolset`](adk_core::Toolset) trait implementation
//! for [`McpServerManager`], tool name collision resolution, and the
//! [`PrefixedTool`] wrapper that delegates all [`Tool`](adk_core::Tool) methods
//! while overriding the tool name.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;
use serde_json::Value;

use super::super::discovery::{MAX_TOOL_NAME_BYTES, escape_for_display};
use super::manager::McpServerManager;
use super::status::ServerStatus;

/// Per-server tool list, ordered by server ID: each entry is `(tool_name, tool_arc)`.
type ServerToolMap = BTreeMap<String, Vec<(String, Arc<dyn Tool>)>>;

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

    fn effect(&self) -> adk_core::ToolEffect {
        self.inner.effect()
    }

    fn egress(&self, args: &serde_json::Value) -> adk_core::egress::ToolEgress {
        self.inner.egress(args)
    }

    fn annotations(&self) -> Option<&Value> {
        self.inner.annotations()
    }

    fn origin(&self) -> &str {
        self.inner.origin()
    }

    fn timeout_override(&self) -> Option<Option<std::time::Duration>> {
        self.inner.timeout_override()
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
///
/// Server IDs and tool names may both contain `__`, so a final name can still
/// repeat: `a` + `b__x` and `a__b` + `x` both become `a__b__x`, and a prefixed
/// name can equal another server's own tool name. Servers are visited in ID
/// order and the first tool to claim a final name keeps it; a later one, or a
/// prefixed name longer than model providers accept, is skipped with a warning.
fn resolve_tool_names(server_tools: &ServerToolMap) -> Vec<Arc<dyn Tool>> {
    // Step 1: Count occurrences of each tool name across all servers
    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for tools in server_tools.values() {
        for (name, _) in tools {
            *name_counts.entry(name).or_default() += 1;
        }
    }

    // Step 2: For names appearing in multiple servers, prefix with server_id
    let mut claimed: HashMap<String, &str> = HashMap::new();
    let mut result = Vec::new();
    for (server_id, tools) in server_tools {
        for (name, tool) in tools {
            let final_name = if name_counts[name.as_str()] > 1 {
                format!("{server_id}__{name}")
            } else {
                name.clone()
            };
            if final_name.len() > MAX_TOOL_NAME_BYTES {
                tracing::warn!(
                    server.id = server_id,
                    tool.name = %escape_for_display(&final_name),
                    max_bytes = MAX_TOOL_NAME_BYTES,
                    "skipping MCP tool whose server-prefixed name is longer than model \
                     providers accept; shorten the server ID"
                );
                continue;
            }
            if let Some(owner) = claimed.get(&final_name) {
                tracing::warn!(
                    server.id = server_id,
                    tool.name = %escape_for_display(&final_name),
                    claimed_by = owner,
                    "skipping MCP tool whose resolved name is already taken by another \
                     server's tool; rename a server so the IDs do not share a '__' boundary"
                );
                continue;
            }
            claimed.insert(final_name.clone(), server_id);
            if final_name == *name {
                result.push(tool.clone());
            } else {
                result.push(Arc::new(PrefixedTool {
                    inner: tool.clone(),
                    prefixed_name: final_name,
                }) as Arc<dyn Tool>);
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
        // Snapshot running toolsets so slow network discovery does not hold the
        // manager map lock and block lifecycle changes.
        let running = {
            let servers = self.servers.read().await;
            servers
                .iter()
                .filter_map(|(server_id, entry)| {
                    (entry.status == ServerStatus::Running)
                        .then(|| entry.toolset.clone().map(|toolset| (server_id.clone(), toolset)))
                        .flatten()
                })
                .collect::<Vec<_>>()
        };

        // Concurrently, so a server that is slow to list costs its own discovery
        // timeout, not every other server's turn as well.
        let discovered =
            futures::future::join_all(running.into_iter().map(|(server_id, toolset)| {
                let ctx = ctx.clone();
                async move { (server_id, toolset.tools(ctx).await) }
            }))
            .await;

        let mut server_tools: ServerToolMap = BTreeMap::new();
        for (server_id, discovered) in discovered {
            match discovered {
                Ok(tools) => {
                    let named_tools: Vec<(String, Arc<dyn Tool>)> =
                        tools.into_iter().map(|t| (t.name().to_string(), t)).collect();
                    server_tools.insert(server_id, named_tools);
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

    /// Leaves every managed server running.
    ///
    /// One manager is typically shared, as an `Arc`, by several agents or runners, and the
    /// application starts its servers, so `Runner::close` on one runner does not stop
    /// servers another still uses. Call [`McpServerManager::shutdown`] to stop them; it
    /// returns once each server's process has exited or its grace period has elapsed.
    async fn close(&self) {
        tracing::debug!(
            manager.name = %self.name,
            "closing the MCP server manager's toolset leaves its servers running; call shutdown \
             to stop them"
        );
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
        let mut server_tools: ServerToolMap = BTreeMap::new();
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
        let mut server_tools: ServerToolMap = BTreeMap::new();
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
        let mut server_tools: ServerToolMap = BTreeMap::new();
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
        let server_tools: ServerToolMap = BTreeMap::new();
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
        let mut server_tools: ServerToolMap = BTreeMap::new();
        server_tools.insert("a".to_string(), vec![("shared".to_string(), make_tool("shared"))]);
        server_tools.insert("b".to_string(), vec![("shared".to_string(), make_tool("shared"))]);
        server_tools.insert("c".to_string(), vec![("shared".to_string(), make_tool("shared"))]);

        let result = resolve_tool_names(&server_tools);
        assert_eq!(result.len(), 3);

        let mut names: Vec<String> = result.iter().map(|t| t.name().to_string()).collect();
        names.sort();
        assert_eq!(names, vec!["a__shared", "b__shared", "c__shared"]);
    }

    fn names(tools: &[Arc<dyn Tool>]) -> Vec<String> {
        tools.iter().map(|tool| tool.name().to_string()).collect()
    }

    /// `a` + `b__x` and `a__b` + `x` both prefix to `a__b__x`.
    #[test]
    fn prefixed_names_that_repeat_keep_the_first_server_in_id_order() {
        let mut server_tools: ServerToolMap = BTreeMap::new();
        server_tools.insert(
            "z".to_string(),
            vec![("b__x".to_string(), make_tool("b__x")), ("x".to_string(), make_tool("x"))],
        );
        server_tools.insert("a__b".to_string(), vec![("x".to_string(), make_tool("x"))]);
        server_tools.insert("a".to_string(), vec![("b__x".to_string(), make_tool("b__x"))]);

        let result = resolve_tool_names(&server_tools);

        assert_eq!(names(&result), vec!["a__b__x", "z__b__x", "z__x"]);
        assert_eq!(result[0].description(), "Tool b__x", "server a keeps a__b__x");
    }

    #[test]
    fn a_prefixed_name_that_equals_another_servers_tool_is_not_declared_twice() {
        let mut server_tools: ServerToolMap = BTreeMap::new();
        server_tools.insert("a".to_string(), vec![("x".to_string(), make_tool("x"))]);
        server_tools.insert("b".to_string(), vec![("x".to_string(), make_tool("x"))]);
        server_tools.insert("c".to_string(), vec![("a__x".to_string(), make_tool("a__x"))]);

        assert_eq!(names(&resolve_tool_names(&server_tools)), vec!["a__x", "b__x"]);
    }

    #[test]
    fn a_prefixed_name_longer_than_providers_accept_is_skipped() {
        let long_id = "s".repeat(60);
        let mut server_tools: ServerToolMap = BTreeMap::new();
        server_tools.insert(long_id.clone(), vec![("read".to_string(), make_tool("read"))]);
        server_tools.insert("b".to_string(), vec![("read".to_string(), make_tool("read"))]);

        assert_eq!(names(&resolve_tool_names(&server_tools)), vec!["b__read"]);
    }

    /// Carries annotations and safety signals that the wrapper must forward.
    struct HintedTool(Value);

    #[async_trait]
    impl Tool for HintedTool {
        fn name(&self) -> &str {
            "read"
        }
        fn description(&self) -> &str {
            "Reads"
        }
        fn is_read_only(&self) -> bool {
            true
        }
        fn is_concurrency_safe(&self) -> bool {
            true
        }
        fn annotations(&self) -> Option<&Value> {
            Some(&self.0)
        }
        fn origin(&self) -> &str {
            "mcp:server"
        }
        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn test_prefixed_tool_forwards_annotations() {
        let hints = serde_json::json!({ "readOnlyHint": true });
        let prefixed = PrefixedTool {
            inner: Arc::new(HintedTool(hints.clone())),
            prefixed_name: "server__read".to_string(),
        };
        assert_eq!(prefixed.annotations(), Some(&hints));
        assert!(prefixed.is_read_only() && prefixed.is_concurrency_safe());
        assert_eq!(prefixed.effect(), adk_core::ToolEffect::ReadOnly);
        assert_eq!(prefixed.origin(), "mcp:server", "approvals bind to the server, not the prefix");
        assert_eq!(adk_core::ToolOrigin::of(&prefixed).source, "mcp:server");
    }
}
