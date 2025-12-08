//! Checkpointing and Persistence Example
//!
//! This example demonstrates state persistence with checkpointing, enabling:
//! - State recovery after failures
//! - Time travel (viewing/restoring past states)
//! - Long-running workflows that survive restarts
//!
//! Key concepts demonstrated:
//! - MemoryCheckpointer for development
//! - Saving checkpoints after each step
//! - Loading and listing checkpoints
//! - Resuming from specific checkpoints
//! - Time travel debugging

use adk_graph::{
    checkpoint::{Checkpointer, MemoryCheckpointer},
    edge::{END, START},
    graph::StateGraph,
    node::{ExecutionConfig, NodeOutput},
    state::State,
};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Checkpointing and Persistence Example ===\n");

    // Create a checkpointer to persist state
    let checkpointer = Arc::new(MemoryCheckpointer::new());

    // Build a multi-step workflow
    let graph = StateGraph::with_channels(&["items", "processed", "validated", "result"])
        // Step 1: Fetch items
        .add_node_fn("fetch", |_ctx| async move {
            println!("[fetch] Fetching items from data source...");

            // Simulate fetching data
            let items = vec![
                json!({"id": 1, "name": "Item A", "value": 100}),
                json!({"id": 2, "name": "Item B", "value": 200}),
                json!({"id": 3, "name": "Item C", "value": 300}),
            ];

            println!("[fetch] Retrieved {} items", items.len());

            Ok(NodeOutput::new()
                .with_update("items", json!(items))
                .with_update("step", json!("fetch_complete")))
        })
        // Step 2: Process items
        .add_node_fn("process", |ctx| async move {
            let items = ctx.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();

            println!("[process] Processing {} items...", items.len());

            // Simulate processing
            let processed: Vec<_> = items
                .iter()
                .map(|item| {
                    let mut processed = item.clone();
                    if let Some(obj) = processed.as_object_mut() {
                        let value = obj.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                        obj.insert("processed_value".to_string(), json!(value * 2));
                        obj.insert("status".to_string(), json!("processed"));
                    }
                    processed
                })
                .collect();

            println!("[process] Processed {} items (values doubled)", processed.len());

            Ok(NodeOutput::new()
                .with_update("processed", json!(processed))
                .with_update("step", json!("process_complete")))
        })
        // Step 3: Validate results
        .add_node_fn("validate", |ctx| async move {
            let processed =
                ctx.get("processed").and_then(|v| v.as_array()).cloned().unwrap_or_default();

            println!("[validate] Validating {} processed items...", processed.len());

            // Simulate validation
            let mut valid_count = 0;
            let validated: Vec<_> = processed
                .iter()
                .map(|item| {
                    let mut validated = item.clone();
                    if let Some(obj) = validated.as_object_mut() {
                        let value =
                            obj.get("processed_value").and_then(|v| v.as_i64()).unwrap_or(0);
                        let is_valid = value > 0 && value < 1000;
                        obj.insert("valid".to_string(), json!(is_valid));
                        if is_valid {
                            valid_count += 1;
                        }
                    }
                    validated
                })
                .collect();

            println!("[validate] {} of {} items passed validation", valid_count, validated.len());

            Ok(NodeOutput::new()
                .with_update("validated", json!(validated))
                .with_update("step", json!("validate_complete")))
        })
        // Step 4: Generate report
        .add_node_fn("report", |ctx| async move {
            let validated =
                ctx.get("validated").and_then(|v| v.as_array()).cloned().unwrap_or_default();

            println!("[report] Generating final report...");

            let valid_items: Vec<_> = validated
                .iter()
                .filter(|item| item.get("valid").and_then(|v| v.as_bool()).unwrap_or(false))
                .collect();

            let total_value: i64 = valid_items
                .iter()
                .filter_map(|item| item.get("processed_value").and_then(|v| v.as_i64()))
                .sum();

            let report = json!({
                "total_items": validated.len(),
                "valid_items": valid_items.len(),
                "total_processed_value": total_value,
                "status": "complete"
            });

            println!(
                "[report] Report: {} valid items, total value: {}",
                valid_items.len(),
                total_value
            );

            Ok(NodeOutput::new()
                .with_update("result", report)
                .with_update("step", json!("complete")))
        })
        // Define edges
        .add_edge(START, "fetch")
        .add_edge("fetch", "process")
        .add_edge("process", "validate")
        .add_edge("validate", "report")
        .add_edge("report", END)
        .compile()?
        .with_checkpointer_arc(checkpointer.clone());

    // ========== Part 1: Run workflow with checkpointing ==========
    println!("{}", "=".repeat(60));
    println!("PART 1: Running workflow with automatic checkpointing");
    println!("{}", "=".repeat(60));

    let thread_id = "data-pipeline-001";
    let result = graph.invoke(State::new(), ExecutionConfig::new(thread_id)).await?;

    println!("\nFinal result: {:?}", result.get("result"));

    // ========== Part 2: View checkpoint history ==========
    println!("\n{}", "=".repeat(60));
    println!("PART 2: Viewing checkpoint history (time travel)");
    println!("{}", "=".repeat(60));

    let checkpoints = checkpointer.list(thread_id).await?;
    println!("\nFound {} checkpoints for thread '{}':", checkpoints.len(), thread_id);

    for (i, cp) in checkpoints.iter().enumerate() {
        let step_name = cp.state.get("step").and_then(|v| v.as_str()).unwrap_or("initial");
        let items_count =
            cp.state.get("items").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        let processed_count =
            cp.state.get("processed").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);

        println!(
            "  {}. Step {} - {} | items: {}, processed: {} | ID: {}...{}",
            i + 1,
            cp.step,
            step_name,
            items_count,
            processed_count,
            &cp.checkpoint_id[..8],
            &cp.checkpoint_id[cp.checkpoint_id.len() - 4..]
        );
    }

    // ========== Part 3: Load specific checkpoint ==========
    println!("\n{}", "=".repeat(60));
    println!("PART 3: Loading a specific checkpoint");
    println!("{}", "=".repeat(60));

    if checkpoints.len() >= 2 {
        // Load the second checkpoint (after fetch)
        let checkpoint = &checkpoints[1];
        println!("\nLoading checkpoint: {}", checkpoint.checkpoint_id);

        if let Some(loaded) = checkpointer.load_by_id(&checkpoint.checkpoint_id).await? {
            println!("Checkpoint state at step {}:", loaded.step);
            println!(
                "  - Items: {:?}",
                loaded.state.get("items").and_then(|v| v.as_array()).map(|a| a.len())
            );
            println!("  - Processed: {:?}", loaded.state.get("processed"));
            println!("  - Step: {:?}", loaded.state.get("step"));
        }
    }

    // ========== Part 4: Resume from checkpoint (simulated failure recovery) ==========
    println!("\n{}", "=".repeat(60));
    println!("PART 4: Simulating failure recovery");
    println!("{}", "=".repeat(60));

    // Create a new workflow that might "fail"
    let failure_checkpointer = Arc::new(MemoryCheckpointer::new());

    let failure_graph = StateGraph::with_channels(&["counter", "status"])
        .add_node_fn("increment", |ctx| async move {
            let counter = ctx.get("counter").and_then(|v| v.as_i64()).unwrap_or(0);
            let new_counter = counter + 1;

            println!("[increment] Counter: {} -> {}", counter, new_counter);

            // Simulate failure at counter = 3
            if new_counter == 3 {
                println!("[increment] SIMULATED FAILURE at counter 3!");
                return Ok(NodeOutput::new()
                    .with_update("counter", json!(new_counter))
                    .with_update("status", json!("failed")));
            }

            Ok(NodeOutput::new()
                .with_update("counter", json!(new_counter))
                .with_update("status", json!("running")))
        })
        .add_edge(START, "increment")
        .add_conditional_edges(
            "increment",
            |state| {
                let counter = state.get("counter").and_then(|v| v.as_i64()).unwrap_or(0);
                let status = state.get("status").and_then(|v| v.as_str()).unwrap_or("");

                if status == "failed" {
                    END.to_string()
                } else if counter < 5 {
                    "increment".to_string()
                } else {
                    END.to_string()
                }
            },
            [("increment", "increment"), (END, END)],
        )
        .compile()?
        .with_checkpointer_arc(failure_checkpointer.clone())
        .with_recursion_limit(10);

    let recovery_thread = "recovery-test";

    // First run - will "fail" at counter 3
    println!("\nFirst run (will fail at 3):");
    let result = failure_graph.invoke(State::new(), ExecutionConfig::new(recovery_thread)).await?;
    println!("Stopped at counter: {}", result.get("counter").and_then(|v| v.as_i64()).unwrap_or(0));

    // Check saved state
    if let Some(state) = failure_graph.get_state(recovery_thread).await? {
        println!(
            "\nSaved state: counter = {}",
            state.get("counter").and_then(|v| v.as_i64()).unwrap_or(0)
        );
    }

    // "Fix the bug" and resume
    println!("\nResuming from checkpoint (bug fixed):");

    // Update status to allow continuation
    failure_graph.update_state(recovery_thread, [("status".to_string(), json!("running"))]).await?;

    // Resume
    let final_result =
        failure_graph.invoke(State::new(), ExecutionConfig::new(recovery_thread)).await?;
    println!(
        "Final counter: {}",
        final_result.get("counter").and_then(|v| v.as_i64()).unwrap_or(0)
    );

    // ========== Part 5: Multiple threads ==========
    println!("\n{}", "=".repeat(60));
    println!("PART 5: Multiple concurrent threads");
    println!("{}", "=".repeat(60));

    let multi_checkpointer = Arc::new(MemoryCheckpointer::new());

    let multi_graph = StateGraph::with_channels(&["user_id", "balance"])
        .add_node_fn("process_user", |ctx| async move {
            let user_id = ctx.get("user_id").and_then(|v| v.as_str()).unwrap_or("unknown");
            let balance = ctx.get("balance").and_then(|v| v.as_i64()).unwrap_or(0);

            println!("[process_user] Processing user {} with balance {}", user_id, balance);

            Ok(NodeOutput::new()
                .with_update("balance", json!(balance + 100))
                .with_update("processed", json!(true)))
        })
        .add_edge(START, "process_user")
        .add_edge("process_user", END)
        .compile()?
        .with_checkpointer_arc(multi_checkpointer.clone());

    // Process multiple users (threads)
    let users = vec![("user-alice", 500), ("user-bob", 300), ("user-charlie", 1000)];

    for (user_id, initial_balance) in &users {
        let mut input = State::new();
        input.insert("user_id".to_string(), json!(user_id));
        input.insert("balance".to_string(), json!(initial_balance));

        let result = multi_graph.invoke(input, ExecutionConfig::new(user_id)).await?;
        println!(
            "  {} final balance: {}",
            user_id,
            result.get("balance").and_then(|v| v.as_i64()).unwrap_or(0)
        );
    }

    // Show each thread's checkpoint
    println!("\nCheckpoints by thread:");
    for (user_id, _) in &users {
        let checkpoints = multi_checkpointer.list(user_id).await?;
        println!("  {}: {} checkpoint(s)", user_id, checkpoints.len());
    }

    println!("\n=== Complete ===");
    Ok(())
}
