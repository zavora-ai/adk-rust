//! Loop action node executor.
//!
//! Supports three loop types:
//! - **forEach**: Iterate over a source array.
//! - **while**: Repeat while a condition is true (safety limit 1000).
//! - **times**: Repeat N times.
//!
//! Parallel execution is a stretch goal — currently sequential only.

use adk_action::{LoopNodeConfig, LoopType, interpolate_variables};
use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute a Loop action node.
pub async fn execute_loop(config: &LoopNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    let state = &ctx.state;
    let output_key = &config.standard.mapping.output_key;
    let node_id = &config.standard.id;

    let collect = config.results.as_ref().is_some_and(|r| r.collect);
    let aggregation_key =
        config.results.as_ref().and_then(|r| r.aggregation_key.as_deref()).unwrap_or(output_key);

    match config.loop_type {
        LoopType::ForEach => execute_for_each(config, state, node_id, collect, aggregation_key),
        LoopType::While => execute_while(config, state, node_id, collect, aggregation_key),
        LoopType::Times => execute_times(config, state, node_id, collect, aggregation_key),
    }
}

fn execute_for_each(
    config: &LoopNodeConfig,
    state: &std::collections::HashMap<String, Value>,
    node_id: &str,
    collect: bool,
    aggregation_key: &str,
) -> Result<NodeOutput> {
    let for_each = config.for_each.as_ref().ok_or_else(|| GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: "forEach loop missing for_each configuration".into(),
    })?;

    // Resolve the source array from state
    let source = state.get(&for_each.source).cloned().unwrap_or(Value::Null);
    let items = match &source {
        Value::Array(arr) => arr.clone(),
        Value::Null => vec![],
        other => vec![other.clone()],
    };

    let mut output = NodeOutput::new();
    let mut results = Vec::new();

    for (index, item) in items.iter().enumerate() {
        tracing::debug!(
            node = %node_id,
            index = index,
            total = items.len(),
            "forEach iteration"
        );
        // Set item and index variables in output
        output = output.with_update(&for_each.item_var, item.clone());
        output = output.with_update(&for_each.index_var, Value::Number(index.into()));

        if collect {
            results.push(item.clone());
        }
    }

    if collect {
        output = output.with_update(aggregation_key, Value::Array(results));
    }

    // Store iteration count
    output = output.with_update(
        &config.standard.mapping.output_key,
        serde_json::json!({ "iterations": items.len() }),
    );

    Ok(output)
}

fn execute_while(
    config: &LoopNodeConfig,
    state: &std::collections::HashMap<String, Value>,
    node_id: &str,
    collect: bool,
    aggregation_key: &str,
) -> Result<NodeOutput> {
    let while_cfg =
        config.while_config.as_ref().ok_or_else(|| GraphError::NodeExecutionFailed {
            node: node_id.to_string(),
            message: "while loop missing while_config configuration".into(),
        })?;

    let max_iterations = while_cfg.max_iterations as usize;
    let mut output = NodeOutput::new();
    let mut results = Vec::new();
    let mut iteration = 0;

    // Evaluate condition by interpolating and checking truthiness.
    // In a sequential executor without sub-graph execution, the state doesn't
    // change between iterations, so we evaluate once and either run 0 or 1 times.
    let resolved = interpolate_variables(&while_cfg.condition, state);
    let is_true = is_truthy(&resolved);

    if is_true && max_iterations > 0 {
        tracing::debug!(node = %node_id, iteration = 0, "while iteration");
        iteration = 1;
        if collect {
            results.push(Value::Number(0.into()));
        }
    }

    if collect {
        output = output.with_update(aggregation_key, Value::Array(results));
    }

    output = output.with_update(
        &config.standard.mapping.output_key,
        serde_json::json!({ "iterations": iteration }),
    );

    Ok(output)
}

fn execute_times(
    config: &LoopNodeConfig,
    state: &std::collections::HashMap<String, Value>,
    node_id: &str,
    collect: bool,
    aggregation_key: &str,
) -> Result<NodeOutput> {
    let times_cfg = config.times.as_ref().ok_or_else(|| GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: "times loop missing times configuration".into(),
    })?;

    let count = times_cfg.count as usize;
    let _ = state; // state available for future sub-graph execution
    let mut output = NodeOutput::new();
    let mut results = Vec::new();

    for i in 0..count {
        tracing::debug!(node = %node_id, index = i, total = count, "times iteration");
        output = output.with_update(&times_cfg.index_var, Value::Number(i.into()));

        if collect {
            results.push(Value::Number(i.into()));
        }
    }

    if collect {
        output = output.with_update(aggregation_key, Value::Array(results));
    }

    output = output.with_update(
        &config.standard.mapping.output_key,
        serde_json::json!({ "iterations": count }),
    );

    Ok(output)
}

/// Check if a string value is "truthy".
fn is_truthy(s: &str) -> bool {
    let trimmed = s.trim().to_lowercase();
    !trimmed.is_empty() && trimmed != "false" && trimmed != "0" && trimmed != "null"
}
