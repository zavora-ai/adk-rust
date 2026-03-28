//! Transform Node scenarios: template interpolation, JSONPath extraction, type coercion.

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

use super::set_node::standard;

pub async fn run() -> Result<()> {
    println!("── 2. Transform Node ───────────────────────────");

    // 2a. Template interpolation
    let graph = GraphAgent::builder("template-demo")
        .description("Template transform")
        .channels(&["name", "greeting"])
        .action_node(ActionNodeConfig::Transform(TransformNodeConfig {
            standard: standard("template", "Greet User", "greeting"),
            transform_type: TransformType::Template,
            template: Some("Hello, {{name}}! Welcome to ADK.".into()),
            expression: None,
            builtin: None,
            coercion: None,
        }))
        .edge(START, "template")
        .edge("template", END)
        .build()?;

    let mut input = State::new();
    input.insert("name".into(), json!("Alice"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  Template:    greeting={}", result["greeting"]);
    assert_eq!(result["greeting"], json!("Hello, Alice! Welcome to ADK."));

    // 2b. JSONPath extraction (dot-notation)
    let graph = GraphAgent::builder("jsonpath-demo")
        .description("JSONPath transform")
        .channels(&["user", "email"])
        .action_node(ActionNodeConfig::Transform(TransformNodeConfig {
            standard: standard("extract", "Extract Email", "email"),
            transform_type: TransformType::Jsonpath,
            template: None,
            expression: Some("user.email".into()),
            builtin: None,
            coercion: None,
        }))
        .edge(START, "extract")
        .edge("extract", END)
        .build()?;

    let mut input = State::new();
    input.insert("user".into(), json!({"name": "Bob", "email": "bob@example.com"}));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  JSONPath:    email={}", result["email"]);
    assert_eq!(result["email"], json!("bob@example.com"));

    // 2c. Type coercion (string to number)
    let graph = GraphAgent::builder("coercion-demo")
        .description("Coercion transform")
        .channels(&["raw_count", "count"])
        .action_node(ActionNodeConfig::Transform(TransformNodeConfig {
            standard: standard("coerce", "Parse Count", "count"),
            transform_type: TransformType::Jsonpath,
            template: None,
            expression: Some("raw_count".into()),
            builtin: None,
            coercion: Some(TypeCoercion { from_type: "string".into(), to_type: "number".into() }),
        }))
        .edge(START, "coerce")
        .edge("coerce", END)
        .build()?;

    let mut input = State::new();
    input.insert("raw_count".into(), json!("42"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  Coercion:    count={} (string→number)", result["count"]);
    assert_eq!(result["count"], json!(42.0));

    println!("  ✓ All Transform node scenarios passed\n");
    Ok(())
}
