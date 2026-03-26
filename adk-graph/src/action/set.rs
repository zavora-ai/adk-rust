//! Set action node executor.
//!
//! Supports three modes:
//! - **set**: Insert each variable into state.
//! - **merge**: Deep-merge each variable with existing state values.
//! - **delete**: Signal key removal by setting to null.

use std::collections::HashMap;

use adk_action::{SetMode, SetNodeConfig, interpolate_variables};
use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute a Set action node.
pub async fn execute_set(config: &SetNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    let state = &ctx.state;
    let mut output = NodeOutput::new();

    // Load environment variables if configured
    if let Some(env_cfg) = &config.env_vars {
        if env_cfg.load_from_env {
            for key in &env_cfg.keys {
                let env_key = if let Some(prefix) = &env_cfg.prefix {
                    format!("{prefix}{key}")
                } else {
                    key.clone()
                };
                if let Ok(val) = std::env::var(&env_key) {
                    output = output.with_update(key, Value::String(val));
                    tracing::debug!(key = %key, "loaded env var");
                }
            }
        }
    }

    match config.mode {
        SetMode::Set => {
            for var in &config.variables {
                let value = resolve_variable_value(&var.value, state);
                if var.is_secret {
                    tracing::debug!(key = %var.key, value = "***", "set secret variable");
                } else {
                    tracing::debug!(key = %var.key, value = %value, "set variable");
                }
                output = output.with_update(&var.key, value);
            }
        }
        SetMode::Merge => {
            for var in &config.variables {
                let new_value = resolve_variable_value(&var.value, state);
                let merged = if let Some(existing) = state.get(&var.key) {
                    deep_merge(existing.clone(), new_value)
                } else {
                    new_value
                };
                if var.is_secret {
                    tracing::debug!(key = %var.key, value = "***", "merged secret variable");
                } else {
                    tracing::debug!(key = %var.key, value = %merged, "merged variable");
                }
                output = output.with_update(&var.key, merged);
            }
        }
        SetMode::Delete => {
            for var in &config.variables {
                tracing::debug!(key = %var.key, "deleting variable");
                output = output.with_update(&var.key, Value::Null);
            }
        }
    }

    Ok(output)
}

/// Resolve a variable value, interpolating string templates.
fn resolve_variable_value(value: &Value, state: &HashMap<String, Value>) -> Value {
    match value {
        Value::String(s) => {
            let interpolated = interpolate_variables(s, state);
            // Try to parse as JSON if the original was a template
            if s.contains("{{") {
                // If the entire string was a single variable reference that resolved to
                // a non-string JSON value, try to parse it back
                if let Ok(parsed) = serde_json::from_str::<Value>(&interpolated) {
                    if !parsed.is_string() {
                        return parsed;
                    }
                }
            }
            Value::String(interpolated)
        }
        _ => value.clone(),
    }
}

/// Deep-merge two JSON values. Objects are merged recursively; other types are overwritten.
fn deep_merge(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Object(mut base_map), Value::Object(overlay_map)) => {
            for (key, overlay_val) in overlay_map {
                let merged = if let Some(base_val) = base_map.remove(&key) {
                    deep_merge(base_val, overlay_val)
                } else {
                    overlay_val
                };
                base_map.insert(key, merged);
            }
            Value::Object(base_map)
        }
        (_, overlay) => overlay,
    }
}

impl From<adk_action::ActionError> for GraphError {
    fn from(err: adk_action::ActionError) -> Self {
        GraphError::NodeExecutionFailed { node: String::new(), message: err.to_string() }
    }
}
