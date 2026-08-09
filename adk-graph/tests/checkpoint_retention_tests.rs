//! Tests for checkpoint retention.
//!
//! A long-running thread accumulates one checkpoint per super-step. Without a
//! policy that grows without bound. The invariant that matters most is that the
//! newest checkpoint is never discarded, because it is the one a resume loads.

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer, RetentionPolicy};
use adk_graph::edge::{END, START};
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::{Checkpoint, State};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// A chain of `steps` nodes, so the run leaves that many checkpoints.
fn chain(
    steps: usize,
    retention: Option<RetentionPolicy>,
) -> (CompiledGraph, Arc<MemoryCheckpointer>) {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let mut graph = StateGraph::with_channels(&["count"]);
    for index in 0..steps {
        let name: &'static str = Box::leak(format!("step{index}").into_boxed_str());
        graph = graph.add_node_fn(name, move |ctx| async move {
            let current = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("count", json!(current + 1)))
        });
    }
    let mut graph = graph.add_edge(START, "step0");
    for index in 0..steps.saturating_sub(1) {
        let from: &'static str = Box::leak(format!("step{index}").into_boxed_str());
        let to: &'static str = Box::leak(format!("step{}", index + 1).into_boxed_str());
        graph = graph.add_edge(from, to);
    }
    let last: &'static str = Box::leak(format!("step{}", steps - 1).into_boxed_str());
    let compiled = graph
        .add_edge(last, END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(Arc::clone(&checkpointer) as Arc<dyn Checkpointer>);

    let compiled = match retention {
        Some(policy) => compiled.with_checkpoint_retention(policy),
        None => compiled,
    };
    (compiled, checkpointer)
}

#[tokio::test]
async fn without_a_policy_every_checkpoint_is_kept() {
    let (graph, checkpointer) = chain(6, None);
    graph.invoke(State::new(), ExecutionConfig::new("unlimited")).await.unwrap();

    let kept = checkpointer.list("unlimited").await.unwrap();
    assert!(kept.len() > 3, "the whole history is kept, found {}", kept.len());
}

#[tokio::test]
async fn a_count_policy_keeps_only_the_newest() {
    let (graph, checkpointer) = chain(6, Some(RetentionPolicy::keep_last(2)));
    graph.invoke(State::new(), ExecutionConfig::new("counted")).await.unwrap();

    let kept = checkpointer.list("counted").await.unwrap();
    assert_eq!(kept.len(), 2, "the policy holds the thread at two");
}

#[tokio::test]
async fn the_newest_checkpoint_survives_the_tightest_policy() {
    // keep_last(0) is raised to 1: discarding the newest would end the thread.
    let policy = RetentionPolicy::keep_last(0);
    assert_eq!(policy.max_per_thread, Some(1));

    let (graph, checkpointer) = chain(5, Some(policy));
    let state = graph.invoke(State::new(), ExecutionConfig::new("tightest")).await.unwrap();

    let kept = checkpointer.list("tightest").await.unwrap();
    assert_eq!(kept.len(), 1, "exactly one is left");
    assert_eq!(
        checkpointer.load("tightest").await.unwrap().map(|c| c.state.get("count").cloned()),
        Some(state.get("count").cloned()),
        "and it is the newest, holding the final state"
    );
}

#[tokio::test]
async fn a_pruned_thread_still_resumes() {
    // The point of keeping the newest: a paused run must still continue.
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["first", "second"])
        .add_node_fn("one", |_ctx| async move {
            Ok(NodeOutput::new().with_update("first", json!(true)))
        })
        .add_node_fn("two", |_ctx| async move {
            Ok(NodeOutput::new().with_update("second", json!(true)))
        })
        .add_edge(START, "one")
        .add_edge("one", "two")
        .add_edge("two", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(Arc::clone(&checkpointer) as Arc<dyn Checkpointer>)
        .with_checkpoint_retention(RetentionPolicy::keep_last(1))
        .with_interrupt_before(&["two"]);

    let first = graph.invoke(State::new(), ExecutionConfig::new("pruned-resume")).await;
    assert!(first.is_err(), "the first run pauses");
    assert_eq!(checkpointer.list("pruned-resume").await.unwrap().len(), 1);

    let state = graph
        .invoke(State::new(), ExecutionConfig::new("pruned-resume"))
        .await
        .expect("a pruned thread must still resume");
    assert_eq!(state.get("second"), Some(&json!(true)));
}

#[tokio::test]
async fn an_age_policy_discards_by_timestamp() {
    // Built by hand, because a real run's checkpoints are all seconds old.
    let checkpointer = MemoryCheckpointer::new();
    let old = {
        let mut c = Checkpoint::new("aged", State::new(), 0, vec![]);
        c.created_at = chrono::Utc::now() - chrono::Duration::hours(48);
        c
    };
    let recent = Checkpoint::new("aged", State::new(), 1, vec![]);
    checkpointer.save(&old).await.unwrap();
    checkpointer.save(&recent).await.unwrap();

    let removed = checkpointer
        .prune("aged", &RetentionPolicy::max_age(Duration::from_secs(3600)))
        .await
        .unwrap();
    assert_eq!(removed, 1, "the 48-hour-old one goes");
    assert_eq!(checkpointer.list("aged").await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_policy_never_leaves_a_thread_empty() {
    // Every checkpoint is older than the limit, and one is still kept.
    let checkpointer = MemoryCheckpointer::new();
    for step in 0..3 {
        let mut c = Checkpoint::new("all-old", State::new(), step, vec![]);
        c.created_at = chrono::Utc::now() - chrono::Duration::days(30);
        checkpointer.save(&c).await.unwrap();
    }

    checkpointer
        .prune("all-old", &RetentionPolicy::max_age(Duration::from_secs(60)))
        .await
        .unwrap();
    assert_eq!(
        checkpointer.list("all-old").await.unwrap().len(),
        1,
        "the newest survives even when the whole thread is past the limit"
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn the_sqlite_backend_prunes_the_same_set() {
    use adk_graph::checkpoint::SqliteCheckpointer;

    let checkpointer = SqliteCheckpointer::new("sqlite::memory:").await.unwrap();
    for step in 0..5 {
        checkpointer.save(&Checkpoint::new("sql", State::new(), step, vec![])).await.unwrap();
    }

    let removed = checkpointer.prune("sql", &RetentionPolicy::keep_last(2)).await.unwrap();
    assert_eq!(removed, 3, "five saved, two kept");
    assert_eq!(checkpointer.list("sql").await.unwrap().len(), 2);
    assert!(checkpointer.load("sql").await.unwrap().is_some(), "and the newest still loads");
}
