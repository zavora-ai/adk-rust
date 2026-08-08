//! Parallel state updates must apply in a deterministic order.
//!
//! A super-step dispatches its frontier through `buffer_unordered` and collected
//! each node's updates as its future resolved, then applied them in that order.
//! With a non-commutative reducer on a shared channel — `Append` builds an array,
//! so order is the result — the same graph and the same input produced different
//! state depending on which node happened to finish first.
//!
//! Timing decided the answer, so a run was not reproducible and a slow
//! dependency could silently reorder a log.

use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::{Reducer, State, StateSchema};
use serde_json::json;
use std::time::Duration;

/// Two nodes append to one channel. The one that sorts first finishes last, so
/// completion order and node order disagree.
///
/// The result must follow node order.
#[tokio::test]
async fn appends_apply_in_node_order_not_completion_order() {
    let schema = StateSchema::builder().channel_with_reducer("log", Reducer::Append).build();

    let graph = StateGraph::new(schema)
        // Sorts first, finishes last.
        .add_node_fn("alpha", |_ctx| async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            Ok(NodeOutput::new().with_update("log", json!("alpha")))
        })
        // Sorts last, finishes first.
        .add_node_fn("zulu", |_ctx| async move {
            Ok(NodeOutput::new().with_update("log", json!("zulu")))
        })
        .add_edge(START, "alpha")
        .add_edge(START, "zulu")
        .add_edge("alpha", END)
        .add_edge("zulu", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("order-1")).await.unwrap();

    assert_eq!(
        state.get("log"),
        Some(&json!(["alpha", "zulu"])),
        "updates must be applied in node order, not in the order the futures resolved"
    );
}

/// The same graph gives the same answer when the durations are reversed.
///
/// Together with the test above this pins that the result does not depend on
/// timing at all: both orderings of completion must produce one state.
#[tokio::test]
async fn the_result_is_the_same_when_completion_order_reverses() {
    async fn run(alpha_delay_ms: u64, zulu_delay_ms: u64) -> State {
        let schema = StateSchema::builder().channel_with_reducer("log", Reducer::Append).build();

        let graph = StateGraph::new(schema)
            .add_node_fn("alpha", move |_ctx| async move {
                tokio::time::sleep(Duration::from_millis(alpha_delay_ms)).await;
                Ok(NodeOutput::new().with_update("log", json!("alpha")))
            })
            .add_node_fn("zulu", move |_ctx| async move {
                tokio::time::sleep(Duration::from_millis(zulu_delay_ms)).await;
                Ok(NodeOutput::new().with_update("log", json!("zulu")))
            })
            .add_edge(START, "alpha")
            .add_edge(START, "zulu")
            .add_edge("alpha", END)
            .add_edge("zulu", END)
            .compile()
            .unwrap();

        graph.invoke(State::new(), ExecutionConfig::new("order-2")).await.unwrap()
    }

    let alpha_slow = run(60, 0).await;
    let zulu_slow = run(0, 60).await;

    assert_eq!(
        alpha_slow.get("log"),
        zulu_slow.get("log"),
        "reversing which node is slower must not change the state"
    );
    assert_eq!(alpha_slow.get("log"), Some(&json!(["alpha", "zulu"])));
}

/// The streamed path orders updates the same way.
///
/// `run_stream` applies each node's updates as it processes that node, so it has
/// its own ordering. It must agree with `invoke`, or the same graph would give
/// two answers depending on how it was run.
#[tokio::test]
async fn the_streamed_path_agrees_with_invoke() {
    use adk_graph::stream::StreamMode;
    use futures::StreamExt;

    let schema = StateSchema::builder().channel_with_reducer("log", Reducer::Append).build();

    let build = || {
        StateGraph::new(schema.clone())
            .add_node_fn("alpha", |_ctx| async move {
                tokio::time::sleep(Duration::from_millis(60)).await;
                Ok(NodeOutput::new().with_update("log", json!("alpha")))
            })
            .add_node_fn("zulu", |_ctx| async move {
                Ok(NodeOutput::new().with_update("log", json!("zulu")))
            })
            .add_edge(START, "alpha")
            .add_edge(START, "zulu")
            .add_edge("alpha", END)
            .add_edge("zulu", END)
            .compile()
            .unwrap()
            .with_checkpointer(adk_graph::checkpoint::MemoryCheckpointer::new())
    };

    let invoked = build().invoke(State::new(), ExecutionConfig::new("stream-a")).await.unwrap();

    let graph = build();
    let stream = graph.stream(State::new(), ExecutionConfig::new("stream-b"), StreamMode::Values);
    let mut stream = Box::pin(stream);
    while stream.next().await.is_some() {}
    let streamed = graph.get_state("stream-b").await.unwrap().unwrap_or_default();

    assert_eq!(
        invoked.get("log"),
        streamed.get("log"),
        "invoke and stream must order updates the same way"
    );
}

/// **Property 4: update application is timing-independent.**
/// *For any* set of node delays, the resulting state is identical.
/// **Validates: Requirements 4.1, 4.3**
#[test]
fn prop_state_does_not_depend_on_completion_order() {
    use proptest::prelude::*;

    proptest!(ProptestConfig::with_cases(32), |(delays in prop::collection::vec(0u64..40, 3..=4))| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");

        let state = runtime.block_on(async {
            let schema =
                StateSchema::builder().channel_with_reducer("log", Reducer::Append).build();
            let mut graph = StateGraph::new(schema);
            // Names are chosen so declaration order and sort order agree, which
            // makes the expected result easy to state.
            let names = ["n0", "n1", "n2", "n3"];
            for (index, delay) in delays.iter().enumerate() {
                let name = names[index];
                let delay = *delay;
                graph = graph.add_node_fn(name, move |_ctx| async move {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    Ok(NodeOutput::new().with_update("log", json!(name)))
                });
            }
            for name in names.iter().take(delays.len()) {
                graph = graph.add_edge(START, name).add_edge(name, END);
            }
            let compiled = graph.compile().expect("compile");
            compiled.invoke(State::new(), ExecutionConfig::new("prop")).await.expect("run")
        });

        let expected: Vec<_> =
            (0..delays.len()).map(|i| json!(["n0", "n1", "n2", "n3"][i])).collect();
        prop_assert_eq!(state.get("log"), Some(&json!(expected)));
    });
}
