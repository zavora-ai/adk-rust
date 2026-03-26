//! Code Node scenarios: Rust mode (JSON eval + interpolation).

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

use super::set_node::standard;

pub async fn run() -> Result<()> {
    println!("── 12. Code Node (Rust mode) ───────────────────");

    // 12a. Code as JSON expression
    let graph = GraphAgent::builder("code-json")
        .description("Code node JSON eval")
        .channels(&["codeResult"])
        .action_node(ActionNodeConfig::Code(CodeNodeConfig {
            standard: standard("json_code", "JSON Expression", "codeResult"),
            language: CodeLanguage::Rust,
            code: r#"{"computed": true, "values": [1, 2, 3]}"#.into(),
            sandbox: None,
        }))
        .edge(START, "json_code")
        .edge("json_code", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  JSON eval:   codeResult={}", result["codeResult"]);
    assert_eq!(result["codeResult"]["computed"], json!(true));
    assert_eq!(result["codeResult"]["values"], json!([1, 2, 3]));

    // 12b. Code with variable interpolation
    let graph = GraphAgent::builder("code-interp")
        .description("Code node interpolation")
        .channels(&["user", "codeResult"])
        .action_node(ActionNodeConfig::Code(CodeNodeConfig {
            standard: standard("interp_code", "Interpolated Code", "codeResult"),
            language: CodeLanguage::Rust,
            code: "Processed user: {{user}}".into(),
            sandbox: None,
        }))
        .edge(START, "interp_code")
        .edge("interp_code", END)
        .build()?;

    let mut input = State::new();
    input.insert("user".into(), json!("Alice"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  Interpolate: codeResult={}", result["codeResult"]);
    assert_eq!(result["codeResult"], json!("Processed user: Alice"));

    println!("  ✓ All Code node scenarios passed\n");
    Ok(())
}
