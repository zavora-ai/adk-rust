//! Resuming a run that paused inside a subgraph.

use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::subgraph::SubgraphNode;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// An outer graph whose subgraph pauses at a static gate.
fn statically_gated(inner_runs: Arc<AtomicUsize>, after_runs: Arc<AtomicUsize>) -> CompiledGraph {
    let counter = Arc::clone(&inner_runs);
    let inner = Arc::new(
        StateGraph::with_channels(&["ticket", "handled"])
            .add_node_fn("act", move |_ctx| {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutput::new().with_update("handled", json!(true)))
                }
            })
            .add_edge(START, "act")
            .add_edge("act", END)
            .compile()
            .unwrap()
            .with_checkpointer(MemoryCheckpointer::new())
            .with_interrupt_before(&["act"]),
    );

    let counter = Arc::clone(&after_runs);
    StateGraph::with_channels(&["ticket", "handled", "after"])
        .add_node(SubgraphNode::new("inner", inner))
        .add_node_fn("after", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("after", json!(true)))
            }
        })
        .add_edge(START, "inner")
        .add_edge("inner", "after")
        .add_edge("after", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
}

#[tokio::test]
async fn a_run_paused_in_a_subgraph_resumes_past_the_inner_gate() {
    let inner_runs = Arc::new(AtomicUsize::new(0));
    let after_runs = Arc::new(AtomicUsize::new(0));
    let outer = statically_gated(Arc::clone(&inner_runs), Arc::clone(&after_runs));

    let first = outer.invoke(State::new(), ExecutionConfig::new("resume-sub")).await;
    assert!(matches!(first, Err(GraphError::Interrupted(_))), "the first run must pause");
    assert_eq!(inner_runs.load(Ordering::SeqCst), 0, "the gated inner node has not run");

    // A person approves, so the same thread runs again.
    let state = outer
        .invoke(State::new(), ExecutionConfig::new("resume-sub"))
        .await
        .expect("the resume must get past the inner gate");

    assert_eq!(inner_runs.load(Ordering::SeqCst), 1, "the inner node runs exactly once");
    assert_eq!(state.get("handled"), Some(&json!(true)), "its output reached the parent");
    assert_eq!(after_runs.load(Ordering::SeqCst), 1, "and the parent continued");
    assert_eq!(state.get("after"), Some(&json!(true)));
}

#[tokio::test]
async fn a_dynamic_pause_inside_a_subgraph_is_answered_by_state() {
    // A node inside decides for itself, so the answer arrives as state rather than
    // as a cleared gate.
    let inner_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&inner_runs);

    let inner = Arc::new(
        StateGraph::with_channels(&["approved", "handled"])
            .add_node_fn("act", move |ctx| {
                let counter = Arc::clone(&counter);
                async move {
                    if ctx.get("approved").and_then(|v| v.as_bool()) != Some(true) {
                        return Ok(NodeOutput::interrupt("approve this action"));
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutput::new().with_update("handled", json!(true)))
                }
            })
            .add_edge(START, "act")
            .add_edge("act", END)
            .compile()
            .unwrap()
            .with_checkpointer(MemoryCheckpointer::new()),
    );

    let outer = StateGraph::with_channels(&["approved", "handled"])
        .add_node(SubgraphNode::new("inner", inner))
        .add_edge(START, "inner")
        .add_edge("inner", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    let first = outer.invoke(State::new(), ExecutionConfig::new("dyn-sub")).await;
    assert!(matches!(first, Err(GraphError::Interrupted(_))), "the first run must pause");
    assert_eq!(inner_runs.load(Ordering::SeqCst), 0);

    let mut approval = State::new();
    approval.insert("approved".to_string(), json!(true));
    let state = outer
        .invoke(approval, ExecutionConfig::new("dyn-sub"))
        .await
        .expect("the decision must reach the inner node");

    assert_eq!(inner_runs.load(Ordering::SeqCst), 1);
    assert_eq!(state.get("handled"), Some(&json!(true)));
}

#[tokio::test]
async fn work_done_before_an_inner_pause_is_not_repeated() {
    // The inner graph did one step, then paused at the next. The resume must not
    // pay for the first step again.
    let first_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&first_runs);

    let inner = Arc::new(
        StateGraph::with_channels(&["cost", "handled"])
            .add_node_fn("expensive", move |_ctx| {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutput::new().with_update("cost", json!(1)))
                }
            })
            .add_node_fn("gated", |_ctx| async move {
                Ok(NodeOutput::new().with_update("handled", json!(true)))
            })
            .add_edge(START, "expensive")
            .add_edge("expensive", "gated")
            .add_edge("gated", END)
            .compile()
            .unwrap()
            .with_checkpointer(MemoryCheckpointer::new())
            .with_interrupt_before(&["gated"]),
    );

    let outer = StateGraph::with_channels(&["cost", "handled"])
        .add_node(SubgraphNode::new("inner", inner))
        .add_edge(START, "inner")
        .add_edge("inner", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    let first = outer.invoke(State::new(), ExecutionConfig::new("no-repeat")).await;
    assert!(matches!(first, Err(GraphError::Interrupted(_))));
    assert_eq!(first_runs.load(Ordering::SeqCst), 1, "the expensive node ran once");

    let state = outer
        .invoke(State::new(), ExecutionConfig::new("no-repeat"))
        .await
        .expect("the resume must complete");
    assert_eq!(first_runs.load(Ordering::SeqCst), 1, "and must not run again on the resume");
    assert_eq!(state.get("handled"), Some(&json!(true)));
}

#[tokio::test]
async fn a_subgraph_that_can_pause_but_not_checkpoint_fails_at_compile_time() {
    // It would re-enter at its first node on resume and repeat finished work.
    let inner = Arc::new(
        StateGraph::with_channels(&["ticket", "handled"])
            .add_node_fn("act", |_ctx| async move {
                Ok(NodeOutput::new().with_update("handled", json!(true)))
            })
            .add_edge(START, "act")
            .add_edge("act", END)
            .compile()
            .unwrap()
            // A gate, and deliberately no checkpointer.
            .with_interrupt_before(&["act"]),
    );

    let built = StateGraph::with_channels(&["ticket", "handled"])
        .add_node(SubgraphNode::new("inner", inner))
        .add_edge(START, "inner")
        .add_edge("inner", END)
        .compile();

    let error = match built {
        Err(error) => error,
        Ok(_) => panic!("a subgraph that cannot resume its own pause must fail compilation"),
    };
    assert!(
        matches!(error, GraphError::InvalidGraph(ref m) if m.contains("no checkpointer")),
        "expected the missing checkpointer to be named, got {error:?}"
    );
}

#[tokio::test]
async fn a_pause_two_subgraphs_deep_resumes() {
    let deepest_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&deepest_runs);

    let deepest = Arc::new(
        StateGraph::with_channels(&["ticket", "handled"])
            .add_node_fn("act", move |_ctx| {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutput::new().with_update("handled", json!(true)))
                }
            })
            .add_edge(START, "act")
            .add_edge("act", END)
            .compile()
            .unwrap()
            .with_checkpointer(MemoryCheckpointer::new())
            .with_interrupt_before(&["act"]),
    );

    let middle = Arc::new(
        StateGraph::with_channels(&["ticket", "handled"])
            .add_node(SubgraphNode::new("deepest", deepest))
            .add_edge(START, "deepest")
            .add_edge("deepest", END)
            .compile()
            .unwrap()
            .with_checkpointer(MemoryCheckpointer::new()),
    );

    let outer = StateGraph::with_channels(&["ticket", "handled"])
        .add_node(SubgraphNode::new("middle", middle))
        .add_edge(START, "middle")
        .add_edge("middle", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    let first = outer.invoke(State::new(), ExecutionConfig::new("deep")).await;
    let message = match first {
        Err(GraphError::Interrupted(interrupted)) => interrupted.interrupt.to_string(),
        other => panic!("the first run must pause, got {other:?}"),
    };
    // Each level prefixes its own node name, so the message says how deep it is.
    assert!(message.contains("middle"), "names the outer subgraph: {message}");
    assert!(message.contains("deepest"), "and the inner one: {message}");
    assert_eq!(deepest_runs.load(Ordering::SeqCst), 0);

    let state = outer
        .invoke(State::new(), ExecutionConfig::new("deep"))
        .await
        .expect("a pause two levels down must resume");
    assert_eq!(deepest_runs.load(Ordering::SeqCst), 1, "the deepest node runs exactly once");
    assert_eq!(state.get("handled"), Some(&json!(true)), "and its output reached the top");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_nested_pause_survives_a_new_graph_instance() {
    // What durability means: the resume runs on graphs built fresh, as a restarted
    // process would build them. Neither Google SDK persists graph state at all.
    use adk_graph::checkpoint::SqliteCheckpointer;

    let dir = std::env::temp_dir().join(format!("adk-nested-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let inner_db = dir.join("inner.db");
    let outer_db = dir.join("outer.db");

    let runs = Arc::new(AtomicUsize::new(0));

    // Built twice, standing in for two process lifetimes.
    async fn build(
        runs: Arc<AtomicUsize>,
        inner_db: std::path::PathBuf,
        outer_db: std::path::PathBuf,
    ) -> CompiledGraph {
        let counter = Arc::clone(&runs);
        let inner = Arc::new(
            StateGraph::with_channels(&["ticket", "handled"])
                .add_node_fn("act", move |_ctx| {
                    let counter = Arc::clone(&counter);
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(NodeOutput::new().with_update("handled", json!(true)))
                    }
                })
                .add_edge(START, "act")
                .add_edge("act", END)
                .compile()
                .unwrap()
                .with_checkpointer(
                    SqliteCheckpointer::new(&format!("sqlite:{}?mode=rwc", inner_db.display()))
                        .await
                        .unwrap(),
                )
                .with_interrupt_before(&["act"]),
        );
        StateGraph::with_channels(&["ticket", "handled"])
            .add_node(SubgraphNode::new("inner", inner))
            .add_edge(START, "inner")
            .add_edge("inner", END)
            .compile()
            .unwrap()
            .with_checkpointer(
                SqliteCheckpointer::new(&format!("sqlite:{}?mode=rwc", outer_db.display()))
                    .await
                    .unwrap(),
            )
    }

    let first_instance = build(Arc::clone(&runs), inner_db.clone(), outer_db.clone()).await;
    let first = first_instance.invoke(State::new(), ExecutionConfig::new("durable")).await;
    assert!(matches!(first, Err(GraphError::Interrupted(_))), "the first run must pause");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    drop(first_instance);

    // A second instance, sharing only the database files.
    let second_instance = build(Arc::clone(&runs), inner_db.clone(), outer_db.clone()).await;
    let state = second_instance
        .invoke(State::new(), ExecutionConfig::new("durable"))
        .await
        .expect("a fresh instance must resume from the databases alone");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(state.get("handled"), Some(&json!(true)));

    let _ = std::fs::remove_dir_all(&dir);
}
