//! Transform action node executor.
//!
//! Supports:
//! - **template**: Interpolate `{{variable}}` patterns.
//! - **jsonpath**: Extract data using dot-notation paths.
//! - **builtin**: Reserved for built-in operations (not yet implemented).

use adk_action::{TransformNodeConfig, TransformType, get_nested_value, interpolate_variables};
use serde_json::Value;

use crate::error::Result;
use crate::node::{NodeContext, NodeOutput};

/// Execute a Transform action node.
pub async fn execute_transform(
    config: &TransformNodeConfig,
    ctx: &NodeContext,
) -> Result<NodeOutput> {
    let state = &ctx.state;
    let output_key = &config.standard.mapping.output_key;

    let result = match config.transform_type {
        TransformType::Template => {
            let template = config.template.as_deref().unwrap_or("");
            let interpolated = interpolate_variables(template, state);
            tracing::debug!(template = %template, result = %interpolated, "template transform");
            Value::String(interpolated)
        }
        TransformType::Jsonpath => {
            let expression = config.expression.as_deref().unwrap_or("");
            // Use dot-notation via get_nested_value
            let extracted = get_nested_value(state, expression).cloned().unwrap_or(Value::Null);
            tracing::debug!(expression = %expression, result = %extracted, "jsonpath transform");
            extracted
        }
        TransformType::Builtin => {
            // Builtin operations are a stretch goal; return null for now
            tracing::warn!("builtin transform operations not yet implemented");
            Value::Null
        }
    };

    // Apply type coercion if configured
    let result = if let Some(coercion) = &config.coercion {
        apply_coercion(result, &coercion.to_type)
    } else {
        result
    };

    Ok(NodeOutput::new().with_update(output_key, result))
}

/// Apply type coercion to a value.
fn apply_coercion(value: Value, to_type: &str) -> Value {
    match to_type {
        "string" => match &value {
            Value::String(_) => value,
            Value::Null => Value::String(String::new()),
            other => Value::String(other.to_string()),
        },
        "number" => match &value {
            Value::Number(_) => value,
            Value::String(s) => s
                .parse::<f64>()
                .map(|n| serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null))
                .unwrap_or(Value::Null),
            Value::Bool(b) => Value::Number(serde_json::Number::from(if *b { 1 } else { 0 })),
            _ => Value::Null,
        },
        "boolean" => match &value {
            Value::Bool(_) => value,
            Value::String(s) => Value::Bool(!s.is_empty() && s != "false" && s != "0"),
            Value::Number(n) => Value::Bool(n.as_f64().is_some_and(|f| f != 0.0)),
            Value::Null => Value::Bool(false),
            _ => Value::Bool(true),
        },
        "array" => match value {
            Value::Array(_) => value,
            Value::Null => Value::Array(vec![]),
            other => Value::Array(vec![other]),
        },
        _ => value,
    }
}
