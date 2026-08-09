//! Tests for one graph running as a node of another.

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

/// An inner graph that measures the text it is given.
fn measuring_graph() -> Arc<CompiledGraph> {
    Arc::new(
        StateGraph::with_channels(&["text", "length"])
            .add_node_fn("measure", |ctx| async move {
                let text = ctx.get("text").and_then(|v| v.as_str()).unwrap_or("");
                Ok(NodeOutput::new().with_update("length", json!(text.len())))
            })
            .add_edge(START, "measure")
            .add_edge("measure", END)
            .compile()
            .unwrap(),
    )
}

#[tokio::test]
async fn a_subgraph_runs_and_returns_a_mapped_channel() {
    let outer = StateGraph::with_channels(&["document", "size"])
        .add_node(
            SubgraphNode::new("measure_doc", measuring_graph())
                .with_input("document", "text")
                .with_output("length", "size"),
        )
        .add_edge(START, "measure_doc")
        .add_edge("measure_doc", END)
        .compile()
        .unwrap();

    let mut input = State::new();
    input.insert("document".to_string(), json!("hello"));
    let state = outer.invoke(input, ExecutionConfig::new("sub-1")).await.unwrap();

    assert_eq!(state.get("size"), Some(&json!(5)));
    // The subgraph's own channels do not leak into the parent.
    assert_eq!(state.get("text"), None);
    assert_eq!(state.get("length"), None);
}

#[tokio::test]
async fn channels_shared_by_name_pass_through_without_being_mapped() {
    let inner = Arc::new(
        StateGraph::with_channels(&["shared", "doubled"])
            .add_node_fn("double", |ctx| async move {
                let value = ctx.get("shared").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("doubled", json!(value * 2)))
            })
            .add_edge(START, "double")
            .add_edge("double", END)
            .compile()
            .unwrap(),
    );

    let outer = StateGraph::with_channels(&["shared", "doubled"])
        .add_node(SubgraphNode::new("doubler", inner))
        .add_edge(START, "doubler")
        .add_edge("doubler", END)
        .compile()
        .unwrap();

    let mut input = State::new();
    input.insert("shared".to_string(), json!(21));
    let state = outer.invoke(input, ExecutionConfig::new("sub-shared")).await.unwrap();
    assert_eq!(state.get("doubled"), Some(&json!(42)));
}

#[tokio::test]
async fn an_isolated_subgraph_exchanges_only_what_it_names() {
    let inner = Arc::new(
        StateGraph::with_channels(&["shared", "doubled"])
            .add_node_fn("double", |ctx| async move {
                // `shared` is not fed in, so this sees nothing and writes 0.
                let value = ctx.get("shared").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("doubled", json!(value * 2)))
            })
            .add_edge(START, "double")
            .add_edge("double", END)
            .compile()
            .unwrap(),
    );

    let outer = StateGraph::with_channels(&["shared", "doubled"])
        .add_node(SubgraphNode::new("doubler", inner).isolated().with_output("doubled", "doubled"))
        .add_edge(START, "doubler")
        .add_edge("doubler", END)
        .compile()
        .unwrap();

    let mut input = State::new();
    input.insert("shared".to_string(), json!(21));
    let state = outer.invoke(input, ExecutionConfig::new("sub-iso")).await.unwrap();
    assert_eq!(
        state.get("doubled"),
        Some(&json!(0)),
        "isolating means `shared` was not fed in, so the subgraph saw nothing"
    );
}

#[tokio::test]
async fn a_mapping_naming_a_channel_the_parent_lacks_fails_at_compile_time() {
    // The Rust advantage: this never reaches a run.
    let error = StateGraph::with_channels(&["document"])
        .add_node(
            SubgraphNode::new("measure_doc", measuring_graph())
                .with_input("absent_in_parent", "text")
                .with_output("length", "document"),
        )
        .add_edge(START, "measure_doc")
        .add_edge("measure_doc", END)
        .compile();
    let error = match error {
        Err(error) => error,
        Ok(_) => panic!("a channel the parent does not declare must fail compilation"),
    };

    match error {
        GraphError::SubgraphChannelMismatch { subgraph, channel, side } => {
            assert_eq!(subgraph, "measure_doc");
            assert_eq!(channel, "absent_in_parent");
            assert_eq!(side, "parent");
        }
        other => panic!("expected SubgraphChannelMismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn a_mapping_naming_a_channel_the_subgraph_lacks_fails_at_compile_time() {
    let error = StateGraph::with_channels(&["document", "size"])
        .add_node(
            SubgraphNode::new("measure_doc", measuring_graph())
                .with_input("document", "absent_in_child"),
        )
        .add_edge(START, "measure_doc")
        .add_edge("measure_doc", END)
        .compile();
    let error = match error {
        Err(error) => error,
        Ok(_) => panic!("a channel the subgraph does not declare must fail compilation"),
    };

    assert!(
        matches!(
            error,
            GraphError::SubgraphChannelMismatch { ref channel, ref side, .. }
                if channel == "absent_in_child" && side == "subgraph"
        ),
        "expected the subgraph side to be named, got {error:?}"
    );
}

#[tokio::test]
async fn a_subgraph_exchanging_nothing_fails_at_compile_time() {
    // Two schemas with no shared name and no mapping: the subgraph could not
    // affect its parent, which is a naming mistake rather than an intention.
    let error = StateGraph::with_channels(&["document"])
        .add_node(SubgraphNode::new("measure_doc", measuring_graph()).isolated())
        .add_edge(START, "measure_doc")
        .add_edge("measure_doc", END)
        .compile();
    let error = match error {
        Err(error) => error,
        Ok(_) => panic!("a subgraph that exchanges nothing must fail compilation"),
    };
    assert!(
        matches!(error, GraphError::InvalidGraph(ref m) if m.contains("exchanges no channels"))
    );
}

#[tokio::test]
async fn a_pause_inside_a_subgraph_pauses_the_parent() {
    let after_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&after_runs);

    let inner = Arc::new(
        StateGraph::with_channels(&["decision", "approved"])
            .add_node_fn("asks", |_ctx| async move {
                Ok(NodeOutput::interrupt("approve the inner step"))
            })
            .add_edge(START, "asks")
            .add_edge("asks", END)
            .compile()
            .unwrap()
            .with_checkpointer(MemoryCheckpointer::new()),
    );

    let outer = StateGraph::with_channels(&["decision", "approved", "after"])
        .add_node(SubgraphNode::new("gate", inner))
        .add_node_fn("after", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("after", json!(true)))
            }
        })
        .add_edge(START, "gate")
        .add_edge("gate", "after")
        .add_edge("after", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    let error = outer
        .invoke(State::new(), ExecutionConfig::new("sub-pause"))
        .await
        .expect_err("a pause inside the subgraph must pause the parent");

    match error {
        GraphError::Interrupted(interrupted) => {
            let text = interrupted.interrupt.to_string();
            assert!(text.contains("gate"), "the pause names the subgraph: {text}");
            assert!(text.contains("approve the inner step"), "and carries the message: {text}");
        }
        other => panic!("expected Interrupted, got {other:?}"),
    }
    assert_eq!(
        after_runs.load(Ordering::SeqCst),
        0,
        "the parent must not continue past a pause inside its subgraph"
    );
}

#[tokio::test]
async fn a_subgraph_runs_on_its_own_thread() {
    // The subgraph's checkpoints are namespaced under the parent's thread, so two
    // subgraphs of one parent cannot collide.
    let inner = measuring_graph();
    let named = Arc::new(
        StateGraph::with_channels(&["text", "length"])
            .add_node_fn("measure", |ctx| async move {
                let text = ctx.get("text").and_then(|v| v.as_str()).unwrap_or("");
                Ok(NodeOutput::new().with_update("length", json!(text.len())))
            })
            .add_edge(START, "measure")
            .add_edge("measure", END)
            .compile()
            .unwrap(),
    );

    let outer = StateGraph::with_channels(&["document", "first", "second"])
        .add_node(
            SubgraphNode::new("left", inner)
                .with_input("document", "text")
                .with_output("length", "first"),
        )
        .add_node(
            SubgraphNode::new("right", named)
                .with_input("document", "text")
                .with_output("length", "second"),
        )
        .add_edge(START, "left")
        .add_edge(START, "right")
        .add_edge("left", END)
        .add_edge("right", END)
        .compile()
        .unwrap();

    let mut input = State::new();
    input.insert("document".to_string(), json!("abcd"));
    let state = outer.invoke(input, ExecutionConfig::new("sub-threads")).await.unwrap();
    assert_eq!(state.get("first"), Some(&json!(4)));
    assert_eq!(state.get("second"), Some(&json!(4)));
}
