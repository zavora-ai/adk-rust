//! Action Nodes Example
//!
//! Demonstrates all core action node types in adk-graph:
//!
//! 1. **Set Node** — Initialize, merge, and delete state variables
//! 2. **Transform Node** — Template interpolation and JSONPath extraction
//! 3. **Switch Node** — Conditional routing with typed operators
//! 4. **Loop Node** — forEach, while, and times iteration
//! 5. **Merge Node** — Combine parallel branch results
//! 6. **Wait Node** — Fixed-duration delays
//! 7. **File Node** — Read, write, list, and delete files
//! 8. **Trigger Node** — Manual trigger with input
//! 9. **WorkflowSchema** — Load and execute a graph from JSON
//!
//! Run: `cargo run -p action-nodes-example`

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
    println!("  ADK Action Nodes — All Core Scenarios");
    println!("═══════════════════════════════════════════════════\n");

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

    println!("\n═══════════════════════════════════════════════════");
    println!("  All scenarios completed successfully!");
    println!("═══════════════════════════════════════════════════");

    Ok(())
}
