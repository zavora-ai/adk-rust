//! Integration tests for McpServerManager.
//!
//! These tests run against real MCP server processes via npx.
//! Run with: `cargo nextest run -p adk-tool --test mcp_server_lifecycle_integration_tests`

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::types::Content;
use adk_core::{ReadonlyContext, Toolset};
use adk_tool::mcp::manager::{McpServerConfig, McpServerManager, ServerStatus};

/// Helper to create a playwright server config.
fn playwright_config() -> McpServerConfig {
    McpServerConfig {
        command: "npx".to_string(),
        args: vec!["--yes".to_string(), "@playwright/mcp@latest".to_string()],
        env: HashMap::new(),
        disabled: false,
        auto_approve: vec!["browser_click".to_string(), "browser_wait_for".to_string()],
        restart_policy: None,
    }
}

/// Helper to create a computer-use server config.
fn computer_use_config() -> McpServerConfig {
    McpServerConfig {
        command: "npx".to_string(),
        args: vec![
            "--yes".to_string(),
            "--prefer-offline".to_string(),
            "@zavora-ai/computer-use-mcp".to_string(),
        ],
        env: HashMap::new(),
        disabled: false,
        auto_approve: vec!["wait".to_string()],
        restart_policy: None,
    }
}

/// Minimal ReadonlyContext for testing Toolset::tools().
struct TestContext {
    user_content: Content,
}

impl TestContext {
    fn new() -> Self {
        Self {
            user_content: Content::new("user").with_text("test"),
        }
    }
}

impl ReadonlyContext for TestContext {
    fn invocation_id(&self) -> &str {
        "test-invocation"
    }

    fn agent_name(&self) -> &str {
        "test-agent"
    }

    fn user_id(&self) -> &str {
        "test-user"
    }

    fn app_name(&self) -> &str {
        "test-app"
    }

    fn session_id(&self) -> &str {
        "test-session"
    }

    fn branch(&self) -> &str {
        "main"
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

/// Start a single MCP server (playwright), verify status is Running,
/// stop it, verify status is Stopped.
#[tokio::test]
async fn test_start_stop_single_server() {
    let configs = HashMap::from([("playwright".to_string(), playwright_config())]);
    let manager = McpServerManager::new(configs);

    // Start the server
    let result = tokio::time::timeout(Duration::from_secs(60), manager.start_server("playwright"))
        .await
        .expect("start_server timed out");
    result.expect("start_server failed");

    // Verify Running
    let status = manager.server_status("playwright").await.unwrap();
    assert_eq!(status, ServerStatus::Running);

    // Stop the server
    manager
        .stop_server("playwright")
        .await
        .expect("stop_server failed");

    // Verify Stopped
    let status = manager.server_status("playwright").await.unwrap();
    assert_eq!(status, ServerStatus::Stopped);

    manager.shutdown().await.ok();
}

/// Create manager with both servers, call start_all(), verify both are
/// Running, then shutdown.
#[tokio::test]
async fn test_start_all_multiple_servers() {
    let configs = HashMap::from([
        ("playwright".to_string(), playwright_config()),
        ("computer-use".to_string(), computer_use_config()),
    ]);
    let manager = McpServerManager::new(configs);

    // Start all servers with a timeout
    let results = tokio::time::timeout(Duration::from_secs(90), manager.start_all())
        .await
        .expect("start_all timed out");

    // Verify both started successfully
    for (id, result) in &results {
        result
            .as_ref()
            .unwrap_or_else(|e| panic!("server '{id}' failed to start: {e}"));
    }

    // Verify both are Running
    assert_eq!(
        manager.server_status("playwright").await.unwrap(),
        ServerStatus::Running
    );
    assert_eq!(
        manager.server_status("computer-use").await.unwrap(),
        ServerStatus::Running
    );
    assert_eq!(manager.running_server_count().await, 2);

    manager.shutdown().await.expect("shutdown failed");
}

/// Start a server, restart it, verify it's still Running after restart.
#[tokio::test]
async fn test_restart_server() {
    let configs = HashMap::from([("playwright".to_string(), playwright_config())]);
    let manager = McpServerManager::new(configs);

    // Start the server
    tokio::time::timeout(Duration::from_secs(60), manager.start_server("playwright"))
        .await
        .expect("start_server timed out")
        .expect("start_server failed");

    assert_eq!(
        manager.server_status("playwright").await.unwrap(),
        ServerStatus::Running
    );

    // Restart the server
    tokio::time::timeout(Duration::from_secs(60), manager.restart_server("playwright"))
        .await
        .expect("restart_server timed out")
        .expect("restart_server failed");

    // Verify still Running after restart
    assert_eq!(
        manager.server_status("playwright").await.unwrap(),
        ServerStatus::Running
    );

    manager.shutdown().await.expect("shutdown failed");
}

/// Start a server, use the Toolset::tools() method to list tools, verify
/// we get tools back.
#[tokio::test]
async fn test_tool_aggregation() {
    let configs = HashMap::from([("playwright".to_string(), playwright_config())]);
    let manager = McpServerManager::new(configs);

    // Start the server
    tokio::time::timeout(Duration::from_secs(60), manager.start_server("playwright"))
        .await
        .expect("start_server timed out")
        .expect("start_server failed");

    // Query tools via the Toolset trait
    let ctx: Arc<dyn ReadonlyContext> = Arc::new(TestContext::new());
    let tools = manager.tools(ctx).await.expect("tools() failed");

    // Playwright MCP server should expose at least one tool
    assert!(
        !tools.is_empty(),
        "expected at least one tool from playwright server"
    );

    // Verify tools have names
    for tool in &tools {
        assert!(!tool.name().is_empty(), "tool name should not be empty");
    }

    manager.shutdown().await.expect("shutdown failed");
}

/// Load config from a JSON string (using the real server configs), start
/// all, verify running.
#[tokio::test]
async fn test_from_json_and_start() {
    let json = r#"{
        "mcpServers": {
            "playwright": {
                "command": "npx",
                "args": ["--yes", "@playwright/mcp@latest"],
                "env": {},
                "disabled": false,
                "autoApprove": ["browser_click"]
            }
        }
    }"#;

    let manager = McpServerManager::from_json(json).expect("from_json failed");

    // Start all servers
    let results = tokio::time::timeout(Duration::from_secs(60), manager.start_all())
        .await
        .expect("start_all timed out");

    for (id, result) in &results {
        result
            .as_ref()
            .unwrap_or_else(|e| panic!("server '{id}' failed to start: {e}"));
    }

    assert_eq!(
        manager.server_status("playwright").await.unwrap(),
        ServerStatus::Running
    );

    manager.shutdown().await.expect("shutdown failed");
}

/// Start multiple servers, call shutdown(), verify all are Stopped.
#[tokio::test]
async fn test_graceful_shutdown() {
    let configs = HashMap::from([
        ("playwright".to_string(), playwright_config()),
        ("computer-use".to_string(), computer_use_config()),
    ]);
    let manager = McpServerManager::new(configs);

    // Start all servers
    let results = tokio::time::timeout(Duration::from_secs(90), manager.start_all())
        .await
        .expect("start_all timed out");

    for (id, result) in &results {
        result
            .as_ref()
            .unwrap_or_else(|e| panic!("server '{id}' failed to start: {e}"));
    }

    assert_eq!(manager.running_server_count().await, 2);

    // Graceful shutdown
    manager.shutdown().await.expect("shutdown failed");

    // Verify all are Stopped
    let statuses = manager.all_statuses().await;
    for (id, status) in &statuses {
        assert_eq!(
            *status,
            ServerStatus::Stopped,
            "server '{id}' should be Stopped after shutdown"
        );
    }
    assert_eq!(manager.running_server_count().await, 0);
}

/// Create empty manager, add a server dynamically, start it, remove it.
#[tokio::test]
async fn test_dynamic_add_remove() {
    let manager = McpServerManager::new(HashMap::new());

    // Dynamically add a server
    manager
        .add_server("playwright".to_string(), playwright_config())
        .await
        .expect("add_server failed");

    assert_eq!(
        manager.server_status("playwright").await.unwrap(),
        ServerStatus::Stopped
    );

    // Start the dynamically added server
    tokio::time::timeout(Duration::from_secs(60), manager.start_server("playwright"))
        .await
        .expect("start_server timed out")
        .expect("start_server failed");

    assert_eq!(
        manager.server_status("playwright").await.unwrap(),
        ServerStatus::Running
    );

    // Remove the server (should stop it first)
    manager
        .remove_server("playwright")
        .await
        .expect("remove_server failed");

    // Verify it no longer exists
    let result = manager.server_status("playwright").await;
    assert!(result.is_err(), "server should no longer exist after removal");

    manager.shutdown().await.ok();
}

/// Start some servers, check server_status(), all_statuses(),
/// running_server_count().
#[tokio::test]
async fn test_status_reporting() {
    let configs = HashMap::from([
        ("playwright".to_string(), playwright_config()),
        ("computer-use".to_string(), computer_use_config()),
    ]);
    let manager = McpServerManager::new(configs);

    // Before starting: both should be Stopped
    let statuses = manager.all_statuses().await;
    assert_eq!(statuses.len(), 2);
    assert_eq!(statuses["playwright"], ServerStatus::Stopped);
    assert_eq!(statuses["computer-use"], ServerStatus::Stopped);
    assert_eq!(manager.running_server_count().await, 0);

    // Start only playwright
    tokio::time::timeout(Duration::from_secs(60), manager.start_server("playwright"))
        .await
        .expect("start_server timed out")
        .expect("start_server failed");

    // Check individual status
    assert_eq!(
        manager.server_status("playwright").await.unwrap(),
        ServerStatus::Running
    );
    assert_eq!(
        manager.server_status("computer-use").await.unwrap(),
        ServerStatus::Stopped
    );

    // Check running count
    assert_eq!(manager.running_server_count().await, 1);

    // Check all_statuses
    let statuses = manager.all_statuses().await;
    assert_eq!(statuses["playwright"], ServerStatus::Running);
    assert_eq!(statuses["computer-use"], ServerStatus::Stopped);

    manager.shutdown().await.expect("shutdown failed");
}

/// Try to start a nonexistent server ID, verify error.
#[tokio::test]
async fn test_start_server_unknown_id_returns_error() {
    let manager = McpServerManager::new(HashMap::new());

    let result = manager.start_server("nonexistent-server").await;
    assert!(result.is_err(), "starting unknown server should return error");

    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("unknown server ID"),
        "error should mention unknown server ID, got: {err_msg}"
    );
}

/// Stop a server that's not running, verify no error.
#[tokio::test]
async fn test_stop_server_not_running_is_noop() {
    let configs = HashMap::from([("playwright".to_string(), playwright_config())]);
    let manager = McpServerManager::new(configs);

    // Server is Stopped, stopping it again should be a no-op
    let result = manager.stop_server("playwright").await;
    assert!(
        result.is_ok(),
        "stopping a non-running server should succeed as no-op"
    );

    // Status should still be Stopped
    assert_eq!(
        manager.server_status("playwright").await.unwrap(),
        ServerStatus::Stopped
    );

    manager.shutdown().await.ok();
}
