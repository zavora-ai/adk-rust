//! Trigger action node executor.
//!
//! Handles manual trigger execution. The manual trigger returns metadata about
//! how the graph was triggered, including trigger type, timestamp, and any
//! input from the graph's initial state.

use adk_action::{TriggerNodeConfig, TriggerType};
use serde_json::json;

use crate::error::Result;
use crate::node::{NodeContext, NodeOutput};

/// Execute a Trigger action node.
///
/// For manual triggers, returns a `NodeOutput` containing:
/// - `trigger_type`: "manual"
/// - `timestamp`: current UTC ISO-8601 timestamp
/// - `input`: the value from state matching the configured `input_label`, or the `default_prompt`
pub async fn execute_trigger(config: &TriggerNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    match config.trigger_type {
        TriggerType::Manual => execute_manual_trigger(config, ctx),
        // Webhook, Schedule, and Event triggers are entry points managed by
        // TriggerRuntime (see trigger_runtime.rs). When encountered as a node
        // in the graph, they simply return metadata about the trigger config.
        TriggerType::Webhook => Ok(NodeOutput::new().with_update(
            &config.standard.mapping.output_key,
            json!({
                "trigger_type": "webhook",
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "path": config.webhook.as_ref().map(|w| w.path.as_str()).unwrap_or(""),
            }),
        )),
        TriggerType::Schedule => Ok(NodeOutput::new().with_update(
            &config.standard.mapping.output_key,
            json!({
                "trigger_type": "schedule",
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "cron": config.schedule.as_ref().map(|s| s.cron.as_str()).unwrap_or(""),
            }),
        )),
        TriggerType::Event => Ok(NodeOutput::new().with_update(
            &config.standard.mapping.output_key,
            json!({
                "trigger_type": "event",
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "source": config.event.as_ref().map(|e| e.source.as_str()).unwrap_or(""),
                "event_type": config.event.as_ref().map(|e| e.event_type.as_str()).unwrap_or(""),
            }),
        )),
    }
}

/// Execute a manual trigger node.
///
/// Reads input from state using the configured `input_label` key. If no input
/// is found in state, falls back to the configured `default_prompt`.
fn execute_manual_trigger(config: &TriggerNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    let manual_config = config.manual.as_ref();

    // Determine the input label (defaults to "input")
    let input_label = manual_config.and_then(|m| m.input_label.as_deref()).unwrap_or("input");

    // Try to get input from state using the input_label key
    let input_value = ctx.state.get(input_label).cloned().unwrap_or_else(|| {
        // Fall back to default_prompt if no input in state
        manual_config
            .and_then(|m| m.default_prompt.as_deref())
            .map(|p| json!(p))
            .unwrap_or(json!(null))
    });

    let output = json!({
        "trigger_type": "manual",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "input": input_value,
        "input_label": input_label,
    });

    Ok(NodeOutput::new().with_update(&config.standard.mapping.output_key, output))
}
