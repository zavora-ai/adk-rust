//! Wait action node executor.
//!
//! Supports:
//! - **fixed**: Sleep for a configured duration.
//! - **condition**: Poll a condition at intervals until true or timeout.
//! - **until**: Sleep until a specific RFC3339 timestamp.

use adk_action::{ActionError, WaitNodeConfig, WaitType, interpolate_variables};
use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute a Wait action node.
pub async fn execute_wait(config: &WaitNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    let node_id = &config.standard.id;
    let output_key = &config.standard.mapping.output_key;

    match config.wait_type {
        WaitType::Fixed => execute_fixed(config, node_id, output_key).await,
        WaitType::Condition => execute_condition(config, ctx, node_id, output_key).await,
        WaitType::Until => execute_until(config, node_id, output_key).await,
        WaitType::Webhook => {
            // Webhook wait requires the action-trigger feature
            tracing::warn!(node = %node_id, "webhook wait not supported in core action feature");
            Ok(NodeOutput::new().with_update(output_key, Value::Null))
        }
    }
}

async fn execute_fixed(
    config: &WaitNodeConfig,
    node_id: &str,
    output_key: &str,
) -> Result<NodeOutput> {
    let fixed = config.fixed.as_ref().ok_or_else(|| GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: "fixed wait missing duration configuration".into(),
    })?;

    let duration_ms = convert_to_ms(fixed.duration, &fixed.unit);
    tracing::debug!(node = %node_id, duration_ms = duration_ms, "fixed wait");

    tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;

    Ok(NodeOutput::new().with_update(output_key, serde_json::json!({ "waited_ms": duration_ms })))
}

async fn execute_condition(
    config: &WaitNodeConfig,
    ctx: &NodeContext,
    node_id: &str,
    output_key: &str,
) -> Result<NodeOutput> {
    let polling = config.condition.as_ref().ok_or_else(|| GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: "condition wait missing polling configuration".into(),
    })?;

    let interval = std::time::Duration::from_millis(polling.interval_ms);
    let max_wait = std::time::Duration::from_millis(polling.max_wait_ms);
    let start = std::time::Instant::now();

    loop {
        // Evaluate condition against current state
        let resolved = interpolate_variables(&polling.condition, &ctx.state);
        let trimmed = resolved.trim().to_lowercase();
        let is_true =
            !trimmed.is_empty() && trimmed != "false" && trimmed != "0" && trimmed != "null";

        if is_true {
            tracing::debug!(node = %node_id, elapsed_ms = ?start.elapsed().as_millis(), "condition met");
            return Ok(NodeOutput::new().with_update(
                output_key,
                serde_json::json!({ "condition_met": true, "elapsed_ms": start.elapsed().as_millis() as u64 }),
            ));
        }

        if start.elapsed() >= max_wait {
            return Err(GraphError::NodeExecutionFailed {
                node: node_id.to_string(),
                message: ActionError::ConditionTimeout { ms: polling.max_wait_ms }.to_string(),
            });
        }

        tokio::time::sleep(interval).await;
    }
}

async fn execute_until(
    config: &WaitNodeConfig,
    node_id: &str,
    output_key: &str,
) -> Result<NodeOutput> {
    let until = config.until.as_ref().ok_or_else(|| GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: "until wait missing timestamp configuration".into(),
    })?;

    let target = chrono::DateTime::parse_from_rfc3339(&until.timestamp).map_err(|e| {
        GraphError::NodeExecutionFailed {
            node: node_id.to_string(),
            message: ActionError::InvalidTimestamp(format!(
                "invalid RFC3339 timestamp '{}': {e}",
                until.timestamp
            ))
            .to_string(),
        }
    })?;

    let now = chrono::Utc::now();
    let target_utc = target.with_timezone(&chrono::Utc);

    if target_utc > now {
        let duration = (target_utc - now).to_std().unwrap_or(std::time::Duration::ZERO);
        tracing::debug!(
            node = %node_id,
            target = %until.timestamp,
            wait_ms = duration.as_millis() as u64,
            "until wait"
        );
        tokio::time::sleep(duration).await;
    } else {
        tracing::debug!(node = %node_id, "until target already passed");
    }

    Ok(NodeOutput::new()
        .with_update(output_key, serde_json::json!({ "target": until.timestamp, "reached": true })))
}

/// Convert a duration value with unit to milliseconds.
fn convert_to_ms(duration: u64, unit: &str) -> u64 {
    match unit {
        "ms" => duration,
        "s" => duration * 1000,
        "m" => duration * 60 * 1000,
        "h" => duration * 60 * 60 * 1000,
        _ => duration, // default to ms
    }
}
