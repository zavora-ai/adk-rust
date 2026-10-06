//! What a paused run saves must be enough to finish it.
//!
//! Two gaps lost work across a pause:
//!
//! - A dynamic or tool-confirmation pause kept only the paused node in the
//!   frontier, so the successors of siblings that completed in the same
//!   super-step were never computed and never ran after the resume.
//! - Fan-in arrivals lived only in the executor, so a pause or crash between
//!   a join's predecessors forgot the earlier arrivals and the join never ran.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::stream::{StreamEvent, StreamMode};
use futures::StreamExt;
use serde_json::json;

#[derive(Default)]
struct Runs {
    a: AtomicUsize,
    after_a: AtomicUsize,
    b: AtomicUsize,
}

impl Runs {
    fn snapshot(&self) -> (usize, usize, usize) {
        (
            self.a.load(Ordering::SeqCst),
            self.after_a.load(Ordering::SeqCst),
            self.b.load(Ordering::SeqCst),
        )
    }
}

/// `a` and `b` run in parallel; `b` pauses until `approved` is set, and `a`
/// has a successor of its own.
fn fan_out_with_pause(runs: &Arc<Runs>, checkpointer: Arc<dyn Checkpointer>) -> CompiledGraph {
    let (for_a, for_after, for_b) = (Arc::clone(runs), Arc::clone(runs), Arc::clone(runs));
    StateGraph::with_channels(&["a_done", "after_a_done", "b_done", "approved"])
        .add_node_fn("a", move |_ctx| {
            let runs = Arc::clone(&for_a);
            async move {
                runs.a.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("a_done", json!(true)))
            }
        })
        .add_node_fn("after_a", move |_ctx| {
            let runs = Arc::clone(&for_after);
            async move {
                runs.after_a.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("after_a_done", json!(true)))
            }
        })
        .add_node_fn("b", move |ctx| {
            let runs = Arc::clone(&for_b);
            async move {
                runs.b.fetch_add(1, Ordering::SeqCst);
                if ctx.get("approved") != Some(&json!(true)) {
                    return Ok(NodeOutput::interrupt("approve b"));
                }
                Ok(NodeOutput::new().with_update("b_done", json!(true)))
            }
        })
        .add_edge(START, "a")
        .add_edge(START, "b")
        .add_edge("a", "after_a")
        .add_edge("after_a", END)
        .add_edge("b", END)
        .compile()
        .expect("compile")
        .with_checkpointer_arc(checkpointer)
}

fn approval() -> State {
    State::from([("approved".to_string(), json!(true))])
}

#[tokio::test]
async fn a_dynamic_pause_keeps_the_successors_of_completed_siblings() {
    let runs = Arc::new(Runs::default());
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = fan_out_with_pause(&runs, Arc::clone(&checkpointer) as Arc<dyn Checkpointer>);

    let first = graph.invoke(State::new(), ExecutionConfig::new("fan-out")).await;
    assert!(matches!(first, Err(GraphError::Interrupted(_))), "got {first:?}");

    let mut frontier = checkpointer.load("fan-out").await.unwrap().unwrap().pending_nodes;
    frontier.sort();
    assert_eq!(frontier, vec!["after_a".to_string(), "b".to_string()]);

    let state = graph.invoke(approval(), ExecutionConfig::new("fan-out")).await.unwrap();

    assert_eq!(runs.snapshot(), (1, 1, 2), "(a, after_a, b) executions");
    assert_eq!(
        (state.get("after_a_done"), state.get("b_done")),
        (Some(&json!(true)), Some(&json!(true)))
    );
}

#[tokio::test]
async fn a_streamed_dynamic_pause_keeps_the_successors_of_completed_siblings() {
    let runs = Arc::new(Runs::default());
    let checkpointer = Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>;
    let graph = fan_out_with_pause(&runs, checkpointer);

    let first: Vec<_> = graph
        .stream(State::new(), ExecutionConfig::new("fan-out-stream"), StreamMode::Updates)
        .collect()
        .await;
    assert!(first.iter().any(|event| matches!(event, Ok(StreamEvent::Interrupted { .. }))));

    let resumed: Vec<_> = graph
        .stream(approval(), ExecutionConfig::new("fan-out-stream"), StreamMode::Updates)
        .collect()
        .await;
    assert!(resumed.iter().any(|event| matches!(event, Ok(StreamEvent::Done { .. }))));

    assert_eq!(runs.snapshot(), (1, 1, 2), "(a, after_a, b) executions");
}

/// `short` reaches `join` one super-step before `long_b`, and the run pauses
/// in between, before `long_b`.
fn join_with_pause_between_arrivals(
    joins: &Arc<AtomicUsize>,
    checkpointer: Arc<dyn Checkpointer>,
) -> CompiledGraph {
    let counter = Arc::clone(joins);
    StateGraph::with_channels(&["short", "long", "joined", "note"])
        .add_node_fn("short", |_ctx| async {
            Ok(NodeOutput::new().with_update("short", json!("s")))
        })
        .add_node_fn("long_a", |_ctx| async {
            Ok(NodeOutput::new().with_update("long", json!("a")))
        })
        .add_node_fn("long_b", |_ctx| async {
            Ok(NodeOutput::new().with_update("long", json!("ab")))
        })
        .add_node_fn("join", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let short = ctx.get("short").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let long = ctx.get("long").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Ok(NodeOutput::new().with_update("joined", json!(format!("{short}+{long}"))))
            }
        })
        .add_edge(START, "short")
        .add_edge(START, "long_a")
        .add_edge("long_a", "long_b")
        .add_edge("short", "join")
        .add_edge("long_b", "join")
        .add_edge("join", END)
        .compile()
        .expect("compile")
        .with_checkpointer_arc(checkpointer)
        .with_interrupt_before(&["long_b"])
}

#[tokio::test]
async fn a_join_released_after_a_resume_counts_arrivals_from_before_the_pause() {
    let joins = Arc::new(AtomicUsize::new(0));
    let checkpointer = Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>;
    let graph = join_with_pause_between_arrivals(&joins, checkpointer);

    let first = graph.invoke(State::new(), ExecutionConfig::new("join")).await;
    assert!(matches!(first, Err(GraphError::Interrupted(_))), "got {first:?}");

    // A fresh executor, as after a process restart: only the checkpoint remains.
    let state = graph.invoke(State::new(), ExecutionConfig::new("join")).await.unwrap();

    assert_eq!(joins.load(Ordering::SeqCst), 1, "the join must run once after the resume");
    assert_eq!(state.get("joined"), Some(&json!("s+ab")));
}

/// Editing state while paused must not discard the arrivals or the answered gate.
#[tokio::test]
async fn update_state_while_paused_keeps_fan_in_arrivals() {
    let joins = Arc::new(AtomicUsize::new(0));
    let checkpointer = Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>;
    let graph = join_with_pause_between_arrivals(&joins, checkpointer);

    let first = graph.invoke(State::new(), ExecutionConfig::new("join-edit")).await;
    assert!(matches!(first, Err(GraphError::Interrupted(_))), "got {first:?}");

    graph
        .update_state("join-edit", [("note".to_string(), json!("reviewed"))])
        .await
        .expect("update state");
    let state = graph.invoke(State::new(), ExecutionConfig::new("join-edit")).await.unwrap();

    assert_eq!(joins.load(Ordering::SeqCst), 1);
    assert_eq!(
        (state.get("joined"), state.get("note")),
        (Some(&json!("s+ab")), Some(&json!("reviewed")))
    );
}
