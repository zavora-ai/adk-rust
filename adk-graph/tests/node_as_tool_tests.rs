//! A graph node, or a whole graph, can be given to a model as a tool.
//!
//! A graph is usually the deterministic half of a system and an `LlmAgent` the
//! deciding half. Without this the two could only be composed the other way
//! round, with the graph calling the agent through an `AgentNode`, so a model
//! could never choose to run a checked workflow.

use adk_core::{Tool, ToolContext};
use adk_graph::edge::{END, START};
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::NodeOutput;
use adk_graph::tool::NodeTool;
use adk_tool::SimpleToolContext;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn ctx() -> Arc<dyn ToolContext> {
    Arc::new(SimpleToolContext::new("node-tool-test")) as Arc<dyn ToolContext>
}

/// A graph with one node that doubles a number.
fn doubling_graph(calls: Arc<AtomicUsize>) -> Arc<CompiledGraph> {
    Arc::new(
        StateGraph::with_channels(&["input", "doubled"])
            .add_node_fn("double", move |ctx| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let value = ctx.get("input").and_then(|v| v.as_i64()).unwrap_or(0);
                    Ok(NodeOutput::new().with_update("doubled", json!(value * 2)))
                }
            })
            .add_edge(START, "double")
            .add_edge("double", END)
            .compile()
            .unwrap(),
    )
}

/// A whole graph runs as one tool call.
#[tokio::test]
async fn a_graph_runs_as_a_tool() {
    let calls = Arc::new(AtomicUsize::new(0));
    let tool = NodeTool::for_graph(doubling_graph(Arc::clone(&calls)))
        .with_name("double_it")
        .with_description("Doubles the input.");

    assert_eq!(tool.name(), "double_it");
    let result = tool.execute(ctx(), json!({ "input": 21 })).await.expect("the tool must run");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.get("doubled"), Some(&json!(42)));
}

/// One node runs on its own, without following edges.
#[tokio::test]
async fn a_single_node_runs_as_a_tool() {
    let calls = Arc::new(AtomicUsize::new(0));
    let tool = NodeTool::for_node(doubling_graph(Arc::clone(&calls)), "double");

    let result = tool.execute(ctx(), json!({ "input": 5 })).await.expect("the tool must run");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.get("doubled"), Some(&json!(10)));
}

/// A graph tool advertises the graph's declared channels.
///
/// A node declares no schema, so its tool accepts any object; a graph's channels
/// are known, so they become its parameters.
#[tokio::test]
async fn the_parameter_schema_comes_from_the_graphs_channels() {
    let graph = doubling_graph(Arc::new(AtomicUsize::new(0)));

    let graph_tool = NodeTool::for_graph(Arc::clone(&graph));
    let schema = graph_tool.parameters_schema().expect("a schema");
    let properties = schema.get("properties").and_then(|p| p.as_object()).expect("properties");
    assert!(properties.contains_key("input"));
    assert!(properties.contains_key("doubled"));

    let node_tool = NodeTool::for_node(graph, "double");
    let schema = node_tool.parameters_schema().expect("a schema");
    assert_eq!(
        schema.get("properties"),
        None,
        "a node declares no channels, so its tool must not invent parameters"
    );
}

/// An explicit schema overrides the derived one.
#[tokio::test]
async fn an_explicit_schema_wins() {
    let tool = NodeTool::for_graph(doubling_graph(Arc::new(AtomicUsize::new(0))))
        .with_parameters_schema(json!({
            "type": "object",
            "properties": { "input": { "type": "integer" } },
            "required": ["input"]
        }));

    let schema = tool.parameters_schema().expect("a schema");
    assert_eq!(schema.get("required"), Some(&json!(["input"])));
}

/// A pause is returned to the caller, not swallowed.
#[tokio::test]
async fn an_interrupt_is_reported_to_the_caller() {
    let graph = Arc::new(
        StateGraph::with_channels(&["approved"])
            .add_node_fn("gate", |_ctx| async move {
                Ok(NodeOutput::interrupt_with_data("approve?", json!({ "amount": 99 })))
            })
            .add_edge(START, "gate")
            .add_edge("gate", END)
            .compile()
            .unwrap()
            .with_checkpointer(adk_graph::checkpoint::MemoryCheckpointer::new()),
    );

    let tool = NodeTool::for_graph(graph);
    let result = tool.execute(ctx(), json!({})).await.expect("a pause is not an error");

    assert_eq!(result.get("status"), Some(&json!("interrupted")));
    let interrupt = result.get("interrupt").expect("the interrupt payload");
    assert_eq!(interrupt.get("kind"), Some(&json!("dynamic")));
    assert_eq!(
        interrupt.get("data").and_then(|d| d.get("amount")),
        Some(&json!(99)),
        "the payload a node attached must reach the caller"
    );
}

/// The tool reports itself long-running, so a pause travels the existing
/// tool-confirmation path rather than a second mechanism.
#[tokio::test]
async fn a_graph_tool_is_long_running() {
    let tool = NodeTool::for_graph(doubling_graph(Arc::new(AtomicUsize::new(0))));
    assert!(tool.is_long_running());
}
