//! Switch Node scenarios: conditional routing with typed operators.

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::node::NodeOutput;
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

use super::set_node::standard;

pub async fn run() -> Result<()> {
    println!("── 3. Switch Node ─────────────────────────────");

    // 3a. FirstMatch routing based on status code
    let graph = GraphAgent::builder("switch-demo")
        .description("Switch routing demo")
        .channels(&["status", "switchResult", "message"])
        .action_node(ActionNodeConfig::Switch(SwitchNodeConfig {
            standard: standard("router", "Route by Status", "switchResult"),
            conditions: vec![
                SwitchCondition {
                    id: "ok".into(),
                    name: "Success".into(),
                    expression: ExpressionMode {
                        field: "status".into(),
                        operator: "eq".into(),
                        value: "200".into(),
                    },
                    output_port: "success_handler".into(),
                },
                SwitchCondition {
                    id: "not_found".into(),
                    name: "Not Found".into(),
                    expression: ExpressionMode {
                        field: "status".into(),
                        operator: "eq".into(),
                        value: "404".into(),
                    },
                    output_port: "error_handler".into(),
                },
            ],
            evaluation_mode: EvaluationMode::FirstMatch,
            default_branch: Some(END.into()),
        }))
        .node_fn("success_handler", |_ctx| async {
            Ok(NodeOutput::new().with_update("message", json!("Request succeeded!")))
        })
        .node_fn("error_handler", |_ctx| async {
            Ok(NodeOutput::new().with_update("message", json!("Resource not found")))
        })
        .edge(START, "router")
        // Switch conditional edges are auto-registered by action_node()
        .edge("success_handler", END)
        .edge("error_handler", END)
        .build()?;

    // Test with status=200
    let mut input = State::new();
    input.insert("status".into(), json!("200"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  status=200:  message={}", result["message"]);
    assert_eq!(result["message"], json!("Request succeeded!"));

    // Test with status=404
    let mut input = State::new();
    input.insert("status".into(), json!("404"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  status=404:  message={}", result["message"]);
    assert_eq!(result["message"], json!("Resource not found"));

    // 3b. Switch with contains operator
    let graph = GraphAgent::builder("switch-contains")
        .description("Switch contains demo")
        .channels(&["tags", "switchResult", "category"])
        .action_node(ActionNodeConfig::Switch(SwitchNodeConfig {
            standard: standard("tag_router", "Route by Tag", "switchResult"),
            conditions: vec![SwitchCondition {
                id: "urgent".into(),
                name: "Has Urgent".into(),
                expression: ExpressionMode {
                    field: "tags".into(),
                    operator: "contains".into(),
                    value: "urgent".into(),
                },
                output_port: "urgent_handler".into(),
            }],
            evaluation_mode: EvaluationMode::FirstMatch,
            default_branch: Some("normal_handler".into()),
        }))
        .node_fn("urgent_handler", |_ctx| async {
            Ok(NodeOutput::new().with_update("category", json!("URGENT")))
        })
        .node_fn("normal_handler", |_ctx| async {
            Ok(NodeOutput::new().with_update("category", json!("normal")))
        })
        .edge(START, "tag_router")
        .edge("urgent_handler", END)
        .edge("normal_handler", END)
        .build()?;

    let mut input = State::new();
    input.insert("tags".into(), json!(["bug", "urgent", "p0"]));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  contains:    category={}", result["category"]);
    assert_eq!(result["category"], json!("URGENT"));

    let mut input = State::new();
    input.insert("tags".into(), json!(["feature", "p2"]));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  default:     category={}", result["category"]);
    assert_eq!(result["category"], json!("normal"));

    println!("  ✓ All Switch node scenarios passed\n");
    Ok(())
}
