//! CLI commands for graph time-travel debugging.
//!
//! Provides `adk graph steps`, `adk graph replay`, `adk graph fork`, and
//! `adk graph resume` commands for inspecting and manipulating graph execution
//! history from the terminal.

use anyhow::{Context, Result};

use crate::cli::GraphCommands;

/// Execute a graph time-travel CLI command.
pub async fn run(command: GraphCommands) -> Result<()> {
    match command {
        GraphCommands::Steps { thread_id, db } => run_steps(&thread_id, db.as_deref()).await,
        GraphCommands::Replay { thread_id, from, to, db } => {
            run_replay(&thread_id, from, to, db.as_deref()).await
        }
        GraphCommands::Fork { thread_id, at, new_thread, db } => {
            run_fork(&thread_id, at, &new_thread, db.as_deref()).await
        }
        GraphCommands::Resume { thread_id, from, db } => {
            run_resume(&thread_id, from, db.as_deref()).await
        }
    }
}

/// List all checkpoints for a thread.
async fn run_steps(thread_id: &str, db: Option<&str>) -> Result<()> {
    let checkpointer = load_checkpointer(db)?;
    let handle = standalone_handle(thread_id, checkpointer.clone());

    let steps = handle.steps().await.context("failed to list steps")?;

    if steps.is_empty() {
        println!("No checkpoints found for thread '{thread_id}'.");
        return Ok(());
    }

    println!("Thread: {thread_id}");
    println!("{:<6} {:<40} {:<26} Pending Nodes", "Step", "Checkpoint ID", "Timestamp");
    println!("{}", "-".repeat(100));

    for step in &steps {
        let ts = step.timestamp.as_deref().unwrap_or("-");
        let pending = if step.pending_nodes.is_empty() {
            "(none)".to_string()
        } else {
            step.pending_nodes.join(", ")
        };
        println!("{:<6} {:<40} {:<26} {}", step.step, step.checkpoint_id, ts, pending);
    }

    println!("\nTotal: {} checkpoint(s)", steps.len());
    Ok(())
}

/// Print the recorded state at each checkpointed step in a range.
///
/// Reads checkpoints; nothing is executed.
async fn run_replay(
    thread_id: &str,
    from: usize,
    to: Option<usize>,
    db: Option<&str>,
) -> Result<()> {
    let checkpointer = load_checkpointer(db)?;
    let handle = standalone_handle(thread_id, checkpointer.clone());

    let transitions =
        handle.state_history(from, to).await.context("failed to read state history")?;

    if transitions.is_empty() {
        println!("No state transitions found in the specified range.");
        return Ok(());
    }

    let range_desc = match to {
        Some(end) => format!("{from}..={end}"),
        None => format!("{from}..end"),
    };
    println!("Recorded state for thread '{thread_id}' (steps {range_desc}):\n");

    for (step, state) in &transitions {
        println!("── Step {step} ──");
        let json = serde_json::to_string_pretty(state).unwrap_or_else(|_| format!("{state:?}"));
        println!("{json}\n");
    }

    println!("{} recorded state(s) shown; nothing was re-executed.", transitions.len());
    Ok(())
}

/// Fork execution at a step into a new thread.
async fn run_fork(thread_id: &str, at: usize, new_thread: &str, db: Option<&str>) -> Result<()> {
    let checkpointer = load_checkpointer(db)?;
    let handle = standalone_handle(thread_id, checkpointer.clone());

    handle.fork_at(at, new_thread).await.context("failed to fork thread")?;

    println!("Forked thread '{thread_id}' at step {at} → new thread '{new_thread}'.");
    Ok(())
}

/// Resume execution from a specific step.
///
/// Note: Resuming requires a compiled graph, which is not available from the CLI
/// alone. This command prints guidance on how to use the API programmatically.
async fn run_resume(thread_id: &str, from: usize, db: Option<&str>) -> Result<()> {
    let _checkpointer = load_checkpointer(db)?;

    println!("Resume is not yet supported from the CLI.");
    println!();
    println!("Resuming execution requires a compiled graph, which cannot be");
    println!("constructed from CLI arguments alone.");
    println!();
    println!("To resume programmatically, use the TimeTravelHandle API:");
    println!();
    println!("    let handle = graph.time_travel(\"{thread_id}\");");
    println!("    let config = ExecutionConfig::new(\"{thread_id}\");");
    println!("    let state = handle.resume_from({from}, config).await?;");
    println!();
    println!("See: https://docs.rs/adk-graph/latest/adk_graph/time_travel/");
    Ok(())
}

/// Load a checkpointer from the given database path, or use an in-memory one.
fn load_checkpointer(
    db: Option<&str>,
) -> Result<std::sync::Arc<dyn adk_graph::checkpoint::Checkpointer>> {
    match db {
        Some(_path) => {
            // SQLite checkpointer requires the `sqlite` feature on adk-graph.
            // For now, inform the user that SQLite support requires a build with
            // the sqlite feature enabled.
            #[cfg(feature = "graph-sqlite")]
            {
                let rt = tokio::runtime::Handle::current();
                let checkpointer = rt.block_on(async {
                    adk_graph::checkpoint::SqliteCheckpointer::new(_path)
                        .await
                        .context("failed to open SQLite checkpoint database")
                })?;
                Ok(std::sync::Arc::new(checkpointer))
            }
            #[cfg(not(feature = "graph-sqlite"))]
            {
                anyhow::bail!(
                    "SQLite checkpoint support requires the `graph-sqlite` feature.\n\
                     Rebuild with: cargo install adk-cli --features graph-sqlite\n\
                     \n\
                     Without --db, an in-memory checkpointer is used (useful for testing)."
                );
            }
        }
        None => {
            // Use in-memory checkpointer — useful for testing the CLI structure,
            // but won't have any pre-existing checkpoints.
            Ok(std::sync::Arc::new(adk_graph::checkpoint::MemoryCheckpointer::new()))
        }
    }
}

/// Create a standalone `TimeTravelHandle` without a full compiled graph.
///
/// Since the CLI doesn't have access to a compiled graph, we create a minimal
/// graph just to satisfy the `TimeTravelHandle` constructor. The `steps()`,
/// `replay()`, and `fork_at()` methods only need the checkpointer — they don't
/// re-execute the graph.
fn standalone_handle(
    thread_id: &str,
    checkpointer: std::sync::Arc<dyn adk_graph::checkpoint::Checkpointer>,
) -> StandaloneTimeTravelHandle {
    StandaloneTimeTravelHandle { thread_id: thread_id.to_string(), checkpointer }
}

/// A lightweight time-travel handle that works without a compiled graph.
///
/// This mirrors the read-only operations of [`adk_graph::TimeTravelHandle`]
/// but doesn't require a graph reference, making it suitable for CLI usage
/// where we only need to inspect checkpoints.
struct StandaloneTimeTravelHandle {
    thread_id: String,
    checkpointer: std::sync::Arc<dyn adk_graph::checkpoint::Checkpointer>,
}

/// Step information mirroring [`adk_graph::StepInfo`] for display purposes.
struct CliStepInfo {
    step: usize,
    checkpoint_id: String,
    timestamp: Option<String>,
    pending_nodes: Vec<String>,
}

impl StandaloneTimeTravelHandle {
    /// List all checkpointed steps for this thread.
    async fn steps(&self) -> adk_graph::error::Result<Vec<CliStepInfo>> {
        let checkpoints = self.checkpointer.list(&self.thread_id).await?;
        let mut steps: Vec<CliStepInfo> = checkpoints
            .into_iter()
            .map(|cp| CliStepInfo {
                step: cp.step,
                checkpoint_id: cp.checkpoint_id,
                timestamp: Some(cp.created_at.to_rfc3339()),
                pending_nodes: cp.pending_nodes,
            })
            .collect();
        steps.sort_by_key(|s| s.step);
        Ok(steps)
    }

    /// Return the recorded state at each checkpointed step in a range.
    async fn state_history(
        &self,
        from_step: usize,
        to_step: Option<usize>,
    ) -> adk_graph::error::Result<Vec<(usize, adk_graph::state::State)>> {
        let mut checkpoints = self.checkpointer.list(&self.thread_id).await?;
        checkpoints.sort_by_key(|cp| cp.step);

        let results: Vec<(usize, adk_graph::state::State)> = checkpoints
            .into_iter()
            .filter(|cp| cp.step >= from_step && to_step.is_none_or(|end| cp.step <= end))
            .map(|cp| (cp.step, cp.state))
            .collect();

        if results.is_empty() || results[0].0 != from_step {
            return Err(adk_graph::error::GraphError::CheckpointError(format!(
                "no checkpoint found at step {from_step} for thread '{}'",
                self.thread_id
            )));
        }

        Ok(results)
    }

    /// Fork the thread at a specific step into a new thread.
    async fn fork_at(&self, step: usize, new_thread_id: &str) -> adk_graph::error::Result<()> {
        let checkpoints = self.checkpointer.list(&self.thread_id).await?;

        let checkpoint = checkpoints.into_iter().find(|cp| cp.step == step).ok_or_else(|| {
            adk_graph::error::GraphError::CheckpointError(format!(
                "no checkpoint found at step {step} for thread '{}'",
                self.thread_id
            ))
        })?;

        let forked = adk_graph::state::Checkpoint::new(
            new_thread_id,
            checkpoint.state,
            checkpoint.step,
            checkpoint.pending_nodes,
        );

        self.checkpointer.save(&forked).await?;
        Ok(())
    }
}
