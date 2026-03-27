//! Code action node executor.
//!
//! - **Rust mode**: Evaluates the code field as a JSON expression or returns it as a string.
//!   (Dynamic Rust compilation is not possible at runtime.)
//! - **JS/TS mode**: Gated behind `action-code` feature; returns an error if not enabled.

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

/// JS/TS code execution — requires the `action-code` feature.
///
/// Currently a placeholder. The `action-code` feature flag is defined but the
/// `quick-js` dependency is not yet wired. When enabled, this will provide a
/// sandboxed JavaScript/TypeScript runtime with configurable resource limits
/// (memory, time, network, filesystem).
fn execute_js_code(node_id: &str) -> Result<NodeOutput> {
    Err(GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: concat!(
            "JavaScript/TypeScript code execution is not yet available. ",
            "The 'action-code' feature is reserved for a future sandboxed JS runtime ",
            "(quick-js). Use language 'rust' for code nodes, or contribute the ",
            "quick-js integration to enable JS/TS support."
        )
        .to_string(),
    })
}
