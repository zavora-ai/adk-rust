//! Graph execution tests

use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;

#[tokio::test]
async fn test_simple_execution() {
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("double", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(value * 2)))
        })
        .add_edge(START, "double")
        .add_edge("double", END)
        .compile()
        .unwrap();

    let mut input = State::new();
    input.insert("value".to_string(), json!(21));

    let result = graph.invoke(input, ExecutionConfig::new("test-thread")).await.unwrap();

    assert_eq!(result.get("value"), Some(&json!(42)));
}

#[tokio::test]
async fn test_sequential_execution() {
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("add_one", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(value + 1)))
        })
        .add_node_fn("double", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(value * 2)))
        })
        .add_node_fn("add_three", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(value + 3)))
        })
        .add_edge(START, "add_one")
        .add_edge("add_one", "double")
        .add_edge("double", "add_three")
        .add_edge("add_three", END)
        .compile()
        .unwrap();

    let mut input = State::new();
    input.insert("value".to_string(), json!(5));

    // (5 + 1) * 2 + 3 = 15
    let result = graph.invoke(input, ExecutionConfig::new("test-thread")).await.unwrap();

    assert_eq!(result.get("value"), Some(&json!(15)));
}

#[tokio::test]
async fn test_conditional_routing() {
    let graph = StateGraph::with_channels(&["value", "route", "result"])
        .add_node_fn("classify", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            let route = if value > 10 { "high" } else { "low" };
            Ok(NodeOutput::new().with_update("route", json!(route)))
        })
        .add_node_fn("high_handler", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("result", json!(format!("HIGH: {}", value))))
        })
        .add_node_fn("low_handler", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("result", json!(format!("LOW: {}", value))))
        })
        .add_edge(START, "classify")
        .add_conditional_edges(
            "classify",
            |state| state.get("route").and_then(|v| v.as_str()).unwrap_or("low").to_string(),
            [("high", "high_handler"), ("low", "low_handler")],
        )
        .add_edge("high_handler", END)
        .add_edge("low_handler", END)
        .compile()
        .unwrap();

    // Test high value
    let mut input_high = State::new();
    input_high.insert("value".to_string(), json!(50));

    let result_high = graph.invoke(input_high, ExecutionConfig::new("test-high")).await.unwrap();

    assert_eq!(result_high.get("result"), Some(&json!("HIGH: 50")));

    // Test low value
    let mut input_low = State::new();
    input_low.insert("value".to_string(), json!(5));

    let result_low = graph.invoke(input_low, ExecutionConfig::new("test-low")).await.unwrap();

    assert_eq!(result_low.get("result"), Some(&json!("LOW: 5")));
}

#[tokio::test]
async fn test_cycle_with_limit() {
    let graph = StateGraph::with_channels(&["count"])
        .add_node_fn("increment", |ctx| async move {
            let count = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("count", json!(count + 1)))
        })
        .add_edge(START, "increment")
        .add_conditional_edges(
            "increment",
            |state| {
                let count = state.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                if count >= 5 {
                    END.to_string()
                } else {
                    "increment".to_string()
                }
            },
            [("increment", "increment"), (END, END)],
        )
        .compile()
        .unwrap();

    let input = State::new();
    let result = graph.invoke(input, ExecutionConfig::new("test-cycle")).await.unwrap();

    assert_eq!(result.get("count"), Some(&json!(5)));
}

#[tokio::test]
async fn test_recursion_limit() {
    let graph = StateGraph::with_channels(&["count"])
        .add_node_fn("infinite_loop", |ctx| async move {
            let count = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("count", json!(count + 1)))
        })
        .add_edge(START, "infinite_loop")
        .add_edge("infinite_loop", "infinite_loop") // Infinite cycle
        .compile()
        .unwrap()
        .with_recursion_limit(5);

    let input = State::new();
    let result = graph.invoke(input, ExecutionConfig::new("test-limit")).await;

    assert!(matches!(result, Err(GraphError::RecursionLimitExceeded(_))));
}

#[tokio::test]
async fn test_with_checkpointer() {
    let checkpointer = MemoryCheckpointer::new();

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("process", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(value + 10)))
        })
        .add_edge(START, "process")
        .add_edge("process", END)
        .compile()
        .unwrap()
        .with_checkpointer(checkpointer);

    let mut input = State::new();
    input.insert("value".to_string(), json!(5));

    let result = graph.invoke(input, ExecutionConfig::new("checkpoint-test")).await.unwrap();

    assert_eq!(result.get("value"), Some(&json!(15)));
}

#[tokio::test]
async fn test_multiple_outputs() {
    let graph = StateGraph::with_channels(&["input", "length", "uppercase", "words"])
        .add_node_fn("analyze", |ctx| async move {
            let input = ctx.get("input").and_then(|v| v.as_str()).unwrap_or("");
            Ok(NodeOutput::new()
                .with_update("length", json!(input.len()))
                .with_update("uppercase", json!(input.to_uppercase()))
                .with_update("words", json!(input.split_whitespace().count())))
        })
        .add_edge(START, "analyze")
        .add_edge("analyze", END)
        .compile()
        .unwrap();

    let mut input = State::new();
    input.insert("input".to_string(), json!("hello world"));

    let result = graph.invoke(input, ExecutionConfig::new("test-multi")).await.unwrap();

    assert_eq!(result.get("length"), Some(&json!(11)));
    assert_eq!(result.get("uppercase"), Some(&json!("HELLO WORLD")));
    assert_eq!(result.get("words"), Some(&json!(2)));
}

#[tokio::test]
async fn test_empty_input_state() {
    let graph = StateGraph::with_channels(&["result"])
        .add_node_fn("generate", |_ctx| async move {
            Ok(NodeOutput::new().with_update("result", json!("generated")))
        })
        .add_edge(START, "generate")
        .add_edge("generate", END)
        .compile()
        .unwrap();

    let input = State::new();
    let result = graph.invoke(input, ExecutionConfig::new("test-empty")).await.unwrap();

    assert_eq!(result.get("result"), Some(&json!("generated")));
}
