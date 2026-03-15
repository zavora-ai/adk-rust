//! adk-auth Audit Logging Example
//!
//! Demonstrates role-based access control with audit logging.
//!
//! Run: cargo run --example auth_audit

use adk_auth::{
    AccessControl, AuditEvent, AuditOutcome, AuditSink, AuthError, AuthMiddleware, FileAuditSink,
    Permission, Role,
};
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::{Arc, Mutex};

// In-memory audit sink for demonstration
struct MemoryAuditSink {
    events: Mutex<Vec<AuditEvent>>,
}

impl MemoryAuditSink {
    fn new() -> Self {
        Self { events: Mutex::new(Vec::new()) }
    }

    fn events(&self) -> Vec<AuditEvent> {
        self.events.lock().unwrap().clone()
    }
}

#[async_trait]
impl AuditSink for MemoryAuditSink {
    async fn log(&self, event: AuditEvent) -> std::result::Result<(), AuthError> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
}

// Simple tool for demonstration
struct DataQueryTool;

#[async_trait]
impl Tool for DataQueryTool {
    fn name(&self) -> &str {
        "data_query"
    }
    fn description(&self) -> &str {
        "Query data from the database"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(serde_json::json!({"rows": 42}))
    }
}

fn main() -> anyhow::Result<()> {
    println!("adk-auth Audit Logging Example");
    println!("==============================\n");

    // Define roles
    let data_analyst = Role::new("data_analyst").allow(Permission::Tool("data_query".into()));

    let guest = Role::new("guest"); // No permissions

    // Build access control
    let ac = AccessControl::builder()
        .role(data_analyst)
        .role(guest)
        .assign("analyst@company.com", "data_analyst")
        .assign("guest@company.com", "guest")
        .build()?;

    println!("Roles:");
    println!("  - data_analyst: can query data");
    println!("  - guest: no permissions");
    println!();

    // Method 1: File-based audit logging
    println!("1. File-based audit logging:");
    let audit_path = "/tmp/adk_audit.jsonl";
    let file_audit = FileAuditSink::new(audit_path)?;
    println!("   Audit log: {}", audit_path);

    let middleware = AuthMiddleware::with_audit(ac.clone(), file_audit);
    let _protected_tool = middleware.protect(DataQueryTool);
    println!("   ✅ Tool protected with file audit\n");

    // Method 2: Check and log manually
    println!("2. Manual permission checks with logging:");

    // Simulate permission checks
    let checks = [
        ("analyst@company.com", "data_query", true),
        ("guest@company.com", "data_query", false),
        ("unknown@company.com", "data_query", false),
    ];

    for (user, tool, expected) in checks {
        let perm = Permission::Tool(tool.into());
        let result = ac.check(user, &perm);
        let outcome = if result.is_ok() { AuditOutcome::Allowed } else { AuditOutcome::Denied };

        // Create and log audit event
        let event = AuditEvent::tool_access(user, tool, outcome.clone());
        println!(
            "   {} {} -> {} ({:?})",
            if expected == result.is_ok() { "✅" } else { "❌" },
            user,
            tool,
            outcome
        );

        // In a real app, you'd log this:
        // audit_sink.log(event).await?;
        let _ = event; // Suppress unused warning
    }
    println!();

    // Method 3: Memory audit sink for testing
    println!("3. In-memory audit (for testing):");
    let memory_audit = Arc::new(MemoryAuditSink::new());

    // Simulate some events
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        memory_audit
            .log(AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed))
            .await
            .unwrap();
        memory_audit
            .log(AuditEvent::tool_access("bob", "admin", AuditOutcome::Denied))
            .await
            .unwrap();
    });

    let events = memory_audit.events();
    println!("   Captured {} audit events:", events.len());
    for event in &events {
        println!("     - {} -> {} ({:?})", event.user, event.resource, event.outcome);
    }
    println!();

    println!("Example complete! Check {} for audit log.", audit_path);

    Ok(())
}
