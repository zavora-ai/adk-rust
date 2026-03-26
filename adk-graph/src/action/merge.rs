//! Merge action node executor.
//!
//! Since we're in a sequential graph executor, merge collects branch results
//! from state rather than waiting for concurrent branches.
//!
//! Supports:
//! - **waitAll**: Collect all branch results.
//! - **waitAny**: Take the first available branch result.
//! - **waitN**: Require N branch results.
//!
//! Combine strategies: array, object, first, last.

use adk_action::{ActionError, CombineStrategy, MergeMode, MergeNodeConfig};
use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute a Merge action node.
///
/// In a sequential graph, "branches" are represented as state keys following
/// the convention `branch:<branch_name>`. The merge node collects these values.
pub async fn execute_merge(config: &MergeNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    let state = &ctx.state;
    let node_id = &config.standard.id;
    let output_key = &config.standard.mapping.output_key;

    // Collect branch results from state — look for keys prefixed with "branch:"
    let branch_results: Vec<(String, Value)> = state
        .iter()
        .filter(|(k, _)| k.starts_with("branch:"))
        .map(|(k, v)| {
            let branch_name = k.strip_prefix("branch:").unwrap_or(k).to_string();
            (branch_name, v.clone())
        })
        .collect();

    let branch_count = branch_results.len() as u32;

    match config.mode {
        MergeMode::WaitAll => {
            // In sequential mode, all branches that exist in state are "complete"
            tracing::debug!(
                node = %node_id,
                branches = branch_count,
                "merge: waitAll"
            );
        }
        MergeMode::WaitAny => {
            if branch_results.is_empty() {
                return Err(GraphError::NodeExecutionFailed {
                    node: node_id.clone(),
                    message: ActionError::NoBranchCompleted { node_id: node_id.clone() }
                        .to_string(),
                });
            }
            tracing::debug!(node = %node_id, "merge: waitAny — using first available");
        }
        MergeMode::WaitN => {
            let required = config.required_count.unwrap_or(1);
            if branch_count < required {
                return Err(GraphError::NodeExecutionFailed {
                    node: node_id.clone(),
                    message: ActionError::InsufficientBranches {
                        got: branch_count,
                        need: required,
                    }
                    .to_string(),
                });
            }
            tracing::debug!(
                node = %node_id,
                got = branch_count,
                required = required,
                "merge: waitN"
            );
        }
    }

    // Apply combine strategy
    let combined = combine_results(&branch_results, &config.combine_strategy);

    Ok(NodeOutput::new().with_update(output_key, combined))
}

/// Combine branch results using the configured strategy.
fn combine_results(branches: &[(String, Value)], strategy: &CombineStrategy) -> Value {
    match strategy {
        CombineStrategy::Array => {
            let values: Vec<Value> = branches.iter().map(|(_, v)| v.clone()).collect();
            Value::Array(values)
        }
        CombineStrategy::Object => {
            let map: serde_json::Map<String, Value> =
                branches.iter().map(|(name, v)| (name.clone(), v.clone())).collect();
            Value::Object(map)
        }
        CombineStrategy::First => branches.first().map(|(_, v)| v.clone()).unwrap_or(Value::Null),
        CombineStrategy::Last => branches.last().map(|(_, v)| v.clone()).unwrap_or(Value::Null),
    }
}
