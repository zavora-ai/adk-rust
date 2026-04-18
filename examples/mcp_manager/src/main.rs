//! MCP Server Manager Example
//!
//! Demonstrates the full lifecycle of `McpServerManager`:
//! - Loading config from JSON
//! - Starting all servers
//! - Querying tools via the Toolset trait
//! - Checking server statuses
//! - Dynamic add/remove of servers
//! - Graceful shutdown

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::types::Content;
use adk_core::{ReadonlyContext, Toolset};
use adk_tool::mcp::manager::{McpServerConfig, McpServerManager};

/// Minimal ReadonlyContext for calling Toolset::tools().
struct ExampleContext {
    user_content: Content,
}

impl ExampleContext {
    fn new() -> Self {
        Self { user_content: Content::new("user").with_text("example") }
    }
}

impl ReadonlyContext for ExampleContext {
    fn invocation_id(&self) -> &str { "example-invocation" }
    fn agent_name(&self) -> &str { "example-agent" }
    fn user_id(&self) -> &str { "example-user" }
    fn app_name(&self) -> &str { "mcp-manager-example" }
    fn session_id(&self) -> &str { "example-session" }
    fn branch(&self) -> &str { "main" }
    fn user_content(&self) -> &Content { &self.user_content }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    println!("=== MCP Server Manager Example ===\n");

    // ── 1. Load config from JSON ──────────────────────────────────────
    println!("1. Loading config from JSON...");
    let json = r#"{
        "mcpServers": {
            "playwright": {
                "command": "npx",
                "args": ["--yes", "@playwright/mcp@latest"],
                "disabled": false,
                "autoApprove": ["browser_click", "browser_wait_for"]
            }
        }
    }"#;

    let manager = McpServerManager::from_json(json)?
        .with_health_check_interval(Duration::from_secs(30))
        .with_grace_period(Duration::from_secs(5))
        .with_name("example_manager");

    println!("   Manager created with name: {}\n", Toolset::name(&manager));

    // ── 2. Check initial statuses ─────────────────────────────────────
    println!("2. Initial statuses:");
    for (id, status) in manager.all_statuses().await {
        println!("   {id}: {status:?}");
    }
    println!();

    // ── 3. Start all servers ──────────────────────────────────────────
    println!("3. Starting all servers...");
    let results = manager.start_all().await;
    for (id, result) in &results {
        match result {
            Ok(()) => println!("   {id}: started successfully"),
            Err(e) => println!("   {id}: failed to start: {e}"),
        }
    }
    println!("   Running servers: {}\n", manager.running_server_count().await);

    // ── 4. Query tools via Toolset trait ──────────────────────────────
    println!("4. Querying tools from all running servers...");
    let ctx: Arc<dyn ReadonlyContext> = Arc::new(ExampleContext::new());
    let tools = manager.tools(ctx).await?;
    println!("   Found {} tools:", tools.len());
    for tool in &tools {
        println!("   - {} : {}", tool.name(), truncate(tool.description(), 60));
    }
    println!();

    // ── 5. Dynamic add/remove ─────────────────────────────────────────
    println!("5. Dynamically adding computer-use server...");
    let computer_use_config = McpServerConfig {
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
    };
    manager.add_server("computer-use".to_string(), computer_use_config).await?;
    println!("   Added. Status: {:?}", manager.server_status("computer-use").await?);

    println!("   Starting computer-use server...");
    manager.start_server("computer-use").await?;
    println!("   Status: {:?}", manager.server_status("computer-use").await?);
    println!("   Running servers: {}\n", manager.running_server_count().await);

    // ── 6. Status reporting ───────────────────────────────────────────
    println!("6. All statuses:");
    for (id, status) in manager.all_statuses().await {
        println!("   {id}: {status:?}");
    }
    println!();

    // ── 7. Remove a server ────────────────────────────────────────────
    println!("7. Removing computer-use server...");
    manager.remove_server("computer-use").await?;
    println!("   Removed. Remaining servers: {}\n", manager.all_statuses().await.len());

    // ── 8. Graceful shutdown ──────────────────────────────────────────
    println!("8. Shutting down all servers...");
    manager.shutdown().await?;
    println!("   All servers stopped.");
    println!("   Running servers: {}\n", manager.running_server_count().await);

    println!("=== Example complete ===");
    Ok(())
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}
