//! Set Node scenarios: set, merge, delete modes + env vars + secrets.

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

pub async fn run() -> Result<()> {
    println!("── 1. Set Node ──────────────────────────────────");

    // 1a. Set mode: insert variables
    let graph = GraphAgent::builder("set-demo")
        .description("Set node demo")
        .channels(&["greeting", "count", "config"])
        .action_node(ActionNodeConfig::Set(SetNodeConfig {
            standard: standard("set_vars", "Set Variables", "setResult"),
            mode: SetMode::Set,
            variables: vec![
                Variable {
                    key: "greeting".into(),
                    value: json!("Hello, ADK!"),
                    value_type: "string".into(),
                    is_secret: false,
                },
                Variable {
                    key: "count".into(),
                    value: json!(42),
                    value_type: "number".into(),
                    is_secret: false,
                },
                Variable {
                    key: "config".into(),
                    value: json!({"debug": true, "level": "info"}),
                    value_type: "object".into(),
                    is_secret: false,
                },
            ],
            env_vars: None,
        }))
        .edge(START, "set_vars")
        .edge("set_vars", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  Set mode:    greeting={}, count={}", result["greeting"], result["count"]);
    assert_eq!(result["greeting"], json!("Hello, ADK!"));
    assert_eq!(result["count"], json!(42));

    // 1b. Merge mode: deep-merge objects
    let graph = GraphAgent::builder("merge-demo")
        .description("Merge node demo")
        .channels(&["config"])
        .action_node(ActionNodeConfig::Set(SetNodeConfig {
            standard: standard("merge_vars", "Merge Config", "mergeResult"),
            mode: SetMode::Merge,
            variables: vec![Variable {
                key: "config".into(),
                value: json!({"verbose": true, "level": "debug"}),
                value_type: "object".into(),
                is_secret: false,
            }],
            env_vars: None,
        }))
        .edge(START, "merge_vars")
        .edge("merge_vars", END)
        .build()?;

    let mut input = State::new();
    input.insert("config".into(), json!({"debug": true, "level": "info"}));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    let config = &result["config"];
    println!("  Merge mode:  config.debug={}, config.verbose={}, config.level={}",
        config["debug"], config["verbose"], config["level"]);
    assert_eq!(config["debug"], json!(true));
    assert_eq!(config["verbose"], json!(true));
    assert_eq!(config["level"], json!("debug")); // overwritten by merge

    // 1c. Delete mode
    let graph = GraphAgent::builder("delete-demo")
        .description("Delete node demo")
        .channels(&["temp_data"])
        .action_node(ActionNodeConfig::Set(SetNodeConfig {
            standard: standard("delete_vars", "Delete Temp", "deleteResult"),
            mode: SetMode::Delete,
            variables: vec![Variable {
                key: "temp_data".into(),
                value: json!(null),
                value_type: "string".into(),
                is_secret: false,
            }],
            env_vars: None,
        }))
        .edge(START, "delete_vars")
        .edge("delete_vars", END)
        .build()?;

    let mut input = State::new();
    input.insert("temp_data".into(), json!("should be removed"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  Delete mode: temp_data={} (null = deleted)", result["temp_data"]);
    assert_eq!(result["temp_data"], json!(null));

    // 1d. Secret variable masking
    let graph = GraphAgent::builder("secret-demo")
        .description("Secret variable demo")
        .channels(&["api_key"])
        .action_node(ActionNodeConfig::Set(SetNodeConfig {
            standard: standard("set_secret", "Set API Key", "secretResult"),
            mode: SetMode::Set,
            variables: vec![Variable {
                key: "api_key".into(),
                value: json!("sk-secret-12345"),
                value_type: "string".into(),
                is_secret: true,
            }],
            env_vars: None,
        }))
        .edge(START, "set_secret")
        .edge("set_secret", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  Secret:      api_key set (masked in traces)");
    assert_eq!(result["api_key"], json!("sk-secret-12345"));

    println!("  ✓ All Set node scenarios passed\n");
    Ok(())
}

/// Helper to create StandardProperties with sensible defaults.
pub fn standard(id: &str, name: &str, output_key: &str) -> StandardProperties {
    StandardProperties {
        id: id.into(),
        name: name.into(),
        description: None,
        position: None,
        error_handling: ErrorHandling {
            mode: ErrorMode::Stop,
            retry_count: None,
            retry_delay: None,
            fallback_value: None,
        },
        tracing: Tracing { enabled: true, log_level: LogLevel::Debug },
        callbacks: Callbacks { on_start: None, on_complete: None, on_error: None },
        execution: ExecutionControl { timeout: 30000, condition: None },
        mapping: InputOutputMapping {
            input_mapping: None,
            output_key: output_key.into(),
        },
    }
}
