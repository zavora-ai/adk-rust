//! Error handling scenarios: continue, retry, fallback, timeout, skip condition.

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

pub async fn run() -> Result<()> {
    println!("── 10. Error Handling ──────────────────────────");

    // 10a. Continue mode: swallow errors and proceed
    let graph = GraphAgent::builder("error-continue")
        .description("Continue on error")
        .channels(&["readResult"])
        .action_node(ActionNodeConfig::File(FileNodeConfig {
            standard: StandardProperties {
                id: "read_missing".into(),
                name: "Read Missing File".into(),
                description: None,
                position: None,
                error_handling: ErrorHandling {
                    mode: ErrorMode::Continue,
                    retry_count: None,
                    retry_delay: None,
                    fallback_value: None,
                },
                tracing: Tracing { enabled: true, log_level: LogLevel::Debug },
                callbacks: Callbacks::default(),
                execution: ExecutionControl { timeout: 5000, condition: None },
                mapping: InputOutputMapping {
                    input_mapping: None,
                    output_key: "readResult".into(),
                },
            },
            operation: FileOperation::Read,
            local: Some(LocalFileConfig { path: "/nonexistent/file.txt".into() }),
            cloud: None,
            parse: None,
            write: None,
            list: None,
        }))
        .edge(START, "read_missing")
        .edge("read_missing", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  continue:    error swallowed, graph completed (readResult={})",
        result.get("readResult").unwrap_or(&json!(null)));

    // 10b. Fallback mode: use default value on error
    let graph = GraphAgent::builder("error-fallback")
        .description("Fallback on error")
        .channels(&["readResult"])
        .action_node(ActionNodeConfig::File(FileNodeConfig {
            standard: StandardProperties {
                id: "read_fallback".into(),
                name: "Read with Fallback".into(),
                description: None,
                position: None,
                error_handling: ErrorHandling {
                    mode: ErrorMode::Fallback,
                    retry_count: None,
                    retry_delay: None,
                    fallback_value: Some(json!({"default": true, "message": "file not available"})),
                },
                tracing: Tracing { enabled: true, log_level: LogLevel::Debug },
                callbacks: Callbacks::default(),
                execution: ExecutionControl { timeout: 5000, condition: None },
                mapping: InputOutputMapping {
                    input_mapping: None,
                    output_key: "readResult".into(),
                },
            },
            operation: FileOperation::Read,
            local: Some(LocalFileConfig { path: "/nonexistent/file.txt".into() }),
            cloud: None,
            parse: None,
            write: None,
            list: None,
        }))
        .edge(START, "read_fallback")
        .edge("read_fallback", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  fallback:    readResult={}", result["readResult"]);
    assert_eq!(result["readResult"]["default"], json!(true));
    assert_eq!(result["readResult"]["message"], json!("file not available"));

    // 10c. Skip condition: skip node when condition is false
    let graph = GraphAgent::builder("skip-demo")
        .description("Skip condition demo")
        .channels(&["should_run", "setResult"])
        .action_node(ActionNodeConfig::Set(SetNodeConfig {
            standard: StandardProperties {
                id: "conditional_set".into(),
                name: "Conditional Set".into(),
                description: None,
                position: None,
                error_handling: ErrorHandling {
                    mode: ErrorMode::Stop,
                    retry_count: None,
                    retry_delay: None,
                    fallback_value: None,
                },
                tracing: Tracing { enabled: true, log_level: LogLevel::Debug },
                callbacks: Callbacks::default(),
                execution: ExecutionControl {
                    timeout: 30000,
                    condition: Some("{{should_run}}".into()),
                },
                mapping: InputOutputMapping {
                    input_mapping: None,
                    output_key: "setResult".into(),
                },
            },
            mode: SetMode::Set,
            variables: vec![Variable {
                key: "was_set".into(),
                value: json!(true),
                value_type: "boolean".into(),
                is_secret: false,
            }],
            env_vars: None,
        }))
        .edge(START, "conditional_set")
        .edge("conditional_set", END)
        .build()?;

    // With condition=false → node is skipped
    let mut input = State::new();
    input.insert("should_run".into(), json!("false"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  skip(false): was_set={} (node skipped)",
        result.get("was_set").unwrap_or(&json!("not set")));
    assert!(result.get("was_set").is_none());

    // With condition=true → node executes
    let mut input = State::new();
    input.insert("should_run".into(), json!("true"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  skip(true):  was_set={} (node executed)", result["was_set"]);
    assert_eq!(result["was_set"], json!(true));

    // 10d. Timeout enforcement
    let graph = GraphAgent::builder("timeout-demo")
        .description("Timeout demo")
        .channels(&["waitResult"])
        .action_node(ActionNodeConfig::Wait(WaitNodeConfig {
            standard: StandardProperties {
                id: "slow_wait".into(),
                name: "Slow Wait".into(),
                description: None,
                position: None,
                error_handling: ErrorHandling {
                    mode: ErrorMode::Stop,
                    retry_count: None,
                    retry_delay: None,
                    fallback_value: None,
                },
                tracing: Tracing { enabled: true, log_level: LogLevel::Debug },
                callbacks: Callbacks::default(),
                execution: ExecutionControl {
                    timeout: 100, // 100ms timeout
                    condition: None,
                },
                mapping: InputOutputMapping {
                    input_mapping: None,
                    output_key: "waitResult".into(),
                },
            },
            wait_type: WaitType::Fixed,
            fixed: Some(FixedDuration { duration: 5000, unit: "ms".into() }), // 5s wait
            until: None,
            webhook: None,
            condition: None,
        }))
        .edge(START, "slow_wait")
        .edge("slow_wait", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await;
    match result {
        Err(e) => println!("  timeout:     correctly timed out: {e}"),
        Ok(_) => panic!("expected timeout error"),
    }

    println!("  ✓ All Error handling scenarios passed\n");
    Ok(())
}
