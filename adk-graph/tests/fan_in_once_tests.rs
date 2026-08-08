//! A fan-in node must run once per set of predecessor completions.
//!
//! The executor advances the frontier from whichever nodes finished in the last
//! super-step. A node with several incoming edges therefore became eligible as
//! soon as *any* one predecessor finished, so on branches of unequal length it
//! ran once per super-step in which a predecessor completed — applying its
//! updates more than once and reading state that was still incomplete.
//!
//! `mark_deferred` fixed this for a caller who knew to reach for it. Nothing in
//! the graph shape required it, so the default was wrong.

use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Two predecessors, one of them two hops away, so they complete in different
/// super-steps. The join must still run once.
#[tokio::test]
async fn a_join_runs_once_when_its_branches_are_unequal() {
    let joins = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&joins);

    let graph = StateGraph::with_channels(&["short", "long", "joined"])
        // Short branch: one hop.
        .add_node_fn("short", |_ctx| async move {
            Ok(NodeOutput::new().with_update("short", json!("s")))
        })
        // Long branch: two hops, so it lands a super-step later.
        .add_node_fn("long_a", |_ctx| async move {
            Ok(NodeOutput::new().with_update("long", json!("a")))
        })
        .add_node_fn("long_b", |_ctx| async move {
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
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("uneven")).await.unwrap();

    assert_eq!(
        joins.load(Ordering::SeqCst),
        1,
        "the join must wait for both branches, not run once per arriving predecessor"
    );
    assert_eq!(
        state.get("joined").and_then(|v| v.as_str()),
        Some("s+ab"),
        "the join must see the completed long branch, not its intermediate value"
    );
}

/// Equal-length branches already worked, and must keep working.
#[tokio::test]
async fn a_join_runs_once_when_its_branches_are_equal() {
    let joins = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&joins);

    let graph = StateGraph::with_channels(&["a", "b", "joined"])
        .add_node_fn(
            "first",
            |_ctx| async move { Ok(NodeOutput::new().with_update("a", json!(1))) },
        )
        .add_node_fn(
            "second",
            |_ctx| async move { Ok(NodeOutput::new().with_update("b", json!(2))) },
        )
        .add_node_fn("join", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let a = ctx.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
                let b = ctx.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("joined", json!(a + b)))
            }
        })
        .add_edge(START, "first")
        .add_edge(START, "second")
        .add_edge("first", "join")
        .add_edge("second", "join")
        .add_edge("join", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("even")).await.unwrap();

    assert_eq!(joins.load(Ordering::SeqCst), 1);
    assert_eq!(state.get("joined").and_then(|v| v.as_i64()), Some(3));
}

/// A single incoming edge is not a fan-in and must not be deferred.
///
/// Guards against the in-degree rule catching an ordinary chain, which would
/// change the behaviour of every linear graph.
#[tokio::test]
async fn a_node_with_one_incoming_edge_is_not_deferred() {
    let runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&runs);

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn(
            "first",
            |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(1))) },
        )
        .add_node_fn("second", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("value", json!(value + 1)))
            }
        })
        .add_edge(START, "first")
        .add_edge("first", "second")
        .add_edge("second", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("linear")).await.unwrap();

    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(state.get("value").and_then(|v| v.as_i64()), Some(2));
}

/// A join can release once a quorum of branches has answered.
///
/// `min_predecessors` decides how many arrivals release the node when
/// `fan_in_timeout` expires. Neither adk-python nor adk-go offers this: both
/// support wait-for-all only.
#[tokio::test]
async fn a_join_can_release_on_a_quorum() {
    use adk_graph::deferred::{DeferredNodeConfig, MergeStrategy};
    use std::time::Duration;

    let graph = StateGraph::with_channels(&["fast_a", "fast_b", "slow", "joined"])
        .add_node_fn("fast_a", |_ctx| async move {
            Ok(NodeOutput::new().with_update("fast_a", json!(1)))
        })
        .add_node_fn("fast_b", |_ctx| async move {
            Ok(NodeOutput::new().with_update("fast_b", json!(2)))
        })
        // Never answers within the timeout.
        .add_node_fn("slow", |_ctx| async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(NodeOutput::new().with_update("slow", json!(3)))
        })
        .add_node_fn("join", |ctx| async move {
            let a = ctx.get("fast_a").and_then(|v| v.as_i64()).unwrap_or(0);
            let b = ctx.get("fast_b").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("joined", json!(a + b)))
        })
        .add_edge(START, "fast_a")
        .add_edge(START, "fast_b")
        .add_edge("fast_a", "join")
        .add_edge("fast_b", "join")
        .add_edge("join", END)
        .mark_deferred(
            "join",
            DeferredNodeConfig {
                merge_strategy: MergeStrategy::MergeMap,
                fan_in_timeout: Some(Duration::from_millis(50)),
                min_predecessors: Some(2),
            },
        )
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("quorum")).await.unwrap();
    assert_eq!(state.get("joined").and_then(|v| v.as_i64()), Some(3));
}
