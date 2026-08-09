//! Do interrupts and checkpoints work under every stream mode?
//!
//! `StreamMode::Messages` runs nodes through `execute_stream` in its own loop,
//! rather than through `execute_super_step`. Both interrupt checks live in
//! `execute_super_step`, so this asks whether that mode pauses at all.

use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::stream::{StreamEvent, StreamMode};
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A graph whose second node must not run before a person approves.
fn gated_graph(dynamic: bool, after_runs: Arc<AtomicUsize>) -> CompiledGraph {
    let counter = Arc::clone(&after_runs);
    let graph = StateGraph::with_channels(&["first", "after"])
        .add_node_fn("first", move |_ctx| async move {
            Ok(NodeOutput::new().with_update("first", json!(true)))
        })
        .add_node_fn("gated", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("after", json!(true)))
            }
        })
        .add_edge(START, "first")
        .add_edge("first", "gated")
        .add_edge("gated", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    if dynamic { graph } else { graph.with_interrupt_before(&["gated"]) }
}

/// Runs the graph in one mode and reports whether an interrupt was seen.
async fn drain(graph: &CompiledGraph, thread: &str, mode: StreamMode) -> bool {
    let mut stream = Box::pin(graph.stream(State::new(), ExecutionConfig::new(thread), mode));
    let mut interrupted = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamEvent::Interrupted { .. }) => interrupted = true,
            Ok(_) => {}
            Err(_) => interrupted = true,
        }
    }
    interrupted
}

#[tokio::test]
async fn a_static_interrupt_stops_a_values_stream() {
    // The control. If this fails the test is wrong, not the crate.
    let runs = Arc::new(AtomicUsize::new(0));
    let graph = gated_graph(false, Arc::clone(&runs));
    let saw = drain(&graph, "static-values", StreamMode::Values).await;
    assert!(saw, "Values mode must report the interrupt");
    assert_eq!(runs.load(Ordering::SeqCst), 0, "the gated node must not run");
}

#[tokio::test]
async fn a_static_interrupt_stops_a_messages_stream() {
    let runs = Arc::new(AtomicUsize::new(0));
    let graph = gated_graph(false, Arc::clone(&runs));
    let saw = drain(&graph, "static-messages", StreamMode::Messages).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "the gated node must not run before the gate is answered"
    );
    assert!(saw, "Messages mode must report the interrupt");
}

#[tokio::test]
async fn a_dynamic_interrupt_stops_a_messages_stream() {
    let runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&runs);
    let graph = StateGraph::with_channels(&["after"])
        .add_node_fn("asks", |_ctx| async move {
            Ok(NodeOutput::interrupt("approve before continuing"))
        })
        .add_node_fn("after", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("after", json!(true)))
            }
        })
        .add_edge(START, "asks")
        .add_edge("asks", "after")
        .add_edge("after", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    let saw = drain(&graph, "dynamic-messages", StreamMode::Messages).await;
    assert_eq!(runs.load(Ordering::SeqCst), 0, "a node must not run past a pause");
    assert!(saw, "Messages mode must report the dynamic interrupt");
}

#[tokio::test]
async fn a_messages_stream_writes_a_checkpoint() {
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn(
            "only",
            |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(7))) },
        )
        .add_edge(START, "only")
        .add_edge("only", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    let mut stream = Box::pin(graph.stream(
        State::new(),
        ExecutionConfig::new("messages-checkpoint"),
        StreamMode::Messages,
    ));
    while let Some(item) = stream.next().await {
        item.expect("the run must not fail");
    }

    let state = graph
        .get_state("messages-checkpoint")
        .await
        .expect("get_state must not error")
        .expect("a completed run must leave a checkpoint");
    assert_eq!(state.get("value"), Some(&json!(7)));
}

#[tokio::test]
async fn a_values_stream_writes_a_checkpoint() {
    // The control for the checkpoint question.
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn(
            "only",
            |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(7))) },
        )
        .add_edge(START, "only")
        .add_edge("only", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    let mut stream = Box::pin(graph.stream(
        State::new(),
        ExecutionConfig::new("values-checkpoint"),
        StreamMode::Values,
    ));
    while let Some(item) = stream.next().await {
        item.expect("the run must not fail");
    }

    let state = graph
        .get_state("values-checkpoint")
        .await
        .expect("get_state must not error")
        .expect("a completed run must leave a checkpoint");
    assert_eq!(state.get("value"), Some(&json!(7)));
}

#[tokio::test]
async fn a_paused_messages_run_resumes_past_the_gate() {
    // The checkpoint exists to make the pause resumable. Without the resume this
    // test would pass on a run that simply stopped forever.
    let runs = Arc::new(AtomicUsize::new(0));
    let graph = gated_graph(false, Arc::clone(&runs));

    let first = drain(&graph, "resume-messages", StreamMode::Messages).await;
    assert!(first, "the first run must pause");
    assert_eq!(runs.load(Ordering::SeqCst), 0, "the gated node has not run yet");

    let second = drain(&graph, "resume-messages", StreamMode::Messages).await;
    assert!(!second, "the second run must not pause again");
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the gated node runs exactly once, after the gate");

    let state = graph
        .get_state("resume-messages")
        .await
        .expect("get_state must not error")
        .expect("a checkpoint");
    assert_eq!(state.get("after"), Some(&json!(true)), "the resumed run finished the work");
}

#[tokio::test]
async fn a_messages_stream_reports_the_pause_to_its_caller() {
    let runs = Arc::new(AtomicUsize::new(0));
    let graph = gated_graph(false, Arc::clone(&runs));

    let mut stream = Box::pin(graph.stream(
        State::new(),
        ExecutionConfig::new("reports-messages"),
        StreamMode::Messages,
    ));
    let mut messages = Vec::new();
    while let Some(Ok(event)) = stream.next().await {
        if let StreamEvent::Interrupted { ref message, .. } = event {
            messages.push(message.clone());
        }
        // The node-level signal is internal and must not reach the caller.
        assert!(
            !matches!(event, StreamEvent::NodeInterrupt { .. }),
            "NodeInterrupt is the executor's own channel, not the caller's"
        );
    }
    assert_eq!(messages.len(), 1, "exactly one pause is reported");
    assert!(messages[0].contains("gated"), "the message names the gated node: {:?}", messages[0]);
}

/// `interrupt_after` gates the node that follows, because the gated node has
/// already applied its updates when the gate fires.
#[tokio::test]
async fn an_interrupt_after_stops_a_messages_stream() {
    let later_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&later_runs);

    let graph = StateGraph::with_channels(&["first", "later"])
        .add_node_fn("first", |_ctx| async move {
            Ok(NodeOutput::new().with_update("first", json!(true)))
        })
        .add_node_fn("later", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("later", json!(true)))
            }
        })
        .add_edge(START, "first")
        .add_edge("first", "later")
        .add_edge("later", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
        .with_interrupt_after(&["first"]);

    let saw = drain(&graph, "after-messages", StreamMode::Messages).await;
    assert!(saw, "Messages mode must report an interrupt_after pause");
    assert_eq!(later_runs.load(Ordering::SeqCst), 0, "the following node must not run yet");

    // And the pause is resumable: `After` resumes at the successors.
    let again = drain(&graph, "after-messages", StreamMode::Messages).await;
    assert!(!again, "the resumed run must not pause again");
    assert_eq!(later_runs.load(Ordering::SeqCst), 1, "the following node runs exactly once");
}
