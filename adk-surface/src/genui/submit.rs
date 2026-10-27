//! Sending a drawn form (spec 3.3 §7.4 rule 5): the stored surface at the version the person
//! saw, its declared action, the person's values typed and checked per field, and the call's
//! arguments resolved on the server, constants taken from the stored surface.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use adk_core::{AdkError, DRAW_TOOL_NAMES, ErrorCategory, ErrorComponent, Event, Part};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::a2ui::DataModel;
use super::draw::DrawnEnvelope;
use super::drawn::{DrawnField, DrawnInput, DrawnNode};
use super::validate::{ActionArg, Validator};
use crate::error::SurfaceError;
use crate::words;

/// The code of a send at a version the agent has changed since.
pub const DRAWN_CHANGED_CODE: &str = "surface.drawn_changed";

/// A drawn form's send, ready to ask for its call.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Submission {
    /// The tool the action asks for.
    pub(crate) tool: String,
    /// The call's arguments.
    pub(crate) args: Value,
    /// The SHA-256 of the person's values, hex: the record carries this, never the values.
    pub(crate) values_digest: String,
    /// The agent that drew the form, by its name in the runtime.
    pub(crate) drawn_by: String,
}

/// The envelope of every drawing a render tool provably made in `events`, oldest first,
/// with the agent that drew it.
pub(crate) fn drawings(events: &[Event]) -> Vec<(DrawnEnvelope, String)> {
    let mut out = Vec::new();
    for event in events {
        let Some(content) = &event.llm_response.content else {
            continue;
        };
        for part in &content.parts {
            if let Part::FunctionResponse { function_response, .. } = part
                && DRAW_TOOL_NAMES.contains(&function_response.name.as_str())
                && let Some(envelope) = super::view::proven(event, &function_response.response)
            {
                out.push((envelope, event.author.clone()));
            }
        }
    }
    out
}

/// Whether `events` hold a call already asked for from `version` of `surface_id`.
fn already_sent(events: &[Event], surface_id: &str, version: u64) -> bool {
    events.iter().any(|event| {
        event.actions.tool_confirmation.is_some()
            && super::requested_via(event)
                .is_some_and(|via| via.surface_id == surface_id && via.version == version)
    })
}

fn changed() -> SurfaceError {
    SurfaceError::Source(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::Conflict,
        DRAWN_CHANGED_CODE,
        words::FORM_CHANGED_SEND_AGAIN,
    ))
}

/// The send of `action_id` on `version` of `surface_id` with the person's `values`, by data
/// path, checked in order (spec 3.3 §7.4 rule 5): the surface exists at `version` and is
/// its latest (`surface.drawn_changed`), the action is declared, and every value is typed
/// per its field and passes the field's checks (`surface.invalid`).
pub(crate) fn submission(
    events: &[Event],
    surface_id: &str,
    version: u64,
    action_id: &str,
    values: &Map<String, Value>,
) -> Result<Submission, SurfaceError> {
    let (latest, drawn_by) = drawings(events)
        .into_iter()
        .filter(|(envelope, _)| envelope.surface_id == surface_id)
        .max_by_key(|(envelope, _)| envelope.version)
        .ok_or_else(changed)?;
    if latest.version != version || latest.deleted {
        return Err(changed());
    }
    let draft = latest.draft().ok_or_else(changed)?;
    let validated = Validator::forms()
        .validate(&draft)
        .map_err(|refusal| SurfaceError::Invalid(refusal.message()))?;
    let Some(action) = validated.draft.actions.iter().find(|action| action.id == action_id) else {
        return Err(SurfaceError::Invalid(format!(
            "this form has no action '{action_id}'; send one of its own buttons"
        )));
    };
    if already_sent(events, surface_id, version) {
        return Err(SurfaceError::Invalid(
            "this form was sent already; the agent can draw it again".to_string(),
        ));
    }
    let tree = super::view::tree(&validated);
    let mut fields = BTreeMap::new();
    collect(&tree, &mut fields);
    let mut model = DataModel::new(validated.draft.data_model.clone());
    for (path, value) in values {
        let Some((field, label)) = fields.get(path.as_str()) else {
            return Err(SurfaceError::Invalid(format!("this form has no field at {path}")));
        };
        let typed = typed(field, value).map_err(|why| {
            SurfaceError::Invalid(format!("{}: {why}", label.as_deref().unwrap_or(path)))
        })?;
        model
            .apply(Some(path), Some(typed))
            .map_err(|error| SurfaceError::Invalid(error.to_string()))?;
    }
    for (field, label) in fields.values() {
        for check in &field.checks {
            if !holds(&check.condition, &model) {
                return Err(SurfaceError::Invalid(match label {
                    Some(label) => format!("{label}: {}", check.message),
                    None => check.message.clone(),
                }));
            }
        }
    }
    let args: Map<String, Value> = action
        .args
        .iter()
        .map(|(name, arg)| {
            let value = match arg {
                ActionArg::Constant(value) => value.clone(),
                ActionArg::Field(field) => model.get(&field.path).cloned().unwrap_or(Value::Null),
            };
            (name.clone(), value)
        })
        .collect();
    let mut canonical: Vec<(&String, &Value)> = values.iter().collect();
    canonical.sort_by(|a, b| a.0.cmp(b.0));
    let digest = Sha256::digest(json!(canonical).to_string().as_bytes());
    let values_digest = digest.iter().fold(String::with_capacity(64), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    Ok(Submission { tool: action.tool.clone(), args: Value::Object(args), values_digest, drawn_by })
}

/// Every field in `node`, by path, with its label.
fn collect<'a>(node: &'a DrawnNode, out: &mut BTreeMap<&'a str, (&'a DrawnField, Option<String>)>) {
    if let Some(field) = &node.field {
        out.insert(field.path.as_str(), (field, node.text.clone()));
    }
    for child in &node.children {
        collect(child, out);
    }
}

/// `value` as `field` takes it, or why it does not.
pub(crate) fn typed(field: &DrawnField, value: &Value) -> Result<Value, String> {
    let text = || value.as_str().ok_or_else(|| "expects text".to_string());
    match field.input {
        DrawnInput::ShortText | DrawnInput::LongText => text().map(|text| json!(text)),
        DrawnInput::Date => {
            let text = text()?;
            if !text.is_empty() && NaiveDate::parse_from_str(text, "%Y-%m-%d").is_err() {
                return Err("expects a date such as 2026-11-15".to_string());
            }
            Ok(json!(text))
        }
        DrawnInput::Time => {
            let text = text()?;
            if !text.is_empty()
                && NaiveTime::parse_from_str(text, "%H:%M").is_err()
                && NaiveTime::parse_from_str(text, "%H:%M:%S").is_err()
            {
                return Err("expects a time such as 10:30".to_string());
            }
            Ok(json!(text))
        }
        DrawnInput::DateTime => {
            let text = text()?;
            if !text.is_empty()
                && NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M").is_err()
                && NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").is_err()
                && chrono::DateTime::parse_from_rfc3339(text).is_err()
            {
                return Err("expects a date and time such as 2026-11-15T10:30".to_string());
            }
            Ok(json!(text))
        }
        DrawnInput::Number | DrawnInput::Slider => {
            let number = match value {
                Value::Number(number) => Some(number.clone()),
                Value::String(text) if text.trim().is_empty() => return Ok(Value::Null),
                Value::String(text) => {
                    text.trim().parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(
                        |number| match number.as_f64() {
                            Some(whole) if whole.fract() == 0.0 && whole.abs() < 9.0e15 => {
                                serde_json::Number::from(whole as i64)
                            }
                            _ => number,
                        },
                    )
                }
                Value::Null => return Ok(Value::Null),
                _ => None,
            };
            let number = number.ok_or_else(|| "expects a number".to_string())?;
            if field.input == DrawnInput::Slider {
                let at = number.as_f64().unwrap_or_default();
                let below = field.min.as_ref().and_then(Value::as_f64).is_some_and(|min| at < min);
                let above = field.max.as_ref().and_then(Value::as_f64).is_some_and(|max| at > max);
                if below || above {
                    return Err("is out of range".to_string());
                }
            }
            Ok(Value::Number(number))
        }
        DrawnInput::Checkbox => {
            value.as_bool().map(Value::Bool).ok_or_else(|| "expects yes or no".to_string())
        }
        DrawnInput::Choice | DrawnInput::Choices => {
            let chosen: Vec<&str> = match value {
                Value::String(one) => vec![one.as_str()],
                Value::Array(items) => items
                    .iter()
                    .map(|item| item.as_str().ok_or_else(|| "expects its options".to_string()))
                    .collect::<Result<_, _>>()?,
                _ => return Err("expects its options".to_string()),
            };
            if field.input == DrawnInput::Choice && chosen.len() > 1 {
                return Err("takes one option".to_string());
            }
            if let Some(unknown) = chosen
                .iter()
                .find(|value| !field.options.iter().any(|option| option.value == **value))
            {
                return Err(format!("has no option '{unknown}'"));
            }
            Ok(json!(chosen))
        }
    }
}

/// The value `value` stands for: a binding's value in `model`, or itself.
fn resolve<'a>(value: &'a Value, model: &'a DataModel) -> Option<&'a Value> {
    match value {
        Value::Object(map) if map.len() == 1 && map.contains_key("path") => {
            model.get(map.get("path")?.as_str()?)
        }
        other => Some(other),
    }
}

/// Whether the check `condition` holds over `model`: the catalog's `required`, `regex`,
/// `length`, `numeric`, `email`, `and`, `or` and `not`. A condition this home cannot read
/// holds, since the validator refused every other function.
pub(crate) fn holds(condition: &Value, model: &DataModel) -> bool {
    let truth = |value: &Value| match value {
        Value::Bool(flag) => *flag,
        Value::Object(map) if map.contains_key("call") => holds(value, model),
        other => resolve(other, model).and_then(Value::as_bool).unwrap_or(false),
    };
    let Some(call) = condition.get("call").and_then(Value::as_str) else {
        return truth(condition);
    };
    let args = condition.get("args").and_then(Value::as_object);
    let arg = |name: &str| args.and_then(|args| args.get(name));
    let value = arg("value").and_then(|value| resolve(value, model));
    let text = || match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    };
    let bound = |name: &str| arg(name).and_then(Value::as_f64);
    match call {
        "required" => match value {
            None | Some(Value::Null) => false,
            Some(Value::String(text)) => !text.trim().is_empty(),
            Some(Value::Array(items)) => !items.is_empty(),
            Some(_) => true,
        },
        "email" => {
            let text = text();
            text.is_empty() || {
                let mut parts = text.splitn(2, '@');
                let (local, domain) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                !local.is_empty()
                    && domain.contains('.')
                    && !domain.starts_with('.')
                    && !domain.ends_with('.')
                    && !text.contains(char::is_whitespace)
                    && !domain.contains('@')
            }
        }
        "regex" => {
            let text = text();
            let Some(pattern) = arg("pattern").and_then(Value::as_str) else {
                return true;
            };
            text.is_empty()
                || regex::Regex::new(&format!("^(?:{pattern})$"))
                    .is_ok_and(|regex| regex.is_match(&text))
        }
        "length" => {
            let count = text().chars().count() as f64;
            bound("min").is_none_or(|min| count >= min)
                && bound("max").is_none_or(|max| count <= max)
        }
        "numeric" => {
            let number = match value {
                Some(Value::Number(number)) => number.as_f64(),
                Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
                _ => None,
            };
            match number {
                // A blank field has no number to check, as on the web page.
                None => match value {
                    None | Some(Value::Null) => true,
                    Some(Value::String(text)) => text.trim().is_empty(),
                    Some(_) => false,
                },
                Some(number) => {
                    bound("min").is_none_or(|min| number >= min)
                        && bound("max").is_none_or(|max| number <= max)
                }
            }
        }
        "and" | "or" => {
            let values: Vec<bool> =
                arg("values").and_then(Value::as_array).into_iter().flatten().map(truth).collect();
            if call == "and" {
                values.iter().all(|value| *value)
            } else {
                values.iter().any(|value| *value)
            }
        }
        "not" => !arg("value").is_some_and(truth),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blank field has no number to check, as on the web page, so an optional quantity
    /// left blank is sent; a number out of range is not.
    #[test]
    fn a_blank_number_passes_a_numeric_check() {
        let check = json!({ "call": "numeric", "args": { "value": { "path": "/qty" }, "min": 1 } });
        for blank in [json!(""), json!("  "), Value::Null] {
            assert!(holds(&check, &DataModel::new(json!({ "qty": blank }))), "{blank}");
        }
        assert!(!holds(&check, &DataModel::new(json!({ "qty": "0" }))));
        assert!(!holds(&check, &DataModel::new(json!({ "qty": "lots" }))));
    }
}
