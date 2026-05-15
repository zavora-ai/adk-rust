//! Schema adapter for Anthropic's Claude models.
//!
//! Anthropic has the most permissive JSON Schema support among major LLM providers.
//! The [`AnthropicSchemaAdapter`] applies only minimal transforms:
//!
//! 1. Strip `$schema` keyword (meta-keyword no provider supports)
//! 2. Strip conditional keywords (`if`/`then`/`else`)
//! 3. Add implicit `"type": "object"` when `properties` exists without a `type` field
//!
//! Everything else passes through unchanged: `$ref`, `$defs`, `anyOf`, `oneOf`,
//! `allOf`, `additionalProperties`, type arrays, `const`, and all `format` values.
//!
//! # Example
//!
//! ```rust
//! use adk_model::anthropic::AnthropicSchemaAdapter;
//! use adk_core::SchemaAdapter;
//! use serde_json::json;
//!
//! let adapter = AnthropicSchemaAdapter;
//! let schema = json!({
//!     "$schema": "http://json-schema.org/draft-07/schema#",
//!     "type": "object",
//!     "properties": {
//!         "name": { "type": "string", "const": "fixed" }
//!     },
//!     "$ref": "#/$defs/Foo",
//!     "$defs": { "Foo": { "type": "string" } },
//!     "anyOf": [{ "type": "string" }, { "type": "number" }],
//!     "additionalProperties": false
//! });
//!
//! let normalized = adapter.normalize_schema(schema);
//! // $schema removed
//! assert!(normalized.get("$schema").is_none());
//! // const preserved (Anthropic supports it natively)
//! assert_eq!(normalized["properties"]["name"]["const"], "fixed");
//! // $ref, $defs, anyOf, additionalProperties all preserved
//! assert!(normalized.get("$ref").is_some());
//! assert!(normalized.get("$defs").is_some());
//! assert!(normalized.get("anyOf").is_some());
//! assert!(normalized.get("additionalProperties").is_some());
//! ```

use std::borrow::Cow;

use adk_core::{SchemaAdapter, schema_utils};
use serde_json::Value;

/// Schema adapter for Anthropic's Claude models (near pass-through).
///
/// Anthropic's function-calling API accepts most JSON Schema features natively.
/// This adapter only strips the meta-keywords that no provider supports and adds
/// implicit object types where needed.
///
/// ## Preserved Features
///
/// - `$ref` and `$defs` (reference resolution not needed)
/// - `anyOf`, `oneOf`, `allOf` (combiners supported)
/// - `additionalProperties` (supported as-is)
/// - Type arrays (e.g., `["string", "null"]`)
/// - `const` keyword (native support)
/// - All `format` values (no stripping)
///
/// ## Transforms Applied
///
/// 1. `strip_schema_keyword` — removes `$schema` meta-keyword
/// 2. `strip_conditional_keywords` — removes `if`/`then`/`else`
/// 3. `add_implicit_object_type` — adds `"type": "object"` when `properties` exists
///
/// ## Tool Name Normalization
///
/// Truncates tool names exceeding 64 characters at the nearest valid UTF-8
/// character boundary, preserving the prefix.
#[derive(Debug)]
pub struct AnthropicSchemaAdapter;

impl SchemaAdapter for AnthropicSchemaAdapter {
    fn normalize_schema(&self, mut schema: Value) -> Value {
        schema_utils::strip_schema_keyword(&mut schema);
        schema_utils::strip_conditional_keywords(&mut schema);
        schema_utils::add_implicit_object_type(&mut schema);
        schema
    }

    fn normalize_tool_name<'a>(&self, name: &'a str) -> Cow<'a, str> {
        if name.len() <= 64 {
            Cow::Borrowed(name)
        } else {
            // Find the largest valid UTF-8 boundary at or before 64 bytes.
            let mut end = 64;
            while end > 0 && !name.is_char_boundary(end) {
                end -= 1;
            }
            Cow::Owned(name[..end].to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_strips_schema_keyword() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("$schema").is_none());
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_strips_conditional_keywords() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "type": "object",
            "if": { "properties": { "kind": { "const": "a" } } },
            "then": { "required": ["extra"] },
            "else": { "required": [] }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("if").is_none());
        assert!(result.get("then").is_none());
        assert!(result.get("else").is_none());
    }

    #[test]
    fn test_adds_implicit_object_type() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "properties": {
                "name": { "type": "string" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_preserves_ref() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "type": "object",
            "$ref": "#/$defs/Address"
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["$ref"], "#/$defs/Address");
    }

    #[test]
    fn test_preserves_defs() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "type": "object",
            "$defs": {
                "Address": {
                    "type": "object",
                    "properties": {
                        "street": { "type": "string" }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("$defs").is_some());
        assert_eq!(result["$defs"]["Address"]["type"], "object");
    }

    #[test]
    fn test_preserves_any_of() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "anyOf": [
                { "type": "string" },
                { "type": "number" }
            ]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("anyOf").is_some());
        assert_eq!(result["anyOf"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_preserves_one_of() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "oneOf": [
                { "type": "boolean" },
                { "type": "null" }
            ]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("oneOf").is_some());
        assert_eq!(result["oneOf"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_preserves_all_of() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "allOf": [
                { "type": "object", "properties": { "a": { "type": "string" } } },
                { "required": ["a"] }
            ]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("allOf").is_some());
        assert_eq!(result["allOf"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_preserves_additional_properties() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "additionalProperties": false
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["additionalProperties"], false);
    }

    #[test]
    fn test_preserves_type_arrays() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "type": ["string", "null"]
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["type"], json!(["string", "null"]));
    }

    #[test]
    fn test_preserves_const() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "type": "string",
            "const": "fixed_value"
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["const"], "fixed_value");
    }

    #[test]
    fn test_preserves_all_format_values() {
        let adapter = AnthropicSchemaAdapter;
        let formats = [
            "date-time",
            "date",
            "time",
            "email",
            "uri",
            "uuid",
            "hostname",
            "ipv4",
            "ipv6",
            "uri-reference",
            "json-pointer",
            "regex",
            "iri",
            "iri-reference",
            "uri-template",
            "int32",
            "int64",
            "float",
            "double",
        ];
        for format in formats {
            let schema = json!({ "type": "string", "format": format });
            let result = adapter.normalize_schema(schema);
            assert_eq!(
                result["format"], format,
                "format '{format}' should be preserved by Anthropic adapter"
            );
        }
    }

    #[test]
    fn test_normalize_tool_name_short() {
        let adapter = AnthropicSchemaAdapter;
        let name = "get_weather";
        assert_eq!(adapter.normalize_tool_name(name), Cow::Borrowed("get_weather"));
    }

    #[test]
    fn test_normalize_tool_name_exactly_64() {
        let adapter = AnthropicSchemaAdapter;
        let name = "a".repeat(64);
        assert_eq!(adapter.normalize_tool_name(&name), Cow::Borrowed(name.as_str()));
    }

    #[test]
    fn test_normalize_tool_name_truncates_at_64() {
        let adapter = AnthropicSchemaAdapter;
        let name = "a".repeat(100);
        let result = adapter.normalize_tool_name(&name);
        assert_eq!(result.len(), 64);
        assert_eq!(result.as_ref(), "a".repeat(64).as_str());
    }

    #[test]
    fn test_normalize_tool_name_multibyte_boundary() {
        let adapter = AnthropicSchemaAdapter;
        // Each '🦀' is 4 bytes. 16 crabs = 64 bytes exactly.
        let name = "🦀".repeat(16);
        assert_eq!(name.len(), 64);
        assert_eq!(adapter.normalize_tool_name(&name), Cow::Borrowed(name.as_str()));

        // 17 crabs = 68 bytes, should truncate to 16 crabs (64 bytes)
        let long_name = "🦀".repeat(17);
        let result = adapter.normalize_tool_name(&long_name);
        assert_eq!(result.len(), 64);
        assert_eq!(result.as_ref(), "🦀".repeat(16).as_str());
    }

    #[test]
    fn test_normalize_tool_name_truncates_at_char_boundary() {
        let adapter = AnthropicSchemaAdapter;
        // 'é' is 2 bytes. 32 of them = 64 bytes exactly.
        let name = "é".repeat(32);
        assert_eq!(name.len(), 64);
        assert_eq!(adapter.normalize_tool_name(&name), Cow::Borrowed(name.as_str()));

        // 33 'é' = 66 bytes, should truncate to 32 'é' (64 bytes)
        let long_name = "é".repeat(33);
        let result = adapter.normalize_tool_name(&long_name);
        assert_eq!(result.len(), 64);
        assert_eq!(result.as_ref(), "é".repeat(32).as_str());
    }

    #[test]
    fn test_empty_schema() {
        let adapter = AnthropicSchemaAdapter;
        assert_eq!(adapter.empty_schema(), json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn test_idempotent() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "properties": {
                "name": { "type": "string" }
            },
            "if": { "const": true },
            "then": { "required": ["name"] },
            "anyOf": [{ "type": "string" }],
            "const": "test",
            "additionalProperties": false
        });
        let first = adapter.normalize_schema(schema);
        let second = adapter.normalize_schema(first.clone());
        assert_eq!(first, second);
    }

    #[test]
    fn test_nested_conditional_keywords_stripped() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "nested": {
                    "type": "object",
                    "properties": {
                        "deep": { "type": "string" }
                    },
                    "if": { "properties": { "deep": { "const": "x" } } },
                    "then": { "required": ["deep"] }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        let nested = &result["properties"]["nested"];
        assert!(nested.get("if").is_none());
        assert!(nested.get("then").is_none());
    }

    #[test]
    fn test_combined_transforms() {
        let adapter = AnthropicSchemaAdapter;
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "properties": {
                "status": { "type": "string", "const": "active" },
                "host": { "type": "string", "format": "hostname" }
            },
            "$ref": "#/$defs/Base",
            "$defs": { "Base": { "type": "object" } },
            "anyOf": [{ "type": "string" }],
            "oneOf": [{ "type": "number" }],
            "allOf": [{ "required": ["status"] }],
            "additionalProperties": { "type": "string" },
            "if": { "properties": { "status": { "const": "active" } } },
            "then": { "required": ["host"] }
        });
        let result = adapter.normalize_schema(schema);

        // $schema removed
        assert!(result.get("$schema").is_none());
        // conditional keywords removed
        assert!(result.get("if").is_none());
        assert!(result.get("then").is_none());
        // implicit type added
        assert_eq!(result["type"], "object");
        // const preserved
        assert_eq!(result["properties"]["status"]["const"], "active");
        // format preserved (even non-standard ones)
        assert_eq!(result["properties"]["host"]["format"], "hostname");
        // $ref, $defs, combiners, additionalProperties all preserved
        assert_eq!(result["$ref"], "#/$defs/Base");
        assert!(result.get("$defs").is_some());
        assert!(result.get("anyOf").is_some());
        assert!(result.get("oneOf").is_some());
        assert!(result.get("allOf").is_some());
        assert!(result.get("additionalProperties").is_some());
    }

    /// Comprehensive pass-through verification test.
    ///
    /// Verifies that `AnthropicSchemaAdapter` preserves all schema features
    /// and only strips `$schema` and conditional keywords (`if`/`then`/`else`).
    ///
    /// **Validates: Requirements 6.4**
    #[test]
    fn test_passthrough_preserves_all_schema_features() {
        let adapter = AnthropicSchemaAdapter;

        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "$defs": { "Foo": { "type": "string" } },
            "properties": {
                "ref_field": { "$ref": "#/$defs/Foo" },
                "nullable": { "type": ["string", "null"] },
                "status": { "const": "active" },
                "format_field": { "type": "string", "format": "hostname" }
            },
            "anyOf": [{"type": "object"}, {"type": "null"}],
            "additionalProperties": true,
            "if": { "properties": { "x": { "type": "number" } } },
            "then": { "required": ["x"] }
        });

        let result = adapter.normalize_schema(schema);

        // 1. Preserves $ref and $defs
        assert_eq!(
            result["properties"]["ref_field"]["$ref"], "#/$defs/Foo",
            "$ref must be preserved"
        );
        assert_eq!(result["$defs"]["Foo"]["type"], "string", "$defs must be preserved");

        // 2. Preserves additionalProperties
        assert_eq!(result["additionalProperties"], true, "additionalProperties must be preserved");

        // 3. Preserves type arrays
        assert_eq!(
            result["properties"]["nullable"]["type"],
            json!(["string", "null"]),
            "type arrays must be preserved"
        );

        // 4. Preserves const keywords
        assert_eq!(
            result["properties"]["status"]["const"], "active",
            "const keyword must be preserved"
        );

        // 5. Preserves all format values (including non-standard ones)
        assert_eq!(
            result["properties"]["format_field"]["format"], "hostname",
            "all format values must be preserved"
        );

        // 6. Preserves anyOf combiner
        let any_of = result["anyOf"].as_array().expect("anyOf must be preserved");
        assert_eq!(any_of.len(), 2);
        assert_eq!(any_of[0]["type"], "object");
        assert_eq!(any_of[1]["type"], "null");

        // 7. Only strips $schema and conditional keywords
        assert!(result.get("$schema").is_none(), "$schema must be stripped");
        assert!(result.get("if").is_none(), "if must be stripped");
        assert!(result.get("then").is_none(), "then must be stripped");
        assert!(
            result.get("else").is_none(),
            "else must be stripped (not present in input but verify absence)"
        );

        // Verify type is preserved
        assert_eq!(result["type"], "object");
    }

    /// Verifies that `else` conditional keyword is also stripped when present.
    ///
    /// **Validates: Requirements 6.3**
    #[test]
    fn test_passthrough_strips_else_keyword() {
        let adapter = AnthropicSchemaAdapter;

        let schema = json!({
            "type": "object",
            "properties": {
                "x": { "type": "number" }
            },
            "if": { "properties": { "x": { "minimum": 0 } } },
            "then": { "required": ["x"] },
            "else": { "properties": { "fallback": { "type": "string" } } }
        });

        let result = adapter.normalize_schema(schema);

        assert!(result.get("if").is_none(), "if must be stripped");
        assert!(result.get("then").is_none(), "then must be stripped");
        assert!(result.get("else").is_none(), "else must be stripped");
        // Properties and type preserved
        assert_eq!(result["type"], "object");
        assert!(result.get("properties").is_some());
    }

    /// Verifies that oneOf and allOf combiners are also preserved.
    ///
    /// **Validates: Requirements 6.4**
    #[test]
    fn test_passthrough_preserves_one_of_and_all_of() {
        let adapter = AnthropicSchemaAdapter;

        let schema = json!({
            "type": "object",
            "oneOf": [
                { "properties": { "a": { "type": "string" } } },
                { "properties": { "b": { "type": "number" } } }
            ],
            "allOf": [
                { "required": ["id"] },
                { "properties": { "id": { "type": "string" } } }
            ]
        });

        let result = adapter.normalize_schema(schema);

        let one_of = result["oneOf"].as_array().expect("oneOf must be preserved");
        assert_eq!(one_of.len(), 2);

        let all_of = result["allOf"].as_array().expect("allOf must be preserved");
        assert_eq!(all_of.len(), 2);
    }
}
