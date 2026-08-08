//! Human approval with a static interrupt.
//!
//! `interrupt_before` pauses the run before a named node so a person can decide
//! whether it proceeds. The run stops with `GraphError::Interrupted`, the state is
//! checkpointed, and invoking the same thread again continues past the gate.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p adk-graph --example static_interrupt_approval
//! ```

use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let graph = StateGraph::with_channels(&["amount", "status"])
        .add_node_fn("prepare", |_ctx| async move {
            println!("  prepare: drafting a refund of 250");
            Ok(NodeOutput::new()
                .with_update("amount", json!(250))
                .with_update("status", json!("awaiting approval")))
        })
        .add_node_fn("refund", |ctx| async move {
            let amount = ctx.get("amount").and_then(|v| v.as_i64()).unwrap_or(0);
            println!("  refund: issuing {amount}");
            Ok(NodeOutput::new().with_update("status", json!("refunded")))
        })
        .add_edge(START, "prepare")
        .add_edge("prepare", "refund")
        .add_edge("refund", END)
        .compile()?
        .with_checkpointer(MemoryCheckpointer::new())
        // `refund` moves money, so it waits for a decision.
        .with_interrupt_before(&["refund"]);

    let thread = "refund-4821";

    println!("\nFirst run — expected to stop at the gate:");
    match graph.invoke(State::new(), ExecutionConfig::new(thread)).await {
        Err(GraphError::Interrupted(interrupted)) => {
            println!("  paused before: {:?}", interrupted.interrupt);
            println!("  state now:     {:?}", interrupted.state.get("status"));
            println!("  checkpoint:    {}", interrupted.checkpoint_id);
        }
        Ok(_) => {
            return Err("the gate should have stopped this run".into());
        }
        Err(other) => return Err(other.into()),
    }

    println!("\nA person approves, so the same thread is invoked again:");
    let state = graph.invoke(State::new(), ExecutionConfig::new(thread)).await?;
    println!("  status:        {:?}", state.get("status"));
    println!("\nThe gate is answered once. A cycle returning to `refund` would ask again.");
    Ok(())
}
