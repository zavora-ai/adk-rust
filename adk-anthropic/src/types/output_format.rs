use serde::{Deserialize, Serialize};

/// Output format configuration for structured outputs.
///
/// Structured outputs constrain Claude's responses to follow a specific schema,
/// ensuring valid, parseable output for downstream processing.
///
/// This feature requires the beta header `structured-outputs-2025-11-13`.
///
/// # Example
///
/// ```
/// use serde_json::json;
/// use adk_anthropic::OutputFormat;
///
/// let output_format = OutputFormat::json_schema(json!({
///     "type": "object",
///     "properties": {
///         "name": { "type": "string" },
///         "email": { "type": "string" },
///         "plan_interest": { "type": "string" },
///         "demo_requested": { "type": "boolean" }
///     },
///     "required": ["name", "email", "plan_interest", "demo_requested"],
///     "additionalProperties": false
/// }));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputFormat {
    /// JSON object format — `{type: "json_object"}`.
    #[serde(rename = "json_object")]
    Json,

    /// JSON schema format for structured outputs.
    ///
    /// Claude will return valid JSON matching the provided schema in
    /// `response.content[0].text`.
    JsonSchema {
        /// The JSON schema that defines the structure of the response.
        schema: serde_json::Value,
    },
}

impl OutputFormat {
    /// Create a new JSON schema output format.
    ///
    /// # Arguments
    ///
    /// * `schema` - A JSON schema that defines the structure of the response.
    ///
    /// # Example
    ///
    /// ```
    /// use serde_json::json;
    /// use adk_anthropic::OutputFormat;
    ///
    /// let output_format = OutputFormat::json_schema(json!({
    ///     "type": "object",
    ///     "properties": {
    ///         "summary": { "type": "string" },
    ///         "confidence": { "type": "number" }
    ///     },
    ///     "required": ["summary", "confidence"],
    ///     "additionalProperties": false
    /// }));
    /// ```
    pub fn json_schema(schema: serde_json::Value) -> Self {
        Self::JsonSchema { schema }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn json_schema_output_format_serialization() {
        let output_format = OutputFormat::json_schema(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "email": { "type": "string" }
            },
            "required": ["name", "email"],
            "additionalProperties": false
        }));

        let json = to_value(&output_format).unwrap();
        assert_eq!(
            json,
            json!({
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "email": { "type": "string" }
                    },
                    "required": ["name", "email"],
                    "additionalProperties": false
                }
            })
        );
    }

    #[test]
    fn json_schema_output_format_deserialization() {
        let json = json!({
            "type": "json_schema",
            "schema": {
                "type": "object",
                "properties": {
                    "answer": { "type": "boolean" }
                },
                "required": ["answer"],
                "additionalProperties": false
            }
        });

        let output_format: OutputFormat = serde_json::from_value(json).unwrap();
        match output_format {
            OutputFormat::JsonSchema { schema } => {
                assert_eq!(schema["type"], "object");
                assert!(schema["properties"]["answer"].is_object());
            }
            _ => panic!("Expected JsonSchema variant"),
        }
    }

    #[test]
    fn json_schema_constructor() {
        let schema = json!({
            "type": "object",
            "properties": {
                "result": { "type": "string" }
            },
            "required": ["result"],
            "additionalProperties": false
        });

        let output_format = OutputFormat::json_schema(schema.clone());
        match output_format {
            OutputFormat::JsonSchema { schema: inner_schema } => {
                assert_eq!(inner_schema, schema);
            }
            _ => panic!("Expected JsonSchema variant"),
        }
    }

    #[test]
    fn json_object_serialization() {
        let output_format = OutputFormat::Json;
        let json = to_value(&output_format).unwrap();
        assert_eq!(json, json!({"type": "json_object"}));
    }

    #[test]
    fn json_object_deserialization() {
        let json = json!({"type": "json_object"});
        let output_format: OutputFormat = serde_json::from_value(json).unwrap();
        assert_eq!(output_format, OutputFormat::Json);
    }
}
