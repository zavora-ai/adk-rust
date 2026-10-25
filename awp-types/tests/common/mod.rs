//! Shared helpers for the AWP 2.0 tests (spec 3.5 §7.2–§7.10).

#![allow(dead_code)]

use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Object keys whose children are data (paths, question ids, field names, categories), not
/// field names; their values are still checked.
const DATA_KEYED: &[&str] = &["files", "answers", "fieldBounds", "caps"];

/// Object keys holding free-form JSON that is not checked at all.
const OPAQUE: &[&str] = &["fields", "resource", "inputSchema", "outputSchema", "jsonLd", "result"];

/// Serializes `value`, parses it back, asserts equality, and asserts every field name is
/// camelCase; returns the JSON value.
pub fn round_trip<T>(value: &T) -> Value
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let text = serde_json::to_string(value).expect("serializes");
    let back: T = serde_json::from_str(&text).expect("parses back");
    assert_eq!(&back, value, "round trip changed the value: {text}");
    let json = serde_json::to_value(value).expect("serializes to a value");
    let back: T = serde_json::from_value(json.clone()).expect("parses back from a value");
    assert_eq!(&back, value);
    assert_camel_case(&json, "$");
    json
}

/// Asserts every field name in `value` is camelCase.
pub fn assert_camel_case(value: &Value, path: &str) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                assert!(is_camel_case(key), "field `{key}` at {path} is not camelCase");
                let child_path = format!("{path}.{key}");
                if OPAQUE.contains(&key.as_str()) {
                    continue;
                }
                if DATA_KEYED.contains(&key.as_str()) {
                    if let Value::Object(entries) = child {
                        for (entry_key, entry) in entries {
                            assert_camel_case(entry, &format!("{child_path}[{entry_key}]"));
                        }
                    }
                    continue;
                }
                assert_camel_case(child, &child_path);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                assert_camel_case(item, &format!("{path}[{index}]"));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn is_camel_case(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

/// Returns the wire value of a unit-like enum value.
pub fn wire<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("serializes")
}
