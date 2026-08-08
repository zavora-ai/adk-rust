//! Verifies `StateGraph::add_deferred_node_fn` provides a correct fan-in barrier:
//! with uneven-length parallel branches, the aggregator must run exactly ONCE,
//! after BOTH branches complete (not once per upstream).

use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::{DeferredNodeConfig, MergeStrategy};
use serde_json::json;

#[tokio::test]
async fn deferred_node_barriers_until_all_upstreams_done() {
    // Branch lengths differ: b -> join (1 hop), a -> a2 -> join (2 hops).
    // Without a barrier, `join` would be scheduled twice (after b, then after a2).
    let graph = StateGraph::with_channels(&["a", "a2", "b", "join_runs", "saw_both"])
        .add_node_fn("a", |_| async { Ok(NodeOutput::new().with_update("a", json!(true))) })
        .add_node_fn("a2", |_| async { Ok(NodeOutput::new().with_update("a2", json!(true))) })
        .add_node_fn("b", |_| async { Ok(NodeOutput::new().with_update("b", json!(true))) })
        .add_deferred_node_fn(
            "join",
            |ctx| async move {
                let runs = ctx.get("join_runs").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
                let saw_both = ctx.get("a2").is_some() && ctx.get("b").is_some();
                Ok(NodeOutput::new()
                    .with_update("join_runs", json!(runs))
                    .with_update("saw_both", json!(saw_both)))
            },
            DeferredNodeConfig {
                merge_strategy: MergeStrategy::Collect,
                fan_in_timeout: None,
                ..Default::default()
            },
        )
        .add_edge(START, "a")
        .add_edge(START, "b")
        .add_edge("a", "a2")
        .add_edge("a2", "join")
        .add_edge("b", "join")
        .add_edge("join", END)
        .compile()
        .unwrap();

    let result = graph.invoke(State::new(), ExecutionConfig::new("fan-in")).await.unwrap();

    // Ran exactly once, and that single run saw BOTH branches' results.
    assert_eq!(result.get("join_runs"), Some(&json!(1)), "aggregator must run exactly once");
    assert_eq!(result.get("saw_both"), Some(&json!(true)), "aggregator must see all branches");
}
