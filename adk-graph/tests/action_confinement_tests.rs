//! Action nodes take paths and URLs from workflow state, which is untrusted.
//!
//! A file node used the interpolated path as given, so `../` or an absolute
//! path reached any file the process could. An HTTP node requested any URL the
//! state produced. Both are now confined.

#![cfg(feature = "action")]

use std::path::PathBuf;

use adk_action::{
    ActionNodeConfig, Callbacks, ErrorHandling, ErrorMode, ExecutionControl, FileNodeConfig,
    FileOperation, FileWriteConfig, InputOutputMapping, LocalFileConfig, LogLevel,
    StandardProperties, Tracing,
};
use adk_graph::action::ActionNodeExecutor;
use adk_graph::error::GraphError;
use adk_graph::node::{ExecutionConfig, Node, NodeContext};
use adk_graph::state::State;
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
        mapping: InputOutputMapping { input_mapping: None, output_key: "result".to_string() },
    }
}

/// Runs `executor` once against `state`, returning the failure message.
async fn failure(executor: &ActionNodeExecutor, state: State) -> String {
    let ctx = NodeContext::new(state, ExecutionConfig::new("confinement"), 0);
    match executor.execute(&ctx).await {
        Err(GraphError::NodeExecutionFailed { message, .. }) => message,
        Err(other) => panic!("expected a node failure, got {other}"),
        Ok(_) => panic!("expected a node failure, but the node succeeded"),
    }
}

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("adk-graph-action-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_node(path_template: &str) -> ActionNodeConfig {
    ActionNodeConfig::File(FileNodeConfig {
        standard: standard("write"),
        operation: FileOperation::Write,
        local: Some(LocalFileConfig { path: path_template.to_string() }),
        cloud: None,
        parse: None,
        write: Some(FileWriteConfig {
            content: json!("payload"),
            create_dirs: true,
            append: false,
        }),
        list: None,
    })
}

#[tokio::test]
async fn a_file_path_escaping_the_root_through_parent_components_is_rejected() {
    let workspace = TempDir::new();
    let root = workspace.0.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let executor = ActionNodeExecutor::new(write_node("{{target}}")).with_file_roots([&root]);

    let state = State::from([("target".to_string(), json!("../escaped.txt"))]);
    let message = failure(&executor, state).await;

    assert!(message.contains("contains '..'"), "{message}");
    assert!(!workspace.0.join("escaped.txt").exists(), "nothing may be written outside the root");
}

#[tokio::test]
async fn a_file_path_inside_the_root_is_written() {
    let root = TempDir::new();
    let executor = ActionNodeExecutor::new(write_node("{{target}}")).with_file_roots([&root.0]);

    let state = State::from([("target".to_string(), json!("nested/out.txt"))]);
    let ctx = NodeContext::new(state, ExecutionConfig::new("confinement"), 0);
    executor.execute(&ctx).await.expect("a path inside the root is permitted");

    assert_eq!(std::fs::read_to_string(root.0.join("nested/out.txt")).unwrap(), "payload");
}

#[cfg(feature = "action-http")]
#[tokio::test]
async fn a_file_url_is_rejected_by_an_http_node() {
    use adk_action::{HttpAuth, HttpBody, HttpMethod, HttpNodeConfig, HttpResponse};
    use std::collections::HashMap;

    let executor = ActionNodeExecutor::new(ActionNodeConfig::Http(HttpNodeConfig {
        standard: standard("fetch"),
        method: HttpMethod::Get,
        url: "{{url}}".to_string(),
        auth: HttpAuth::None,
        headers: HashMap::new(),
        body: HttpBody::None,
        response: HttpResponse { response_type: "text".to_string(), status_validation: None },
        rate_limit: None,
    }));

    let state = State::from([("url".to_string(), json!("file:///etc/passwd"))]);
    let message = failure(&executor, state).await;

    assert!(message.contains("scheme 'file' is not permitted"), "{message}");
}
