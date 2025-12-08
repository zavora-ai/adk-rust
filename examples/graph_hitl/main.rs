//! Human-in-the-Loop (HITL) Example
//!
//! This example demonstrates interrupt-based human intervention in graph workflows.
//! The graph pauses execution to allow human review/approval before continuing.
//!
//! Graph: START -> plan -> [interrupt] -> execute -> END
//!
//! Key concepts demonstrated:
//! - Static interrupts (interrupt_before/interrupt_after)
//! - Dynamic interrupts from within nodes
//! - State editing during pause
//! - Resuming execution after interrupt

use adk_graph::{
    checkpoint::MemoryCheckpointer,
    edge::{END, START},
    error::GraphError,
    graph::StateGraph,
    node::{ExecutionConfig, NodeOutput},
    state::State,
};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Human-in-the-Loop Example ===\n");

    // Create a checkpointer to persist state across interrupts
    let checkpointer = Arc::new(MemoryCheckpointer::new());

    // Build a workflow that requires human approval
    let graph = StateGraph::with_channels(&["task", "plan", "approved", "result", "risk_level"])
        // Planning node: creates an execution plan
        .add_node_fn("plan", |ctx| async move {
            let task = ctx
                .get("task")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown task");

            println!("[plan] Creating execution plan for: {}", task);

            // Simulate planning - determine risk level
            let (plan, risk_level) = if task.contains("delete") || task.contains("production") {
                (
                    format!(
                        "HIGH RISK PLAN:\n1. Backup current state\n2. Execute: {}\n3. Verify results",
                        task
                    ),
                    "high",
                )
            } else if task.contains("update") || task.contains("modify") {
                (
                    format!("MEDIUM RISK PLAN:\n1. Execute: {}\n2. Verify results", task),
                    "medium",
                )
            } else {
                (format!("LOW RISK PLAN:\n1. Execute: {}", task), "low")
            };

            println!("[plan] Risk level: {}", risk_level);
            println!("[plan] Plan created:\n{}", plan);

            Ok(NodeOutput::new()
                .with_update("plan", json!(plan))
                .with_update("risk_level", json!(risk_level)))
        })
        // Review node: checks if approval is needed and may interrupt
        .add_node_fn("review", |ctx| async move {
            let risk_level = ctx
                .get("risk_level")
                .and_then(|v| v.as_str())
                .unwrap_or("low");
            let plan = ctx
                .get("plan")
                .and_then(|v| v.as_str())
                .unwrap_or("No plan");
            let approved = ctx.get("approved").and_then(|v| v.as_bool());

            println!("[review] Checking approval status...");

            // If already approved, continue
            if approved == Some(true) {
                println!("[review] Already approved, proceeding...");
                return Ok(NodeOutput::new());
            }

            // For high/medium risk, require approval via dynamic interrupt
            if risk_level == "high" || risk_level == "medium" {
                println!(
                    "[review] {} risk detected - requesting human approval",
                    risk_level.to_uppercase()
                );

                // Return an interrupt to pause for human review
                return Ok(NodeOutput::interrupt_with_data(
                    &format!(
                        "Please review and approve the {} risk plan",
                        risk_level.to_uppercase()
                    ),
                    json!({
                        "plan": plan,
                        "risk_level": risk_level,
                        "action_required": "Set 'approved' to true to continue"
                    }),
                ));
            }

            // Low risk: auto-approve
            println!("[review] Low risk - auto-approving");
            Ok(NodeOutput::new().with_update("approved", json!(true)))
        })
        // Execute node: carries out the plan
        .add_node_fn("execute", |ctx| async move {
            let plan = ctx
                .get("plan")
                .and_then(|v| v.as_str())
                .unwrap_or("No plan");
            let approved = ctx.get("approved").and_then(|v| v.as_bool()).unwrap_or(false);

            if !approved {
                println!("[execute] ERROR: Cannot execute without approval!");
                return Ok(NodeOutput::new().with_update(
                    "result",
                    json!("Execution blocked: Not approved"),
                ));
            }

            println!("[execute] Executing approved plan...");
            println!("[execute] {}", plan);

            // Simulate execution
            let result = "Successfully executed plan. All steps completed.".to_string();
            println!("[execute] Result: {}", result);

            Ok(NodeOutput::new().with_update("result", json!(result)))
        })
        // Define edges
        .add_edge(START, "plan")
        .add_edge("plan", "review")
        .add_edge("review", "execute")
        .add_edge("execute", END)
        .compile()?
        .with_checkpointer_arc(checkpointer.clone());

    // ========== Test 1: Low Risk Task (auto-approved) ==========
    println!("{}", "=".repeat(60));
    println!("TEST 1: Low Risk Task (auto-approved)");
    println!("{}", "=".repeat(60));

    let mut input = State::new();
    input.insert("task".to_string(), json!("Read configuration file"));

    let result = graph.invoke(input, ExecutionConfig::new("low-risk-thread")).await?;

    println!("\nFinal result: {}", result.get("result").and_then(|v| v.as_str()).unwrap_or("none"));

    // ========== Test 2: High Risk Task (requires approval) ==========
    println!("\n{}", "=".repeat(60));
    println!("TEST 2: High Risk Task (requires approval)");
    println!("{}", "=".repeat(60));

    let mut input = State::new();
    input.insert("task".to_string(), json!("delete all records from production database"));

    let thread_id = "high-risk-thread";
    let result = graph.invoke(input, ExecutionConfig::new(thread_id)).await;

    // Expect an interrupt
    match result {
        Err(GraphError::Interrupted(interrupted)) => {
            println!("\n*** EXECUTION PAUSED ***");
            println!("Interrupt: {}", interrupted.interrupt);
            println!("Thread ID: {}", interrupted.thread_id);
            println!("State at pause:");
            println!(
                "  - Plan: {}",
                interrupted.state.get("plan").and_then(|v| v.as_str()).unwrap_or("?")
            );
            println!(
                "  - Risk: {}",
                interrupted.state.get("risk_level").and_then(|v| v.as_str()).unwrap_or("?")
            );

            // Simulate human review and approval
            println!("\n[HUMAN] Reviewing plan...");
            println!("[HUMAN] Approving execution.");

            // Update state with approval
            graph.update_state(thread_id, [("approved".to_string(), json!(true))]).await?;

            // Resume execution
            println!("\n*** RESUMING EXECUTION ***\n");
            let final_result = graph.invoke(State::new(), ExecutionConfig::new(thread_id)).await?;

            println!(
                "\nFinal result: {}",
                final_result.get("result").and_then(|v| v.as_str()).unwrap_or("none")
            );
        }
        Ok(state) => {
            println!("Completed without interrupt: {:?}", state.get("result"));
        }
        Err(e) => {
            println!("Error: {}", e);
        }
    }

    // ========== Test 3: Demonstrate interrupt_before ==========
    println!("\n{}", "=".repeat(60));
    println!("TEST 3: Using interrupt_before (static interrupt)");
    println!("{}", "=".repeat(60));

    // Create a graph with static interrupt_before
    let graph_with_interrupt = StateGraph::with_channels(&["data", "prepared"])
        .add_node_fn("prepare", |ctx| async move {
            let data = ctx.get("data").and_then(|v| v.as_str()).unwrap_or("none");
            println!("[prepare] Preparing data: {}", data);
            Ok(NodeOutput::new().with_update("prepared", json!(format!("Ready: {}", data))))
        })
        .add_node_fn("execute", |ctx| async move {
            let prepared = ctx.get("prepared").and_then(|v| v.as_str()).unwrap_or("none");
            println!("[execute] Executing: {}", prepared);
            Ok(NodeOutput::new().with_update("result", json!(format!("Done: {}", prepared))))
        })
        .add_edge(START, "prepare")
        .add_edge("prepare", "execute")
        .add_edge("execute", END)
        .compile()?
        .with_interrupt_before(&["execute"]); // Pause before execute

    let mut input = State::new();
    input.insert("data".to_string(), json!("important document"));

    let result = graph_with_interrupt.invoke(input, ExecutionConfig::new("interrupt-thread")).await;

    match result {
        Err(GraphError::Interrupted(interrupted)) => {
            println!("\n*** PAUSED BEFORE EXECUTE NODE ***");
            println!("Interrupt type: {}", interrupted.interrupt);
            println!("State preserved: prepared = {:?}", interrupted.state.get("prepared"));
            println!("Thread ID: {}", interrupted.thread_id);
            if !interrupted.checkpoint_id.is_empty() {
                println!(
                    "Checkpoint ID: {}...{}",
                    &interrupted.checkpoint_id[..8],
                    &interrupted.checkpoint_id[interrupted.checkpoint_id.len() - 4..]
                );
            }
            println!("\n(In a real app, you would update state and resume from this checkpoint)");
        }
        Ok(state) => {
            println!("Completed: {:?}", state.get("result"));
        }
        Err(e) => {
            println!("Error: {}", e);
        }
    }

    println!("\n=== Complete ===");
    Ok(())
}
