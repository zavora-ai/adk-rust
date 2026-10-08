//! Code action node executor.
//!
//! - **Rust mode**: Evaluates the code field as a JSON expression or returns it as a string.
//!   (Dynamic Rust compilation is not possible at runtime.)
//! - **JS/TS mode**: Not implemented under any feature; returns an error saying so.

use adk_action::{CodeLanguage, CodeNodeConfig};
use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute a Code action node.
pub async fn execute_code(config: &CodeNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    let node_id = &config.standard.id;
    let output_key = &config.standard.mapping.output_key;

    match config.language {
        CodeLanguage::Rust => execute_rust_code(config, ctx, node_id, output_key),
        CodeLanguage::Javascript | CodeLanguage::Typescript => execute_js_code(node_id),
    }
}

/// Execute Rust code mode.
///
/// Since we cannot dynamically compile Rust at runtime, we treat the code
/// field as either a JSON expression to evaluate or a string value to store.
fn execute_rust_code(
    config: &CodeNodeConfig,
    ctx: &NodeContext,
    node_id: &str,
    output_key: &str,
) -> Result<NodeOutput> {
    let code = &config.code;

    tracing::debug!(node = %node_id, code_len = code.len(), "executing rust code node");

    // Try to parse the code as a JSON value first
    let result = if let Ok(json_value) = serde_json::from_str::<Value>(code) {
        json_value
    } else {
        // If it's not valid JSON, interpolate variables and return as string
        let state = &ctx.state;
        let interpolated = adk_action::interpolate_variables(code, state);
        Value::String(interpolated)
    };

    Ok(NodeOutput::new().with_update(output_key, result))
}

/// JS/TS code execution: not implemented.
///
/// No sandboxed JavaScript runtime is integrated, and the `action-code` feature
/// does not add one.
fn execute_js_code(node_id: &str) -> Result<NodeOutput> {
    Err(GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: "JavaScript and TypeScript code nodes are not implemented, in any feature \
                  configuration; use language 'rust'"
            .to_string(),
    })
}
