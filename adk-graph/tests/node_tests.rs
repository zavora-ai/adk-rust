//! Node tests

use adk_graph::error::GraphError;
use adk_graph::node::{
    ExecutionConfig, FunctionNode, Node, NodeContext, NodeOutput, PassthroughNode,
};
use adk_graph::state::State;
use serde_json::json;

#[test]
fn test_node_output_builder() {
    let output =
        NodeOutput::new().with_update("key1", json!("value1")).with_update("key2", json!(42));

    assert_eq!(output.updates.get("key1"), Some(&json!("value1")));
    assert_eq!(output.updates.get("key2"), Some(&json!(42)));
    assert!(output.interrupt.is_none());
    assert!(output.events.is_empty());
}

#[tokio::test]
async fn test_function_node() {
    let node = FunctionNode::new("test_node", |ctx| async move {
        let value = ctx.get("input").and_then(|v| v.as_i64()).unwrap_or(0);
        Ok(NodeOutput::new().with_update("output", json!(value * 2)))
    });

    assert_eq!(node.name(), "test_node");

    let mut state = State::new();
    state.insert("input".to_string(), json!(21));

    let config = ExecutionConfig::new("test-thread");
    let ctx = NodeContext::new(state, config, 0);
    let result = node.execute(&ctx).await.unwrap();

    assert_eq!(result.updates.get("output"), Some(&json!(42)));
}

#[tokio::test]
async fn test_passthrough_node() {
    let node = PassthroughNode::new("passthrough");

    assert_eq!(node.name(), "passthrough");

    let mut state = State::new();
    state.insert("value".to_string(), json!("unchanged"));

    let config = ExecutionConfig::new("test-thread");
    let ctx = NodeContext::new(state, config, 0);
    let result = node.execute(&ctx).await.unwrap();

    assert!(result.updates.is_empty());
    assert!(result.interrupt.is_none());
}

#[tokio::test]
async fn test_function_node_with_multiple_outputs() {
    let node = FunctionNode::new("multi_output", |ctx| async move {
        let input = ctx.get("input").and_then(|v| v.as_str()).unwrap_or("");
        Ok(NodeOutput::new()
            .with_update("length", json!(input.len()))
            .with_update("uppercase", json!(input.to_uppercase()))
            .with_update("words", json!(input.split_whitespace().count())))
    });

    let mut state = State::new();
    state.insert("input".to_string(), json!("hello world"));

    let config = ExecutionConfig::new("test-thread");
    let ctx = NodeContext::new(state, config, 0);
    let result = node.execute(&ctx).await.unwrap();

    assert_eq!(result.updates.get("length"), Some(&json!(11)));
    assert_eq!(result.updates.get("uppercase"), Some(&json!("HELLO WORLD")));
    assert_eq!(result.updates.get("words"), Some(&json!(2)));
}

#[tokio::test]
async fn test_node_context_methods() {
    let mut state = State::new();
    state.insert("key1".to_string(), json!("value1"));
    state.insert("key2".to_string(), json!(100));

    let config = ExecutionConfig::new("test-thread");
    let ctx = NodeContext::new(state, config, 5);

    assert_eq!(ctx.get("key1"), Some(&json!("value1")));
    assert_eq!(ctx.get("key2"), Some(&json!(100)));
    assert_eq!(ctx.get("nonexistent"), None);
    assert_eq!(ctx.step, 5);
}

#[tokio::test]
async fn test_node_error_handling() {
    let node = FunctionNode::new("error_node", |_ctx| async move {
        Err(GraphError::NodeExecutionFailed {
            node: "error_node".to_string(),
            message: "Test error".to_string(),
        })
    });

    let config = ExecutionConfig::new("test-thread");
    let ctx = NodeContext::new(State::new(), config, 0);
    let result = node.execute(&ctx).await;

    assert!(result.is_err());
    match result {
        Err(GraphError::NodeExecutionFailed { node, message }) => {
            assert_eq!(node, "error_node");
            assert_eq!(message, "Test error");
        }
        _ => panic!("Expected NodeExecutionFailed error"),
    }
}

#[test]
fn test_execution_config() {
    let config = ExecutionConfig::new("thread-123")
        .with_recursion_limit(100)
        .with_metadata("key", json!("value"));

    assert_eq!(config.thread_id, "thread-123");
    assert_eq!(config.recursion_limit, 100);
    assert_eq!(config.metadata.get("key"), Some(&json!("value")));
    assert!(config.resume_from.is_none());
}

#[test]
fn test_execution_config_with_resume() {
    let config = ExecutionConfig::new("thread-123").with_resume_from("checkpoint-456");

    assert_eq!(config.resume_from, Some("checkpoint-456".to_string()));
}
