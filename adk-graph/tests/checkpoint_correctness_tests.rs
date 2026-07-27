//! Checkpoints must record what still has to run, and a streamed run must
//! checkpoint at all.
//!
//! Three defects motivated these tests:
//!
//! 1. `run` saved the checkpoint *before* advancing the frontier, so the saved
//!    `pending_nodes` were the nodes that had just finished. Resuming re-executed
//!    them, double-applying any non-idempotent update.
//! 2. `run_stream` never saved a checkpoint at all, and its interrupt path
//!    returned without saving one, so a streamed run could not be resumed and a
//!    streamed human-in-the-loop interrupt was unrecoverable.
//! 3. In `StreamMode::Messages` the executor drained `execute_stream` for events
//!    and then called `execute` again for state updates, running every node — and
//!    every agent behind an `AgentNode` — twice per super-step.

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::stream::StreamMode;
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A counter shared with node closures so executions can be counted.
fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

#[tokio::test]
async fn a_checkpoint_records_the_nodes_that_still_have_to_run() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("first", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(value + 1)))
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
        .with_checkpointer_arc(checkpointer.clone() as Arc<dyn Checkpointer>);

    let mut input = State::new();
    input.insert("value".to_string(), json!(0));
    let result = graph.invoke(input, ExecutionConfig::new("thread-order")).await.unwrap();
    assert_eq!(result.get("value"), Some(&json!(11)));

    // The final checkpoint must not name a node that already ran.
    let cp = checkpointer.load("thread-order").await.unwrap().expect("a checkpoint was saved");
    assert!(
        cp.pending_nodes.is_empty(),
        "a finished run must checkpoint an empty frontier, got {:?}",
        cp.pending_nodes
    );
}

#[tokio::test]
async fn resuming_a_finished_run_does_not_re_execute_it() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let runs = counter();
    let runs_in_node = runs.clone();

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("accumulate", move |ctx| {
            let runs = runs_in_node.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("value", json!(value + 1)))
            }
        })
        .add_edge(START, "accumulate")
        .add_edge("accumulate", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer.clone() as Arc<dyn Checkpointer>);

    let mut input = State::new();
    input.insert("value".to_string(), json!(0));
    let first = graph.invoke(input, ExecutionConfig::new("thread-resume")).await.unwrap();
    assert_eq!(first.get("value"), Some(&json!(1)));
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    // Re-invoking the same thread resumes from the terminal checkpoint. The node
    // has already run, so it must not run again and the value must not advance.
    let second = graph.invoke(State::new(), ExecutionConfig::new("thread-resume")).await.unwrap();
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "resuming a completed run re-executed the node, double-applying its update"
    );
    assert_eq!(second.get("value"), Some(&json!(1)));
}

#[tokio::test]
async fn a_streamed_run_saves_checkpoints() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("step", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(value + 5)))
        })
        .add_edge(START, "step")
        .add_edge("step", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer.clone() as Arc<dyn Checkpointer>);

    let mut input = State::new();
    input.insert("value".to_string(), json!(0));
    let stream = graph.stream(input, ExecutionConfig::new("thread-stream"), StreamMode::Values);
    let mut stream = std::pin::pin!(stream);
    while let Some(event) = stream.next().await {
        event.expect("the streamed run must not fail");
    }

    let cp = checkpointer
        .load("thread-stream")
        .await
        .unwrap()
        .expect("a streamed run must save a checkpoint so it can be resumed");
    assert_eq!(cp.state.get("value"), Some(&json!(5)));
}

#[tokio::test]
async fn messages_mode_executes_each_node_once() {
    let runs = counter();
    let runs_in_node = runs.clone();

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("once", move |ctx| {
            let runs = runs_in_node.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("value", json!(value + 1)))
            }
        })
        .add_edge(START, "once")
        .add_edge("once", END)
        .compile()
        .unwrap();

    let mut input = State::new();
    input.insert("value".to_string(), json!(0));
    let stream = graph.stream(input, ExecutionConfig::new("thread-messages"), StreamMode::Messages);
    let mut stream = std::pin::pin!(stream);
    while let Some(event) = stream.next().await {
        event.expect("the streamed run must not fail");
    }

    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "Messages mode ran the node more than once; every agent behind an \
         AgentNode would be billed twice"
    );
}

#[tokio::test]
async fn messages_mode_still_applies_state_updates() {
    // Taking updates from the stream must not lose them.
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("double", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(value * 2)))
        })
        .add_edge(START, "double")
        .add_edge("double", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer.clone() as Arc<dyn Checkpointer>);

    let mut input = State::new();
    input.insert("value".to_string(), json!(21));
    let stream = graph.stream(input, ExecutionConfig::new("thread-updates"), StreamMode::Messages);
    let mut done_state = None;
    let mut stream = std::pin::pin!(stream);
    while let Some(event) = stream.next().await {
        if let Ok(adk_graph::stream::StreamEvent::Done { state, .. }) =
            event.as_ref().map(|e| e.clone())
        {
            done_state = Some(state);
        }
        event.expect("the streamed run must not fail");
    }

    let state = done_state.expect("the stream must report completion");
    assert_eq!(state.get("value"), Some(&json!(42)));
}
