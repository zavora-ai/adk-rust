//! adk-auth Basic Example
//!
//! Demonstrates role-based access control for tools.
//!
//! Run: cargo run --example auth_basic

use adk_auth::{AccessControl, AuthMiddleware, Permission, Role};
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

// Simple tools for demonstration
struct SearchTool;
struct CodeExecTool;
struct AdminTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }
    fn description(&self) -> &str {
        "Search the web"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(serde_json::json!({"result": "Search results..."}))
    }
}

#[async_trait]
impl Tool for CodeExecTool {
    fn name(&self) -> &str {
        "code_exec"
    }
    fn description(&self) -> &str {
        "Execute code (dangerous!)"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(serde_json::json!({"result": "Code executed"}))
    }
}

#[async_trait]
impl Tool for AdminTool {
    fn name(&self) -> &str {
        "admin_panel"
    }
    fn description(&self) -> &str {
        "Admin operations"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(serde_json::json!({"result": "Admin action completed"}))
    }
}

fn main() -> anyhow::Result<()> {
    println!("adk-auth Basic Example");
    println!("======================\n");

    // Define roles
    let admin = Role::new("admin").allow(Permission::AllTools).allow(Permission::AllAgents);

    let analyst = Role::new("analyst")
        .allow(Permission::Tool("search".into()))
        .deny(Permission::Tool("code_exec".into()))
        .deny(Permission::Tool("admin_panel".into()));

    let developer = Role::new("developer")
        .allow(Permission::Tool("search".into()))
        .allow(Permission::Tool("code_exec".into()))
        .deny(Permission::Tool("admin_panel".into()));

    // Build access control
    let ac = AccessControl::builder()
        .role(admin)
        .role(analyst)
        .role(developer)
        .assign("alice@company.com", "admin")
        .assign("bob@company.com", "analyst")
        .assign("charlie@company.com", "developer")
        .build()?;

    println!("Roles defined:");
    println!("  - admin: all tools");
    println!("  - analyst: search only");
    println!("  - developer: search + code_exec");
    println!();

    // Test permissions
    let test_cases = [
        ("alice@company.com", "search"),
        ("alice@company.com", "code_exec"),
        ("alice@company.com", "admin_panel"),
        ("bob@company.com", "search"),
        ("bob@company.com", "code_exec"),
        ("bob@company.com", "admin_panel"),
        ("charlie@company.com", "search"),
        ("charlie@company.com", "code_exec"),
        ("charlie@company.com", "admin_panel"),
    ];

    println!("Permission checks:");
    for (user, tool) in test_cases {
        let perm = Permission::Tool(tool.into());
        let result = ac.check(user, &perm);
        let status = if result.is_ok() { "✅" } else { "❌" };
        println!("  {} {} -> {}", status, user, tool);
    }
    println!();

    // Create middleware to protect tools
    let middleware = AuthMiddleware::new(ac);
    let tools: Vec<Arc<dyn Tool>> =
        vec![Arc::new(SearchTool), Arc::new(CodeExecTool), Arc::new(AdminTool)];

    let protected_tools = middleware.protect_all(tools);
    println!("Protected {} tools with access control", protected_tools.len());

    Ok(())
}
