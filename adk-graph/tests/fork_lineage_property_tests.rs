//! Forking never changes the source thread (spec 3.3 §12, developer track).

#![cfg(feature = "time-travel")]

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::time_travel::{ForkSpec, fork_thread};
use proptest::prelude::*;
use serde_json::{Value, json};
use std::sync::Arc;

fn value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<i64>().prop_map(Value::from),
        any::<bool>().prop_map(Value::from),
        "[a-z]{0,8}".prop_map(Value::from),
        Just(json!({ "nested": [1, 2] })),
    ]
}

/// A fork request: which checkpoint (by index, may be past the end), which keys to set
/// (some undeclared), whether to check against the schema, and the target thread.
fn fork_request() -> impl Strategy<Value = (usize, Vec<(usize, Value)>, bool, usize)> {
    (0usize..5, proptest::collection::vec((0usize..5, value()), 0..4), any::<bool>(), 0usize..3)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn forking_never_changes_the_source(
        steps in 2usize..5,
        requests in proptest::collection::vec(fork_request(), 1..5),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let keys = ["a", "b", "c", "d", "undeclared"];
            let checkpointer = Arc::new(MemoryCheckpointer::new());
            let mut graph = StateGraph::with_channels(&keys[..4]);
            for index in 0..steps {
                graph = graph.add_node_fn(&format!("s{index}"), move |_ctx| async move {
                    Ok(NodeOutput::new().with_update(["a", "b", "c", "d"][index % 4], json!(index)))
                });
                let from = if index == 0 { START.to_string() } else { format!("s{}", index - 1) };
                graph = graph.add_edge(&from, &format!("s{index}"));
            }
            let graph = graph
                .add_edge(&format!("s{}", steps - 1), END)
                .compile()
                .unwrap()
                .with_checkpointer_arc(checkpointer.clone());
            graph.invoke(State::new(), ExecutionConfig::new("source")).await.unwrap();
            let before = checkpointer.list("source").await.unwrap();

            for (at, edits, checked, target) in requests {
                let from = before.get(at).map_or_else(|| "missing".to_string(), |c| c.checkpoint_id.clone());
                let mut spec = ForkSpec::new(from, ["source", "fork-1", "fork-2"][target]);
                for (key, value) in edits {
                    spec = spec.set(keys[key], value);
                }
                let schema = checked.then(|| graph.schema());
                // Refused or written, a fork leaves the source as it was.
                let _ = fork_thread(checkpointer.as_ref(), spec, schema).await;
                prop_assert_eq!(&checkpointer.list("source").await.unwrap(), &before);
            }
            Ok::<(), TestCaseError>(())
        })?;
    }
}
