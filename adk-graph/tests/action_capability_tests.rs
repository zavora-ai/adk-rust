//! A node whose backend does not exist must be rejected before the graph runs.
//!
//! Several action variants accept and validate a configuration while their backend is
//! still a placeholder: database drivers are not integrated, IMAP monitoring and SMTP
//! sending are not implemented, and JavaScript/TypeScript code has no sandboxed
//! runtime. A workflow could deserialize, validate, and compile, then fail only when
//! that node executed — after earlier nodes had already had their side effects. That is
//! operationally different from a configuration refused up front.

#![cfg(feature = "action")]

use adk_action::{
    ActionNodeConfig, Callbacks, CodeLanguage, CodeNodeConfig, DatabaseConnection,
    DatabaseNodeConfig, DatabaseType, ErrorHandling, ErrorMode, ExecutionControl,
    InputOutputMapping, LogLevel, StandardProperties, Tracing,
};
use adk_graph::action::ActionNodeExecutor;
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::NodeOutput;
use serde_json::json;

fn standard(id: &str) -> StandardProperties {
    StandardProperties {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        position: None,
        error_handling: ErrorHandling {
            mode: ErrorMode::Stop,
            retry_count: None,
            retry_delay: None,
            fallback_value: None,
        },
        tracing: Tracing { enabled: false, log_level: LogLevel::None },
        callbacks: Callbacks { on_start: None, on_complete: None, on_error: None },
        execution: ExecutionControl { timeout: 30000, condition: None },
        mapping: InputOutputMapping { input_mapping: None, output_key: "value".to_string() },
    }
}

/// Compiles a one-node graph around `config` and returns the outcome.
fn compile_with(config: ActionNodeConfig) -> Result<(), String> {
    let node_id = config.standard().id.clone();
    let graph = StateGraph::with_channels(&["value"])
        .add_node(ActionNodeExecutor::new(config))
        .add_edge(START, &node_id)
        .add_edge(&node_id, END)
        .compile();
    graph.map(|_| ()).map_err(|e| e.to_string())
}

#[test]
fn a_database_node_is_rejected_at_build_time() {
    let config = ActionNodeConfig::Database(DatabaseNodeConfig {
        standard: standard("db"),
        connection: DatabaseConnection {
            database_type: DatabaseType::Postgresql,
            connection_string: Some("postgres://localhost/db".to_string()),
            credential_ref: None,
        },
        sql: None,
        mongo: None,
        redis: None,
    });

    let error = compile_with(config).expect_err("a placeholder backend must not compile");
    assert!(
        error.contains("placeholder") || error.contains("cannot execute"),
        "the rejection must say why: {error}"
    );
}

#[test]
fn a_javascript_code_node_is_rejected_at_build_time() {
    let config = ActionNodeConfig::Code(CodeNodeConfig {
        standard: standard("js"),
        language: CodeLanguage::Javascript,
        code: "return 1;".to_string(),
        sandbox: None,
    });

    let error = compile_with(config).expect_err("JS has no runtime, so it must not compile");
    assert!(error.contains("cannot execute"), "the rejection must say why: {error}");
}

#[test]
fn a_rust_code_node_still_compiles() {
    // Guards against the gate rejecting a variant that does work.
    let config = ActionNodeConfig::Code(CodeNodeConfig {
        standard: standard("rs"),
        language: CodeLanguage::Rust,
        code: json!({ "value": 1 }).to_string(),
        sandbox: None,
    });

    compile_with(config).expect("a rust code node is executable and must compile");
}

#[test]
fn an_ordinary_graph_still_compiles() {
    // The new per-node validation pass must not reject plain nodes.
    StateGraph::with_channels(&["value"])
        .add_node_fn("double", |ctx| async move {
            let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(value * 2)))
        })
        .add_edge(START, "double")
        .add_edge("double", END)
        .compile()
        .expect("a plain graph must still compile");
}
