//! Action Nodes Example
//!
//! Demonstrates all action node types in adk-graph.
//!
//! **Core scenarios** (no extra deps):
//! 1–10: Set, Transform, Switch, Loop, Merge, Wait, File, Trigger, WorkflowSchema, Error Handling
//!
//! **Feature-gated scenarios** (run with `--features http`):
//! 11: HTTP Node — GET/POST with auth, interpolation, status validation
//! 12: Code Node — Rust mode JSON eval and interpolation
//! 13: Database Node — Config validation (placeholder executors)
//! 14: Notification Node — Slack/Discord/Teams/webhook payload dispatch
//!
//! Run core:  `cargo run -p action-nodes-example`
//! Run all:   `cargo run -p action-nodes-example --features full`

mod scenarios;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    println!("═══════════════════════════════════════════════════");
    println!("  ADK Action Nodes — All Scenarios");
    println!("═══════════════════════════════════════════════════\n");

    // Core scenarios (always available)
    scenarios::set_node::run().await?;
    scenarios::transform_node::run().await?;
    scenarios::switch_node::run().await?;
    scenarios::loop_node::run().await?;
    scenarios::merge_node::run().await?;
    scenarios::wait_node::run().await?;
    scenarios::file_node::run().await?;
    scenarios::trigger_node::run().await?;
    scenarios::workflow_schema::run().await?;
    scenarios::error_handling::run().await?;

    // Always-available feature-gated scenarios
    scenarios::code_node::run().await?;

    // HTTP-dependent scenarios
    #[cfg(feature = "http")]
    {
        scenarios::http_node::run().await?;
        scenarios::database_node::run().await?;
        scenarios::notification_node::run().await?;
    }

    #[cfg(not(feature = "http"))]
    {
        println!("── Feature-gated scenarios ────────────────────");
        println!("  HTTP, Database, Notification nodes require --features http");
        println!("  Run: cargo run -p action-nodes-example --features full\n");
    }

    println!("═══════════════════════════════════════════════════");
    println!("  All scenarios completed successfully!");
    println!("═══════════════════════════════════════════════════");

    Ok(())
}
