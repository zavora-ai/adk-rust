//! Decoding of tool-call arguments shared by the provider clients.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
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

/// Parses the JSON arguments accumulated from a streamed tool call.
///
/// An empty payload is a zero-argument call and decodes to `{}`.
///
/// # Errors
///
/// Returns an [`ErrorCategory::Internal`] error with the given `code` when the
/// payload is not a JSON object, so a truncated or malformed call never runs
/// the tool with substituted arguments.
pub(crate) fn parse_streamed_tool_arguments(
    provider: &str,
    code: &'static str,
    tool_name: &str,
    arguments: &str,
) -> Result<Value, AdkError> {
    let encoded = Value::String(arguments.to_owned());
    decode_tool_call_arguments(Some(&encoded)).map_err(|error| {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Internal,
            code,
            format!("{provider} returned invalid JSON arguments for tool '{tool_name}': {error}"),
        )
        .with_provider(provider)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "model.test.invalid_tool_arguments";

    #[test]
    fn empty_streamed_arguments_are_an_empty_object() {
        for empty in ["", "  ", "null", "[]"] {
            assert_eq!(
                parse_streamed_tool_arguments("test", CODE, "no_args", empty).unwrap(),
                serde_json::json!({}),
                "{empty:?}"
            );
        }
    }

    #[test]
    fn malformed_streamed_arguments_are_an_error() {
        for malformed in [r#"{"city":"#, r#"["pwd"]"#, "42"] {
            let error =
                parse_streamed_tool_arguments("test", CODE, "weather", malformed).unwrap_err();
            assert_eq!(error.code, CODE, "{malformed:?}");
            assert_eq!(error.category, ErrorCategory::Internal);
            assert_eq!(error.details.provider.as_deref(), Some("test"));
            assert!(error.message.contains("'weather'"));
        }
    }

    #[test]
    fn valid_streamed_arguments_parse() {
        assert_eq!(
            parse_streamed_tool_arguments("test", CODE, "weather", r#"{"city":"Nairobi"}"#)
                .unwrap(),
            serde_json::json!({"city": "Nairobi"})
        );
    }
}
