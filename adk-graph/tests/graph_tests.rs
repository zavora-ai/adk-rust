//! Graph construction and compilation tests

use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::StateGraph;
use adk_graph::node::NodeOutput;
use serde_json::json;

#[test]
fn test_basic_graph_construction() {
    let graph = StateGraph::with_channels(&["input", "output"])
        .add_node_fn("process", |_ctx| async move { Ok(NodeOutput::new()) })
        .add_edge(START, "process")
        .add_edge("process", END)
        .compile();

    assert!(graph.is_ok());
}

#[test]
fn test_graph_missing_entry() {
    let graph = StateGraph::with_channels(&["input"])
        .add_node_fn("process", |_ctx| async move { Ok(NodeOutput::new()) })
        .add_edge("process", END)
        .compile();

    assert!(matches!(graph, Err(GraphError::NoEntryPoint)));
}

#[test]
fn test_graph_missing_node() {
    let graph = StateGraph::with_channels(&["input"])
        .add_node_fn("process", |_ctx| async move { Ok(NodeOutput::new()) })
        .add_edge(START, "nonexistent")
        .compile();

    // Reference to nonexistent node should fail validation
    assert!(graph.is_err());
}

#[test]
fn test_graph_with_multiple_nodes() {
    let graph =
        StateGraph::with_channels(&["value"])
            .add_node_fn("step1", |_ctx| async move {
                Ok(NodeOutput::new().with_update("value", json!(1)))
            })
            .add_node_fn("step2", |_ctx| async move {
                Ok(NodeOutput::new().with_update("value", json!(2)))
            })
            .add_node_fn("step3", |_ctx| async move {
                Ok(NodeOutput::new().with_update("value", json!(3)))
            })
            .add_edge(START, "step1")
            .add_edge("step1", "step2")
            .add_edge("step2", "step3")
            .add_edge("step3", END)
            .compile();

    assert!(graph.is_ok());
}

#[test]
fn test_graph_with_conditional_edges() {
    let graph = StateGraph::with_channels(&["action", "result"])
        .add_node_fn("router", |_ctx| async move { Ok(NodeOutput::new()) })
        .add_node_fn("action_a", |_ctx| async move {
            Ok(NodeOutput::new().with_update("result", json!("A")))
        })
        .add_node_fn("action_b", |_ctx| async move {
            Ok(NodeOutput::new().with_update("result", json!("B")))
        })
        .add_edge(START, "router")
        .add_conditional_edges(
            "router",
            |state| state.get("action").and_then(|v| v.as_str()).unwrap_or("a").to_string(),
            [("a", "action_a"), ("b", "action_b")],
        )
        .add_edge("action_a", END)
        .add_edge("action_b", END)
        .compile();

    assert!(graph.is_ok());
}

#[test]
fn test_graph_with_cycle() {
    let graph = StateGraph::with_channels(&["count", "done"])
        .add_node_fn("increment", |ctx| async move {
            let count = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new()
                .with_update("count", json!(count + 1))
                .with_update("done", json!(count >= 2)))
        })
        .add_node_fn("finish", |_ctx| async move { Ok(NodeOutput::new()) })
        .add_edge(START, "increment")
        .add_conditional_edges(
            "increment",
            |state| {
                if state.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
                    "finish".to_string()
                } else {
                    "increment".to_string()
                }
            },
            [("increment", "increment"), ("finish", "finish")],
        )
        .add_edge("finish", END)
        .compile();

    assert!(graph.is_ok());
}

#[test]
fn test_graph_with_recursion_limit() {
    // Just verify it compiles and doesn't panic
    let _graph = StateGraph::with_channels(&["count"])
        .add_node_fn("loop", |_ctx| async move { Ok(NodeOutput::new()) })
        .add_edge(START, "loop")
        .add_edge("loop", END)
        .compile()
        .unwrap()
        .with_recursion_limit(5);
}

#[test]
fn test_state_graph_builder_methods() {
    let graph = StateGraph::with_channels(&["a", "b", "c"]);

    assert!(graph.schema.channels.contains_key("a"));
    assert!(graph.schema.channels.contains_key("b"));
    assert!(graph.schema.channels.contains_key("c"));
}

#[test]
fn test_graph_node_access() {
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("node_a", |_ctx| async move { Ok(NodeOutput::new()) })
        .add_node_fn("node_b", |_ctx| async move { Ok(NodeOutput::new()) })
        .add_edge(START, "node_a")
        .add_edge("node_a", "node_b")
        .add_edge("node_b", END);

    assert!(graph.nodes.contains_key("node_a"));
    assert!(graph.nodes.contains_key("node_b"));
    assert_eq!(graph.nodes.len(), 2);
}
