//! Pending writes, durability modes, and the single-writer rule, as a graph run
//! sees them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use adk_core::ToolEffect;
use adk_graph::checkpoint::{Checkpointer, Durability, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::{Checkpoint, State};
use adk_graph::stream::{StreamEvent, StreamMode};
use futures::StreamExt;
use serde_json::json;

/// Counts how often each parallel branch ran; `right` fails while `fail` is set.
struct Branches {
    left_runs: Arc<AtomicUsize>,
    right_runs: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
}

/// START fans out to `left` and `right`, which both feed `join`.
fn parallel_graph(checkpointer: Arc<MemoryCheckpointer>) -> (CompiledGraph, Branches) {
    let branches = Branches {
        left_runs: Arc::new(AtomicUsize::new(0)),
        right_runs: Arc::new(AtomicUsize::new(0)),
        fail: Arc::new(AtomicBool::new(true)),
    };
    let left_runs = Arc::clone(&branches.left_runs);
    let right_runs = Arc::clone(&branches.right_runs);
    let fail = Arc::clone(&branches.fail);
    let graph = StateGraph::with_channels(&["left", "right", "joined"])
        .add_node_fn("left", move |_ctx| {
            let runs = Arc::clone(&left_runs);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("left", json!("charged")))
            }
        })
        .add_node_fn("right", move |_ctx| {
            let runs = Arc::clone(&right_runs);
            let fail = Arc::clone(&fail);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                if fail.load(Ordering::SeqCst) {
                    return Err(GraphError::Other("upstream unavailable".to_string()));
                }
                Ok(NodeOutput::new().with_update("right", json!("emailed")))
            }
        })
        .add_node_fn("join", |ctx| async move {
            let joined = json!([ctx.get("left").cloned(), ctx.get("right").cloned()]);
            Ok(NodeOutput::new().with_update("joined", joined))
        })
        .add_edge(START, "left")
        .add_edge(START, "right")
        .add_edge("left", "join")
        .add_edge("right", "join")
        .add_edge("join", END)
        .compile()
        .expect("graph compiles")
        .with_checkpointer_arc(checkpointer);
    (graph, branches)
}

/// A failed parallel step keeps the successful branch's writes, and a resume
/// applies them instead of running that branch again.
#[tokio::test]
async fn a_failed_parallel_step_does_not_rerun_its_successful_branch() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let (graph, branches) = parallel_graph(Arc::clone(&checkpointer));

    let error = graph
        .invoke(State::new(), ExecutionConfig::new("pending"))
        .await
        .expect_err("the right branch fails");
    assert!(matches!(error, GraphError::NodeExecutionFailed { ref node, .. } if node == "right"));

    let saved = checkpointer.load("pending").await.unwrap().expect("the failed step is saved");
    assert_eq!(saved.step, 0, "the failed step is not advanced");
    assert_eq!(saved.pending_writes.keys().collect::<Vec<_>>(), vec!["left"]);
    assert_eq!(
        saved.pending_writes["left"].updates,
        [("left".to_string(), json!("charged"))].into()
    );
    assert_eq!(saved.state.get("left"), None, "pending writes are not applied yet");

    branches.fail.store(false, Ordering::SeqCst);
    let state = graph.invoke(State::new(), ExecutionConfig::new("pending")).await.unwrap();

    assert_eq!(branches.left_runs.load(Ordering::SeqCst), 1, "the successful branch ran once");
    assert_eq!(branches.right_runs.load(Ordering::SeqCst), 2, "the failed branch ran again");
    assert_eq!(state.get("joined"), Some(&json!(["charged", "emailed"])));
    let newest = checkpointer.load("pending").await.unwrap().unwrap();
    assert!(newest.pending_writes.is_empty(), "a completed step clears its pending writes");
}

/// The streaming paths keep pending writes the same way.
#[tokio::test]
async fn streaming_runs_keep_pending_writes() {
    for mode in [StreamMode::Values, StreamMode::Messages] {
        let checkpointer = Arc::new(MemoryCheckpointer::new());
        let (graph, branches) = parallel_graph(Arc::clone(&checkpointer));
        let thread = format!("stream-{mode:?}");

        let events: Vec<_> =
            graph.stream(State::new(), ExecutionConfig::new(&thread), mode).collect().await;
        assert!(events.iter().any(Result::is_err), "{mode:?}: the right branch fails");
        let saved = checkpointer.load(&thread).await.unwrap().expect("the failed step is saved");
        assert_eq!(saved.pending_writes.keys().collect::<Vec<_>>(), vec!["left"], "{mode:?}");
        assert_eq!(saved.state.get("left"), None, "{mode:?}: the step's state is not applied");

        branches.fail.store(false, Ordering::SeqCst);
        let events: Vec<_> =
            graph.stream(State::new(), ExecutionConfig::new(&thread), mode).collect().await;
        let done = events
            .into_iter()
            .map(|event| event.expect("the resumed run succeeds"))
            .find_map(|event| match event {
                StreamEvent::Done { state, .. } => Some(state),
                _ => None,
            })
            .expect("the run completes");
        assert_eq!(done.get("joined"), Some(&json!(["charged", "emailed"])), "{mode:?}");
        assert_eq!(branches.left_runs.load(Ordering::SeqCst), 1, "{mode:?}: left ran once");
    }
}

fn linear_graph(
    checkpointer: Arc<MemoryCheckpointer>,
    seen_at_c: Arc<std::sync::Mutex<Option<Checkpoint>>>,
) -> CompiledGraph {
    let probe = Arc::clone(&checkpointer);
    StateGraph::with_channels(&["a", "b", "c"])
        .add_node_fn("a", |_ctx| async { Ok(NodeOutput::new().with_update("a", json!(1))) })
        .add_node_fn("b", |_ctx| async { Ok(NodeOutput::new().with_update("b", json!(2))) })
        .add_node_fn("c", move |_ctx| {
            let probe = Arc::clone(&probe);
            let seen = Arc::clone(&seen_at_c);
            async move {
                *seen.lock().unwrap() = probe.load("durability").await?;
                Ok(NodeOutput::new().with_update("c", json!(3)))
            }
        })
        .add_edge(START, "a")
        .add_edge("a", "b")
        .add_edge("b", "c")
        .add_edge("c", END)
        .compile()
        .expect("graph compiles")
        .with_checkpointer_arc(checkpointer)
}

/// Each mode writes what its documentation says, and every mode ends durable.
#[tokio::test]
async fn durability_modes_write_as_documented() {
    for (durability, expected_versions, stored_before_c) in [
        (Durability::Sync, vec![1, 2, 3], Some(2)),
        (Durability::Async, vec![1, 2, 3], None),
        (Durability::Exit, vec![1], None),
    ] {
        let checkpointer = Arc::new(MemoryCheckpointer::new());
        let seen = Arc::new(std::sync::Mutex::new(None));
        let graph = linear_graph(Arc::clone(&checkpointer), Arc::clone(&seen));
        let config = ExecutionConfig::new("durability").with_durability(durability);
        let state = graph.invoke(State::new(), config).await.unwrap();

        assert_eq!(state.get("c"), Some(&json!(3)), "{durability:?}");
        let versions: Vec<u64> =
            checkpointer.list("durability").await.unwrap().iter().map(|c| c.version).collect();
        assert_eq!(versions, expected_versions, "{durability:?}");
        let newest = checkpointer.load("durability").await.unwrap().unwrap();
        assert!(newest.pending_nodes.is_empty(), "{durability:?}: the final step is stored");
        if let Some(version) = stored_before_c {
            let seen = seen.lock().unwrap().clone().expect("a checkpoint exists before c");
            assert_eq!(seen.version, version, "{durability:?}");
        }
        if durability == Durability::Exit {
            assert_eq!(*seen.lock().unwrap(), None, "Exit writes nothing mid-run");
        }
    }
}

/// A non-idempotent node is never reached with earlier steps unsaved.
#[tokio::test]
async fn a_non_idempotent_node_forces_earlier_checkpoints_out() {
    for durability in [Durability::Async, Durability::Exit] {
        let checkpointer = Arc::new(MemoryCheckpointer::new());
        let seen = Arc::new(std::sync::Mutex::new(None));
        let graph = linear_graph(Arc::clone(&checkpointer), Arc::clone(&seen))
            .with_node_effect("c", ToolEffect::NonIdempotent);
        let config = ExecutionConfig::new("durability").with_durability(durability);
        graph.invoke(State::new(), config).await.unwrap();

        let seen = seen.lock().unwrap().clone().expect("a checkpoint exists before c");
        assert_eq!(seen.pending_nodes, vec!["c".to_string()], "{durability:?}");
        assert_eq!(seen.state.get("b"), Some(&json!(2)), "{durability:?}");
    }
}

/// A second writer on a thread is refused rather than interleaved.
#[tokio::test]
async fn a_second_writer_on_a_thread_is_refused() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let started = Arc::new(tokio::sync::Notify::new());
    let (gate, signal) = (Arc::clone(&release), Arc::clone(&started));
    let graph = Arc::new(
        StateGraph::with_channels(&["value"])
            .add_node_fn("slow", move |_ctx| {
                let gate = Arc::clone(&gate);
                let signal = Arc::clone(&signal);
                async move {
                    signal.notify_one();
                    gate.notified().await;
                    Ok(NodeOutput::new().with_update("value", json!("late")))
                }
            })
            .add_edge(START, "slow")
            .add_edge("slow", END)
            .compile()
            .unwrap()
            .with_checkpointer_arc(Arc::clone(&checkpointer) as Arc<dyn Checkpointer>),
    );

    let runner = Arc::clone(&graph);
    let run = tokio::spawn(async move {
        runner.invoke(State::new(), ExecutionConfig::new("contended")).await
    });
    started.notified().await;
    // Another writer advances the thread while the run is mid-step.
    checkpointer.save(&Checkpoint::new("contended", State::new(), 0, vec![])).await.unwrap();
    release.notify_one();

    let error = run.await.unwrap().expect_err("the stale writer is refused");
    assert!(
        matches!(error, GraphError::CheckpointConflict { version: 1, head: 1, .. }),
        "got {error}"
    );
    let versions: Vec<u64> =
        checkpointer.list("contended").await.unwrap().iter().map(|c| c.version).collect();
    assert_eq!(versions, vec![1], "the refused run stored nothing");
}
