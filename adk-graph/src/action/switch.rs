//! Switch action node executor.
//!
//! Evaluates typed conditions against graph state and routes to matching output ports.
//! Supports FirstMatch (first true condition) and AllMatch (all true conditions) modes.

use adk_action::{
    ActionError, EvaluationMode, ExpressionMode, SwitchCondition, SwitchNodeConfig,
    get_nested_value,
};
use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute a Switch action node.
pub async fn execute_switch(config: &SwitchNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    let state = &ctx.state;
    let node_id = &config.standard.id;

    match config.evaluation_mode {
        EvaluationMode::FirstMatch => {
            for condition in &config.conditions {
                if evaluate_condition(&condition.expression, state) {
                    tracing::debug!(
                        node = %node_id,
                        condition = %condition.name,
                        port = %condition.output_port,
                        "switch: first match"
                    );
                    return Ok(NodeOutput::new().with_update(
                        &config.standard.mapping.output_key,
                        Value::String(condition.output_port.clone()),
                    ));
                }
            }
            // No match — try default
            if let Some(default) = &config.default_branch {
                tracing::debug!(node = %node_id, branch = %default, "switch: default branch");
                Ok(NodeOutput::new().with_update(
                    &config.standard.mapping.output_key,
                    Value::String(default.clone()),
                ))
            } else {
                Err(GraphError::NodeExecutionFailed {
                    node: node_id.clone(),
                    message: ActionError::NoMatchingBranch { node_id: node_id.clone() }.to_string(),
                })
            }
        }
        EvaluationMode::AllMatch => {
            let matched: Vec<String> = config
                .conditions
                .iter()
                .filter(|c| evaluate_condition(&c.expression, state))
                .map(|c| c.output_port.clone())
                .collect();

            if matched.is_empty() {
                if let Some(default) = &config.default_branch {
                    tracing::debug!(node = %node_id, branch = %default, "switch: all-match default");
                    Ok(NodeOutput::new().with_update(
                        &config.standard.mapping.output_key,
                        Value::String(default.clone()),
                    ))
                } else {
                    Err(GraphError::NodeExecutionFailed {
                        node: node_id.clone(),
                        message: ActionError::NoMatchingBranch { node_id: node_id.clone() }
                            .to_string(),
                    })
                }
            } else {
                tracing::debug!(node = %node_id, ports = ?matched, "switch: all matches");
                let ports: Vec<Value> = matched.into_iter().map(Value::String).collect();
                Ok(NodeOutput::new()
                    .with_update(&config.standard.mapping.output_key, Value::Array(ports)))
            }
        }
    }
}

/// Evaluate switch conditions from a config against state. Returns matching output ports.
pub fn evaluate_switch_conditions(
    conditions: &[SwitchCondition],
    state: &std::collections::HashMap<String, Value>,
    mode: &EvaluationMode,
    default_branch: Option<&str>,
) -> std::result::Result<Vec<String>, ActionError> {
    let mut matched = Vec::new();

    for condition in conditions {
        if evaluate_condition(&condition.expression, state) {
            matched.push(condition.output_port.clone());
            if matches!(mode, EvaluationMode::FirstMatch) {
                return Ok(matched);
            }
        }
    }

    if matched.is_empty() {
        if let Some(default) = default_branch {
            Ok(vec![default.to_string()])
        } else {
            Err(ActionError::NoMatchingBranch { node_id: String::new() })
        }
    } else {
        Ok(matched)
    }
}

/// Evaluate a single condition expression against state.
fn evaluate_condition(
    expr: &ExpressionMode,
    state: &std::collections::HashMap<String, Value>,
) -> bool {
    let field_value = get_nested_value(state, &expr.field);
    let operator = expr.operator.as_str();
    let compare_value = &expr.value;

    match operator {
        "eq" => match_eq(field_value, compare_value),
        "neq" => !match_eq(field_value, compare_value),
        "gt" => compare_numeric(field_value, compare_value, |a, b| a > b),
        "lt" => compare_numeric(field_value, compare_value, |a, b| a < b),
        "gte" => compare_numeric(field_value, compare_value, |a, b| a >= b),
        "lte" => compare_numeric(field_value, compare_value, |a, b| a <= b),
        "contains" => match_contains(field_value, compare_value),
        "startsWith" => match_starts_with(field_value, compare_value),
        "endsWith" => match_ends_with(field_value, compare_value),
        "matches" => match_regex(field_value, compare_value),
        "in" => match_in(field_value, compare_value),
        "empty" => match_empty(field_value),
        "exists" => field_value.is_some(),
        _ => {
            tracing::warn!(operator = %operator, "unknown switch operator");
            false
        }
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn match_eq(field: Option<&Value>, compare: &str) -> bool {
    match field {
        Some(Value::String(s)) => s == compare,
        Some(Value::Number(n)) => {
            if let Ok(cmp) = compare.parse::<f64>() {
                n.as_f64().is_some_and(|f| (f - cmp).abs() < f64::EPSILON)
            } else {
                n.to_string() == compare
            }
        }
        Some(Value::Bool(b)) => {
            let cmp = compare == "true";
            *b == cmp
        }
        Some(Value::Null) => compare.is_empty() || compare == "null",
        None => compare.is_empty() || compare == "null",
        Some(v) => value_to_string(v) == compare,
    }
}

fn compare_numeric(field: Option<&Value>, compare: &str, op: fn(f64, f64) -> bool) -> bool {
    let field_num = field.and_then(|v| match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    });
    let compare_num = compare.parse::<f64>().ok();

    match (field_num, compare_num) {
        (Some(a), Some(b)) => op(a, b),
        _ => false,
    }
}

fn match_contains(field: Option<&Value>, compare: &str) -> bool {
    match field {
        Some(Value::String(s)) => s.contains(compare),
        Some(Value::Array(arr)) => arr.iter().any(|v| value_to_string(v) == compare),
        _ => false,
    }
}

fn match_starts_with(field: Option<&Value>, compare: &str) -> bool {
    match field {
        Some(Value::String(s)) => s.starts_with(compare),
        _ => false,
    }
}

fn match_ends_with(field: Option<&Value>, compare: &str) -> bool {
    match field {
        Some(Value::String(s)) => s.ends_with(compare),
        _ => false,
    }
}

fn match_regex(field: Option<&Value>, pattern: &str) -> bool {
    let s = match field {
        Some(v) => value_to_string(v),
        None => return false,
    };
    regex::Regex::new(pattern).is_ok_and(|re| re.is_match(&s))
}

fn match_in(field: Option<&Value>, compare: &str) -> bool {
    // compare is a comma-separated list of values
    let field_str = match field {
        Some(v) => value_to_string(v),
        None => return false,
    };
    compare.split(',').map(str::trim).any(|item| item == field_str)
}

fn match_empty(field: Option<&Value>) -> bool {
    match field {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        _ => false,
    }
}
