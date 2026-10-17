//! XML-based tool call markup parsing.
//!
//! Some models (especially smaller ones or those without native function calling)
//! output tool calls using XML-like markup:
//!
//! ```text
//! <tool_call>
//! function_name
//! <arg_key>param1</arg_key>
//! <arg_value>value1</arg_value>
//! <arg_key>param2</arg_key>
//! <arg_value>value2</arg_value>
//! </tool_call>
//! ```
//!
//! This module provides utilities to parse such markup into proper `Part::FunctionCall`.

use adk_core::{Content, Part};

/// Normalize model content: convert tool call markup in text parts to `FunctionCall` parts,
/// and give each function call without an ID a generated one.
///
/// The generated ID ([`adk_core::GENERATED_CALL_ID_PREFIX`]) identifies the call, its
/// response and any confirmation request for it across runs, so two calls to one tool from
/// a provider that sends no IDs (Ollama, tool call markup) stay apart. `LlmAgent` removes
/// these IDs from the contents it sends to a model.
pub fn normalize_content(content: &mut Content) {
    let parts = std::mem::take(&mut content.parts);
    let mut normalized = Vec::new();

    for part in parts {
        match part {
            Part::Text { text } => {
                normalized.extend(convert_text_to_parts(text));
            }
            other => normalized.push(other),
        }
    }

    content.parts = normalized;
    content.assign_call_ids();
}

/// Normalize `Option<Content>`; see [`normalize_content`].
pub fn normalize_option_content(content: &mut Option<Content>) {
    if let Some(content) = content {
        normalize_content(content);
    }
}

/// Convert text containing tool call markup to a list of parts.
fn convert_text_to_parts(text: String) -> Vec<Part> {
    const TOOL_CALL_START: &str = "<tool_call>";
    const TOOL_CALL_END: &str = "</tool_call>";

    if !text.contains(TOOL_CALL_START) {
        return vec![Part::Text { text }];
    }

    let mut parts = Vec::new();
    let mut remainder = text.as_str();

    while let Some(start_idx) = remainder.find(TOOL_CALL_START) {
        let (before, after_start_tag) = remainder.split_at(start_idx);
        if !before.is_empty() {
            parts.push(Part::Text { text: before.to_string() });
        }

        let after_start = &after_start_tag[TOOL_CALL_START.len()..];
        if let Some(end_idx) = after_start.find(TOOL_CALL_END) {
            let block = &after_start[..end_idx];
            if let Some(call_part) = parse_tool_call_block(block) {
                parts.push(call_part);
            } else {
                // Failed to parse - keep as text
                parts.push(Part::Text {
                    text: format!("{}{}{}", TOOL_CALL_START, block, TOOL_CALL_END),
                });
            }
            remainder = &after_start[end_idx + TOOL_CALL_END.len()..];
        } else {
            // Unclosed tag - keep remainder as text
            parts.push(Part::Text { text: format!("{}{}", TOOL_CALL_START, after_start) });
            remainder = "";
            break;
        }
    }

    if !remainder.is_empty() {
        parts.push(Part::Text { text: remainder.to_string() });
    }

    if parts.is_empty() { vec![Part::Text { text }] } else { parts }
}

/// Parse a tool call block into a FunctionCall part.
fn parse_tool_call_block(block: &str) -> Option<Part> {
    let trimmed = block.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut lines = trimmed.lines();
    let name_line = lines.next()?.trim();
    if name_line.is_empty() {
        return None;
    }

    let remainder = lines.collect::<Vec<_>>().join("\n");
    let mut slice = remainder.as_str();
    let mut args_map = serde_json::Map::new();
    let mut found_arg = false;

    loop {
        slice = slice.trim_start();
        if slice.is_empty() {
            break;
        }

        let rest = if let Some(rest) = slice.strip_prefix("<arg_key>") {
            rest
        } else {
            break;
        };

        let key_end = rest.find("</arg_key>")?;
        let key = rest[..key_end].trim().to_string();
        let mut after_key = &rest[key_end + "</arg_key>".len()..];

        after_key = after_key.trim_start();
        let rest = if let Some(rest) = after_key.strip_prefix("<arg_value>") {
            rest
        } else {
            break;
        };

        let value_end = rest.find("</arg_value>")?;
        let value_text = rest[..value_end].trim();
        let value = parse_arg_value(value_text);
        args_map.insert(key, value);
        slice = &rest[value_end + "</arg_value>".len()..];
        found_arg = true;
    }

    if !found_arg {
        return None;
    }

    Some(Part::FunctionCall {
        name: name_line.to_string(),
        args: serde_json::Value::Object(args_map),
        id: None,
        thought_signature: None,
    })
}

/// Parse an argument value, attempting JSON parsing first.
fn parse_arg_value(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return serde_json::Value::String(String::new());
    }

    serde_json::from_str(trimmed).unwrap_or_else(|_| serde_json::Value::String(trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_markup() {
        let parts = convert_text_to_parts("Hello world".to_string());
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], Part::Text { text } if text == "Hello world"));
    }

    #[test]
    fn test_simple_tool_call() {
        let text = r#"<tool_call>
get_weather
<arg_key>city</arg_key>
<arg_value>Tokyo</arg_value>
</tool_call>"#
            .to_string();

        let parts = convert_text_to_parts(text);
        assert_eq!(parts.len(), 1);

        if let Part::FunctionCall { name, args, .. } = &parts[0] {
            assert_eq!(name, "get_weather");
            assert_eq!(args["city"], "Tokyo");
        } else {
            panic!("Expected FunctionCall");
        }
    }

    #[test]
    fn test_tool_call_with_surrounding_text() {
        let text = r#"Let me check the weather. <tool_call>
get_weather
<arg_key>city</arg_key>
<arg_value>Paris</arg_value>
</tool_call> Done!"#
            .to_string();

        let parts = convert_text_to_parts(text);
        assert_eq!(parts.len(), 3);
        assert!(matches!(&parts[0], Part::Text { text } if text.contains("Let me check")));
        assert!(matches!(&parts[1], Part::FunctionCall { name, .. } if name == "get_weather"));
        assert!(matches!(&parts[2], Part::Text { text } if text.contains("Done")));
    }

    #[test]
    fn test_multiple_args() {
        let text = r#"<tool_call>
calculator
<arg_key>operation</arg_key>
<arg_value>add</arg_value>
<arg_key>a</arg_key>
<arg_value>5</arg_value>
<arg_key>b</arg_key>
<arg_value>3</arg_value>
</tool_call>"#
            .to_string();

        let parts = convert_text_to_parts(text);
        assert_eq!(parts.len(), 1);

        if let Part::FunctionCall { name, args, .. } = &parts[0] {
            assert_eq!(name, "calculator");
            assert_eq!(args["operation"], "add");
            // Note: numeric values come as strings unless valid JSON
            assert_eq!(args["a"], 5);
            assert_eq!(args["b"], 3);
        } else {
            panic!("Expected FunctionCall");
        }
    }

    #[test]
    fn test_json_arg_value() {
        let text = r#"<tool_call>
process
<arg_key>config</arg_key>
<arg_value>{"enabled": true, "count": 42}</arg_value>
</tool_call>"#
            .to_string();

        let parts = convert_text_to_parts(text);
        assert_eq!(parts.len(), 1);

        if let Part::FunctionCall { args, .. } = &parts[0] {
            assert!(args["config"]["enabled"].as_bool().unwrap());
            assert_eq!(args["config"]["count"], 42);
        } else {
            panic!("Expected FunctionCall");
        }
    }

    #[test]
    fn test_normalize_content() {
        let mut content = Content {
            role: "model".to_string(),
            parts: vec![Part::Text {
                text: r#"<tool_call>
test_tool
<arg_key>param</arg_key>
<arg_value>value</arg_value>
</tool_call>"#
                    .to_string(),
            }],
        };

        normalize_content(&mut content);
        assert_eq!(content.parts.len(), 1);
        assert!(
            matches!(&content.parts[0], Part::FunctionCall { name, .. } if name == "test_tool")
        );
    }

    #[test]
    fn calls_without_ids_get_distinct_generated_ids_and_provider_ids_stay() {
        let call = |id: Option<&str>| Part::FunctionCall {
            name: "transfer".to_string(),
            args: serde_json::json!({}),
            id: id.map(str::to_string),
            thought_signature: None,
        };
        let mut content = Content {
            role: "model".to_string(),
            parts: vec![call(None), call(None), call(Some("c1"))],
        };

        normalize_content(&mut content);
        let ids: Vec<String> = content
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::FunctionCall { id, .. } => id.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 3);
        assert!(ids[0].starts_with(adk_core::GENERATED_CALL_ID_PREFIX));
        assert!(ids[1].starts_with(adk_core::GENERATED_CALL_ID_PREFIX));
        assert_ne!(ids[0], ids[1]);
        assert_eq!(ids[2], "c1");

        // Normalizing again keeps the IDs already given.
        let before = content.parts.clone();
        normalize_content(&mut content);
        assert_eq!(content.parts, before);
    }
}
