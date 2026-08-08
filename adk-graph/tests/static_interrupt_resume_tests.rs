//! A static interrupt must be a pause, not a permanent stop.
//!
//! `interrupt_before` is documented as human-in-the-loop support, but the check
//! in `execute_super_step` was a pure function of the frontier: it raised the
//! interrupt before executing anything, and the interrupt handler checkpointed
//! that same frontier. Resuming therefore reached the identical conclusion and
//! raised the interrupt again, so the node never ran however many times a caller
//! resumed.
//!
//! Nothing covered it. Before these tests, `git grep interrupt_before` across
//! `adk-graph/tests/` and `examples/` returned nothing.

use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, Router, START};
use adk_graph::error::GraphError;
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Runs the graph, then resumes up to `max_resumes` times, returning how many
/// interrupts were raised in total.
///
/// A caller that must resume more times than there are interrupt sites is stuck,
/// which is the defect these tests pin.
async fn run_then_resume(
    graph: &adk_graph::graph::CompiledGraph,
    thread: &str,
    max_resumes: usize,
) -> (usize, Option<State>) {
    let mut interrupts = 0;
    let mut input = State::new();
    for _ in 0..=max_resumes {
        match graph.invoke(input.clone(), ExecutionConfig::new(thread)).await {
            Ok(state) => return (interrupts, Some(state)),
            Err(GraphError::Interrupted(_)) => {
                interrupts += 1;
                // Resume carries no new input; the checkpoint holds the state.
                input = State::new();
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
    (interrupts, None)
}

/// The node behind a static interrupt executes once the run is resumed.
#[tokio::test]
async fn a_static_interrupt_can_be_resumed_past() {
    let gated_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&gated_runs);

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn(
            "open",
            |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(1))) },
        )
        .add_node_fn("gated", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("value", json!(value + 10)))
            }
        })
        .add_edge(START, "open")
        .add_edge("open", "gated")
        .add_edge("gated", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
        .with_interrupt_before(&["gated"]);

    let (interrupts, final_state) = run_then_resume(&graph, "resume-past", 3).await;

    assert_eq!(interrupts, 1, "the interrupt must be raised once, not on every resume");
    assert_eq!(
        gated_runs.load(Ordering::SeqCst),
        1,
        "the gated node must execute after the resume"
    );
    let state = final_state.expect("the run must complete after one resume");
    assert_eq!(state.get("value").and_then(|v| v.as_i64()), Some(11));
}

/// Resuming past one static interrupt does not suppress a later one.
#[tokio::test]
async fn resuming_past_one_interrupt_leaves_the_next_armed() {
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn(
            "first",
            |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(1))) },
        )
        .add_node_fn("second", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(value + 1)))
        })
        .add_edge(START, "first")
        .add_edge("first", "second")
        .add_edge("second", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
        .with_interrupt_before(&["first", "second"]);

    let (interrupts, final_state) = run_then_resume(&graph, "two-gates", 4).await;

    assert_eq!(interrupts, 2, "both gates must fire exactly once");
    let state = final_state.expect("the run must complete after two resumes");
    assert_eq!(state.get("value").and_then(|v| v.as_i64()), Some(2));
}

/// A static interrupt inside a cycle fires each time the node is scheduled.
///
/// The cleared marker applies to one arrival, not to the node forever, so a loop
/// that returns to an approval gate asks again.
#[tokio::test]
async fn an_interrupt_inside_a_cycle_fires_on_every_arrival() {
    let graph = StateGraph::with_channels(&["count"])
        .add_node_fn("tick", |ctx| async move {
            let count = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("count", json!(count + 1)))
        })
        .add_node_fn("gate", |_ctx| async move { Ok(NodeOutput::new()) })
        .add_edge(START, "tick")
        .add_edge("tick", "gate")
        .add_conditional_edges(
            "gate",
            Router::custom(|state: &State| {
                let count = state.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                if count >= 3 { END.to_string() } else { "tick".to_string() }
            }),
            [("tick", "tick"), (END, END)],
        )
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
        .with_interrupt_before(&["gate"]);

    let (interrupts, final_state) = run_then_resume(&graph, "cyclic-gate", 8).await;

    assert_eq!(interrupts, 3, "the gate is scheduled three times, so it asks three times");
    let state = final_state.expect("the run must complete");
    assert_eq!(state.get("count").and_then(|v| v.as_i64()), Some(3));
}

/// The gate marker survives the durable backend.
///
/// `MemoryCheckpointer` keeps a `Checkpoint` in memory, so a new field works
/// there whether or not it is persisted. The SQLite backend serializes column by
/// column, so the marker needed its own column — without it a resume against the
/// durable checkpointer would still stop at the gate forever.
///
/// This is also the first test in this crate to exercise `SqliteCheckpointer` at
/// all.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_static_interrupt_can_be_resumed_past_with_sqlite() {
    use adk_graph::checkpoint::SqliteCheckpointer;

    let gated_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&gated_runs);

    let checkpointer = SqliteCheckpointer::new("sqlite::memory:")
        .await
        .expect("open an in-memory sqlite checkpointer");

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn(
            "open",
            |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(1))) },
        )
        .add_node_fn("gated", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("value", json!(value + 10)))
            }
        })
        .add_edge(START, "open")
        .add_edge("open", "gated")
        .add_edge("gated", END)
        .compile()
        .unwrap()
        .with_checkpointer(checkpointer)
        .with_interrupt_before(&["gated"]);

    let (interrupts, final_state) = run_then_resume(&graph, "sqlite-resume", 3).await;

    assert_eq!(interrupts, 1, "the marker must survive the sqlite round trip");
    assert_eq!(gated_runs.load(Ordering::SeqCst), 1);
    let state = final_state.expect("the run must complete after one resume");
    assert_eq!(state.get("value").and_then(|v| v.as_i64()), Some(11));
}

/// `interrupt_after` fires once and the run then completes.
///
/// It has the opposite timing to `interrupt_before`: the node has already
/// produced its updates, so the checkpoint must record the advanced frontier and
/// no marker is needed. This test exists to pin that the two do not share a
/// mechanism by accident.
#[tokio::test]
async fn an_interrupt_after_a_node_can_be_resumed_past() {
    let first_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&first_runs);

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("first", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("value", json!(1)))
            }
        })
        .add_node_fn("second", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(value + 10)))
        })
        .add_edge(START, "first")
        .add_edge("first", "second")
        .add_edge("second", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
        .with_interrupt_after(&["first"]);

    let (interrupts, final_state) = run_then_resume(&graph, "after-gate", 3).await;

    assert_eq!(interrupts, 1, "the gate must fire once");
    assert_eq!(
        first_runs.load(Ordering::SeqCst),
        1,
        "the node must not run again on resume: it already applied its update"
    );
    let state = final_state.expect("the run must complete after one resume");
    assert_eq!(state.get("value").and_then(|v| v.as_i64()), Some(11));
}
