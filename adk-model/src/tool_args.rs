//! Decoding of tool-call arguments shared by the provider clients.

use serde_json::Value;

/// Normalizes tool arguments returned by OpenAI-compatible wire protocols.
///
/// Compatible providers sometimes encode a no-argument call as a missing value,
/// `null`, an empty string, or an empty array. These representations are all
/// equivalent to an empty JSON object. Non-empty arguments must still resolve
/// to an object so malformed payloads cannot silently invoke a tool.
pub(crate) fn decode_tool_call_arguments(arguments: Option<&Value>) -> Result<Value, String> {
    let decoded = match arguments {
        None | Some(Value::Null) => serde_json::json!({}),
        Some(Value::String(encoded)) if encoded.trim().is_empty() => serde_json::json!({}),
        Some(Value::String(encoded)) => {
            let decoded: Value = serde_json::from_str(encoded)
                .map_err(|error| format!("arguments are not valid JSON: {error}"))?;
            match decoded {
                Value::Null => serde_json::json!({}),
                Value::Array(items) if items.is_empty() => serde_json::json!({}),
                decoded => decoded,
            }
        }
        Some(Value::Object(fields)) => Value::Object(fields.clone()),
        Some(Value::Array(items)) if items.is_empty() => serde_json::json!({}),
        Some(other) => {
            return Err(format!(
                "arguments must be a JSON object or an encoded JSON object, got {}",
                match other {
                    Value::Array(_) => "array",
                    Value::Bool(_) => "boolean",
                    Value::Number(_) => "number",
                    Value::String(_) => "string",
                    Value::Null => "null",
                    Value::Object(_) => "object",
                }
            ));
        }
    };
    if decoded.is_object() {
        Ok(decoded)
    } else {
        Err("arguments must decode to a JSON object".to_owned())
    }
}

/// Decodes a tool call's arguments, or marks the call as having undecodable arguments.
///
/// Arguments that do not decode to a JSON object become
/// [`adk_core::invalid_tool_arguments`], so the turn continues and the agent answers the call
/// with an error instead of executing it; the model can then send the call again. A truncated
/// or malformed call never runs with substituted arguments.
pub(crate) fn tool_call_arguments(
    provider: &str,
    tool_name: &str,
    arguments: Option<&Value>,
) -> Value {
    decode_tool_call_arguments(arguments).unwrap_or_else(|reason| {
        let raw = match arguments {
            Some(Value::String(raw)) => raw.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        };
        tracing::warn!(
            provider = %provider,
            tool.name = %tool_name,
            reason = %reason,
            "model sent tool-call arguments that are not a JSON object"
        );
        adk_core::invalid_tool_arguments(reason, &raw)
    })
}

/// Decodes the JSON arguments accumulated from a streamed tool call, as
/// [`tool_call_arguments`] does.
///
/// An empty payload is a zero-argument call and decodes to `{}`.
pub(crate) fn streamed_tool_arguments(provider: &str, tool_name: &str, arguments: &str) -> Value {
    tool_call_arguments(provider, tool_name, Some(&Value::String(arguments.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_streamed_arguments_are_an_empty_object() {
        for empty in ["", "  ", "null", "[]"] {
            assert_eq!(
                streamed_tool_arguments("test", "no_args", empty),
                serde_json::json!({}),
                "{empty:?}"
            );
        }
    }

    #[test]
    fn malformed_streamed_arguments_are_marked_not_substituted() {
        for (malformed, reason) in [
            (r#"{"city":"#, "arguments are not valid JSON"),
            (r#"["pwd"]"#, "arguments must decode to a JSON object"),
            ("42", "arguments must decode to a JSON object"),
        ] {
            let args = streamed_tool_arguments("test", "weather", malformed);
            let found = adk_core::invalid_tool_arguments_reason(&args).unwrap();
            assert!(found.starts_with(reason), "{malformed:?}: {found}");
            assert_eq!(
                args[adk_core::INVALID_TOOL_ARGUMENTS_KEY]["raw"],
                malformed,
                "{malformed:?}"
            );
        }
    }

    #[test]
    fn valid_streamed_arguments_parse() {
        assert_eq!(
            streamed_tool_arguments("test", "weather", r#"{"city":"Nairobi"}"#),
            serde_json::json!({"city": "Nairobi"})
        );
    }

    #[test]
    fn structured_arguments_that_are_not_objects_are_marked() {
        let args = tool_call_arguments("test", "weather", Some(&serde_json::json!(7)));
        assert!(adk_core::invalid_tool_arguments_reason(&args).is_some());
        assert_eq!(args[adk_core::INVALID_TOOL_ARGUMENTS_KEY]["raw"], "7");
    }
}
