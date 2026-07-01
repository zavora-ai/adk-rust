//! Marshalling between the framework's JSON values and Monty's [`MontyObject`].
//!
//! The CodeAct seam speaks `serde_json::Value`: a [`PendingCall`] surfaces tool
//! arguments as JSON, the driver feeds a tool's JSON result back in, and a
//! finished script reports its value as JSON. Monty speaks [`MontyObject`]. These
//! two functions bridge the gap.
//!
//! [`PendingCall`]: adk_agent::codeact::PendingCall

use monty::{DictPairs, JsonMontyObject, MontyObject};
use serde_json::Value;

/// Convert a host JSON value into a Monty value, to be injected into a script
/// (a tool result, a resolved name, ...).
///
/// The mapping is the obvious one. A JSON integer that fits in `i64` becomes an
/// `int`; anything larger (or fractional) becomes a `float`. Objects become
/// `dict`s keyed by their string keys, preserving insertion order.
pub(crate) fn json_to_monty(value: Value) -> MontyObject {
    match value {
        Value::Null => MontyObject::None,
        Value::Bool(b) => MontyObject::Bool(b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                MontyObject::Int(i)
            } else {
                // u64 values above i64::MAX and all non-integral numbers fall
                // back to float — good enough for tool I/O, which is rarely
                // built around > 2^63 integers.
                MontyObject::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => MontyObject::String(s),
        Value::Array(items) => MontyObject::List(items.into_iter().map(json_to_monty).collect()),
        Value::Object(map) => {
            let pairs: Vec<(MontyObject, MontyObject)> =
                map.into_iter().map(|(k, v)| (MontyObject::String(k), json_to_monty(v))).collect();
            MontyObject::Dict(DictPairs::from(pairs))
        }
    }
}

/// Convert a Monty value produced by a script into host JSON.
///
/// Uses Monty's [`JsonMontyObject`] "natural" projection: JSON-native Python
/// values serialize bare (`42`, `"hi"`, `[...]`, `{"a": 1}`); the rare
/// non-JSON-native value (a `tuple`, `bytes`, ...) uses Monty's `{"$tag": ...}`
/// convention. The CodeAct contract only cares that the top-level completion
/// value is a tagged object, which a script's final `dict` expression always is.
pub(crate) fn monty_to_json(obj: &MontyObject) -> Value {
    serde_json::to_value(JsonMontyObject(obj)).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_json_native_values() {
        let cases = [
            json!(null),
            json!(true),
            json!(42),
            json!(3.5),
            json!("hello"),
            json!([1, 2, 3]),
            json!({"type": "final_result", "value": {"n": 7}}),
        ];
        for case in cases {
            let monty = json_to_monty(case.clone());
            assert_eq!(monty_to_json(&monty), case);
        }
    }

    #[test]
    fn large_u64_degrades_to_float() {
        let big = json!(u64::MAX);
        assert!(matches!(json_to_monty(big), MontyObject::Float(_)));
    }
}
