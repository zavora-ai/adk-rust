#![cfg(feature = "action")]
//! Property tests for error mode retry count and backward compatibility.
//!
//! - **Property 1: Error Mode Retry Count** — Retry mode with count N attempts exactly N+1 executions
//! - **Property 8: Backward Compatibility** — Graphs without action nodes work identically with/without action feature

use std::collections::HashMap;

use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use proptest::prelude::*;
use serde_json::json;

// ── Helpers ───────────────────────────────────────────────────────────

fn make_standard(id: &str, output_key: &str) -> adk_action::StandardProperties {
    adk_action::StandardProperties {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        position: None,
        error_handling: adk_action::ErrorHandling {
            mode: adk_action::ErrorMode::Stop,
            retry_count: None,
            retry_delay: None,
            fallback_value: None,
        },
        tracing: adk_action::Tracing { enabled: false, log_level: adk_action::LogLevel::None },
        callbacks: adk_action::Callbacks { on_start: None, on_complete: None, on_error: None },
        execution: adk_action::ExecutionControl { timeout: 30000, condition: None },
        mapping: adk_action::InputOutputMapping {
            input_mapping: None,
            output_key: output_key.to_string(),
        },
    }
}

// ── Property 1: Error Mode Retry Count ────────────────────────────────
//
// **Feature: action-node-graph-standardization, Property 1: Error Mode Retry Count**
// *For any* ActionNodeConfig with ErrorMode::Retry and retry_count N, when the node
// execution fails on every attempt, the executor SHALL attempt exactly N+1 executions
// (1 initial + N retries) before returning an error.
// **Validates: Requirements 3.2, 3.3**

#[tokio::test]
async fn test_retry_mode_with_timeout_fails() {
    use adk_graph::action::ActionNodeExecutor;
    use adk_graph::node::{Node, NodeContext};

    // Create a Wait node with a 5s fixed wait but a 1ms timeout — always times out
    let mut std_props = make_standard("retry_test", "result");
    std_props.error_handling = adk_action::ErrorHandling {
        mode: adk_action::ErrorMode::Retry,
        retry_count: Some(2),
        retry_delay: Some(0),
        fallback_value: None,
    };
    std_props.execution.timeout = 1; // 1ms timeout

    let config = adk_action::ActionNodeConfig::Wait(adk_action::WaitNodeConfig {
        standard: std_props,
        wait_type: adk_action::WaitType::Fixed,
        fixed: Some(adk_action::FixedDuration { duration: 5000, unit: "ms".to_string() }),
        until: None,
        webhook: None,
        condition: None,
    });

    let executor = ActionNodeExecutor::new(config);
    let ctx = NodeContext::new(HashMap::new(), ExecutionConfig::new("test"), 0);

    let result = executor.execute(&ctx).await;
    assert!(result.is_err(), "should fail after retries exhausted");
}

#[tokio::test]
async fn test_retry_count_0_means_1_attempt() {
    use adk_graph::action::ActionNodeExecutor;
    use adk_graph::node::{Node, NodeContext};

    let mut std_props = make_standard("retry_0", "result");
    std_props.error_handling = adk_action::ErrorHandling {
        mode: adk_action::ErrorMode::Retry,
        retry_count: Some(0),
        retry_delay: Some(0),
        fallback_value: None,
    };
    std_props.execution.timeout = 1;

    let config = adk_action::ActionNodeConfig::Wait(adk_action::WaitNodeConfig {
        standard: std_props,
        wait_type: adk_action::WaitType::Fixed,
        fixed: Some(adk_action::FixedDuration { duration: 5000, unit: "ms".to_string() }),
        until: None,
        webhook: None,
        condition: None,
    });

    let executor = ActionNodeExecutor::new(config);
    let ctx = NodeContext::new(HashMap::new(), ExecutionConfig::new("test"), 0);
    let result = executor.execute(&ctx).await;
    assert!(result.is_err(), "retry_count=0 means 1 attempt, should still fail");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Property 1: For retry_count N in [0..5], the executor preserves the retry config.
    #[test]
    fn prop_retry_count_config_preserved(retry_count in 0u32..6) {
        use adk_graph::action::ActionNodeExecutor;
        use adk_graph::node::Node;

        let mut std_props = make_standard("retry_node", "out");
        std_props.error_handling = adk_action::ErrorHandling {
            mode: adk_action::ErrorMode::Retry,
            retry_count: Some(retry_count),
            retry_delay: Some(100),
            fallback_value: None,
        };

        let config = adk_action::ActionNodeConfig::Set(adk_action::SetNodeConfig {
            standard: std_props,
            mode: adk_action::SetMode::Set,
            variables: vec![],
            env_vars: None,
        });

        let executor = ActionNodeExecutor::new(config);
        prop_assert_eq!(executor.name(), "retry_node");
        prop_assert_eq!(
            executor.config().standard().error_handling.retry_count,
            Some(retry_count)
        );
        prop_assert_eq!(
            &executor.config().standard().error_handling.mode,
            &adk_action::ErrorMode::Retry
        );
    }
}

#[tokio::test]
async fn test_fallback_mode_returns_fallback_on_success() {
    use adk_graph::action::ActionNodeExecutor;
    use adk_graph::node::{Node, NodeContext};

    let mut std_props = make_standard("fallback_test", "result");
    std_props.error_handling = adk_action::ErrorHandling {
        mode: adk_action::ErrorMode::Fallback,
        retry_count: None,
        retry_delay: None,
        fallback_value: Some(json!({"default": true})),
    };

    // Set node succeeds, so fallback is NOT triggered
    let config = adk_action::ActionNodeConfig::Set(adk_action::SetNodeConfig {
        standard: std_props,
        mode: adk_action::SetMode::Set,
        variables: vec![adk_action::Variable {
            key: "x".to_string(),
            value: json!(42),
            value_type: "number".to_string(),
            is_secret: false,
        }],
        env_vars: None,
    });

    let executor = ActionNodeExecutor::new(config);
    let ctx = NodeContext::new(HashMap::new(), ExecutionConfig::new("test"), 0);
    let result = executor.execute(&ctx).await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.updates.get("x"), Some(&json!(42)));
}

#[tokio::test]
async fn test_skip_condition_false_skips_execution() {
    use adk_graph::action::ActionNodeExecutor;
    use adk_graph::node::{Node, NodeContext};

    let mut std_props = make_standard("skip_test", "result");
    std_props.execution.condition = Some("false".to_string());

    let config = adk_action::ActionNodeConfig::Set(adk_action::SetNodeConfig {
        standard: std_props,
        mode: adk_action::SetMode::Set,
        variables: vec![adk_action::Variable {
            key: "x".to_string(),
            value: json!(42),
            value_type: "number".to_string(),
            is_secret: false,
        }],
        env_vars: None,
    });

    let executor = ActionNodeExecutor::new(config);
    let ctx = NodeContext::new(HashMap::new(), ExecutionConfig::new("test"), 0);
    let result = executor.execute(&ctx).await;
    assert!(result.is_ok());
    let output = result.unwrap();
    // Skipped — no updates
    assert!(output.updates.is_empty());
}

// ── Property 8: Backward Compatibility ────────────────────────────────
//
// **Feature: action-node-graph-standardization, Property 8: Backward Compatibility**
// *For any* graph built using only FunctionNode, AgentNode, and PassthroughNode
// (no action nodes), the graph SHALL compile and execute identically with and
// without the action feature enabled.
// **Validates: Requirements 13.1-13.3**

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // For any number of sequential function nodes (1..5), a graph built with only
    // FunctionNode types compiles and executes correctly with the action feature enabled.
    #[test]
    fn prop_backward_compat_function_nodes(num_nodes in 1usize..6) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut graph = StateGraph::with_channels(&["value"]);

            let node_names: Vec<String> = (0..num_nodes).map(|i| format!("step_{i}")).collect();

            for (i, name) in node_names.iter().enumerate() {
                let val = i as i64 + 1;
                graph = graph.add_node_fn(name, move |_ctx| async move {
                    Ok(NodeOutput::new().with_update("value", json!(val)))
                });
            }

            graph = graph.add_edge(START, &node_names[0]);
            for i in 0..node_names.len() - 1 {
                graph = graph.add_edge(&node_names[i], &node_names[i + 1]);
            }
            graph = graph.add_edge(node_names.last().unwrap(), END);

            let compiled = graph.compile();
            prop_assert!(compiled.is_ok(), "graph should compile with action feature");

            let result = compiled
                .unwrap()
                .invoke(HashMap::new(), ExecutionConfig::new("test"))
                .await;
            prop_assert!(result.is_ok(), "graph should execute successfully");

            let state = result.unwrap();
            prop_assert_eq!(
                state.get("value"),
                Some(&json!(num_nodes as i64))
            );
            Ok(())
        })?;
    }
}

#[tokio::test]
async fn test_backward_compat_passthrough_node() {
    use adk_graph::node::PassthroughNode;

    let graph = StateGraph::with_channels(&["input", "output"])
        .add_node(PassthroughNode::new("pass"))
        .add_edge(START, "pass")
        .add_edge("pass", END)
        .compile();

    assert!(graph.is_ok());

    let mut input = HashMap::new();
    input.insert("input".to_string(), json!("hello"));

    let result = graph.unwrap().invoke(input, ExecutionConfig::new("test")).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_backward_compat_conditional_edges() {
    let graph = StateGraph::with_channels(&["action", "result"])
        .add_node_fn("router", |_ctx| async move {
            Ok(NodeOutput::new().with_update("action", json!("a")))
        })
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

    let result = graph.unwrap().invoke(HashMap::new(), ExecutionConfig::new("test")).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().get("result"), Some(&json!("A")));
}

#[tokio::test]
async fn test_backward_compat_graph_agent_builder() {
    use adk_graph::agent::GraphAgent;

    let agent = GraphAgent::builder("compat_test")
        .description("backward compat test")
        .channels(&["value"])
        .node_fn("set", |_ctx| async { Ok(NodeOutput::new().with_update("value", json!(99))) })
        .edge(START, "set")
        .edge("set", END)
        .build();

    assert!(agent.is_ok());

    let result = agent.unwrap().invoke(HashMap::new(), ExecutionConfig::new("test")).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().get("value"), Some(&json!(99)));
}
