/// Serialize a tool result `Value` into a string suitable for model provider APIs.
///
/// This avoids double-encoding: when the value is already a `String`, it is returned
/// as-is. JSON objects and arrays are serialized to their JSON text representation.
/// Primitive values (numbers, booleans, null) are converted via `to_string()`.
pub(crate) fn serialize_tool_result(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            serde_json::to_string(value).unwrap_or_default()
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_value_is_not_double_encoded() {
        let value = json!("hello");
        assert_eq!(serialize_tool_result(&value), "hello");
    }

    #[test]
    fn object_value_is_serialized_as_json() {
        let value = json!({"key": "value", "num": 42});
        let result = serialize_tool_result(&value);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed, value);
    }

    #[test]
    fn array_value_is_serialized_as_json() {
        let value = json!([1, 2, 3]);
        let result = serialize_tool_result(&value);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed, value);
    }

    #[test]
    fn number_value_is_stringified() {
        assert_eq!(serialize_tool_result(&json!(42)), "42");
        assert_eq!(serialize_tool_result(&json!(3.14)), "3.14");
    }

    #[test]
    fn bool_value_is_stringified() {
        assert_eq!(serialize_tool_result(&json!(true)), "true");
        assert_eq!(serialize_tool_result(&json!(false)), "false");
    }

    #[test]
    fn null_value_is_stringified() {
        assert_eq!(serialize_tool_result(&json!(null)), "null");
    }
}
