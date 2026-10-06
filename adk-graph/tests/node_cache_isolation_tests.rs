//! A node-cache hit must behave like the node running: its updates join the
//! step's batch, and the key a miss stores is the key the next lookup computes.
//!
//! Hits were applied to the live state before uncached siblings read it, so a
//! sibling saw a result from its own super-step, and a miss in the same step
//! stored its result under a key computed from that changed state, which no
//! later lookup could match.

#![cfg(feature = "node-cache")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_graph::agent::GraphAgent;
use adk_graph::cache::{CacheBackend, NodeCachePolicy};
use adk_graph::edge::{END, START};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;

#[tokio::test]
async fn a_mixed_hit_and_miss_step_keeps_isolation_and_stores_a_matching_key() {
    let hit_runs = Arc::new(AtomicUsize::new(0));
    let miss_runs = Arc::new(AtomicUsize::new(0));
    let (hits, misses) = (Arc::clone(&hit_runs), Arc::clone(&miss_runs));

    let agent = GraphAgent::builder("cached")
        .channels(&["x", "roomy_out", "tight_saw"])
        .node_fn("roomy", move |ctx| {
            let hits = Arc::clone(&hits);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let x = ctx.get("x").cloned().unwrap_or_default();
                Ok(NodeOutput::new().with_update("roomy_out", json!({"from": x})))
            }
        })
        .node_fn("tight", move |ctx| {
            let misses = Arc::clone(&misses);
            async move {
                misses.fetch_add(1, Ordering::SeqCst);
                let saw = ctx.get("roomy_out").cloned().unwrap_or_default();
                Ok(NodeOutput::new().with_update("tight_saw", saw))
            }
        })
        .edge(START, "roomy")
        .edge(START, "tight")
        .edge("roomy", END)
        .edge("tight", END)
        // `roomy` keeps every entry; `tight` keeps one, so a second input evicts
        // the first and the next run with the first input hits one and misses the other.
        .node_cache(
            "roomy",
            NodeCachePolicy { backend: CacheBackend::InMemory { max_entries: 16 }, ttl: None },
        )
        .node_cache(
            "tight",
            NodeCachePolicy { backend: CacheBackend::InMemory { max_entries: 1 }, ttl: None },
        )
        .build()
        .expect("build");

    let input = |x: i64| State::from([("x".to_string(), json!(x))]);
    agent.invoke(input(1), ExecutionConfig::new("run-1")).await.unwrap();
    agent.invoke(input(2), ExecutionConfig::new("run-2")).await.unwrap();
    assert_eq!((hit_runs.load(Ordering::SeqCst), miss_runs.load(Ordering::SeqCst)), (2, 2));

    // `roomy` hits and `tight` misses in the same super-step.
    let mixed = agent.invoke(input(1), ExecutionConfig::new("run-3")).await.unwrap();
    assert_eq!((hit_runs.load(Ordering::SeqCst), miss_runs.load(Ordering::SeqCst)), (2, 3));
    assert_eq!(
        (mixed.get("roomy_out"), mixed.get("tight_saw")),
        (Some(&json!({"from": 1})), Some(&json!(null))),
        "`tight` must read the pre-step state, not `roomy`'s cached result"
    );

    // The key `tight` stored on its miss is the key this identical run looks up.
    let repeat = agent.invoke(input(1), ExecutionConfig::new("run-4")).await.unwrap();
    assert_eq!((hit_runs.load(Ordering::SeqCst), miss_runs.load(Ordering::SeqCst)), (2, 3));
    assert_eq!(repeat, mixed);
}
