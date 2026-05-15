//! OpenAI schema adapters for function-calling APIs.
//!
//! Provides two adapters:
//!
//! - [`OpenAiStrictSchemaAdapter`]: For OpenAI strict mode — preserves `$ref`/`$defs`,
//!   `anyOf`/`oneOf`, and type arrays while recursively injecting
//!   `additionalProperties: false` on all object schemas.
//!
//! - [`OpenAiSchemaAdapter`]: For OpenAI non-strict mode — applies minimal safe fixes
//!   without destroying schema semantics. Does NOT remove `$ref`, `$defs`, `anyOf`,
//!   `oneOf`, `additionalProperties`, or collapse type arrays.
//!
//! Both adapters share the same allowed format list as Gemini:
//! `date-time`, `date`, `time`, `email`, `uri`, `uuid`, `int32`, `int64`, `float`, `double`.

use adk_core::{SchemaAdapter, schema_utils};
use serde_json::Value;

/// Allowed format values for OpenAI adapters (same as Gemini/Generic).
const OPENAI_ALLOWED_FORMATS: &[&str] =
    &["date-time", "date", "time", "email", "uri", "uuid", "int32", "int64", "float", "double"];

/// Schema adapter for OpenAI strict mode.
///
/// Applies conservative transforms and recursively sets `additionalProperties: false`
/// on all object schemas. Preserves `$ref`, `$defs`, `anyOf`, `oneOf`, and type arrays.
///
/// Transform order:
/// 1. `strip_schema_keyword`
/// 2. `strip_conditional_keywords`
/// 3. `convert_const_to_enum`
/// 4. `add_implicit_object_type`
/// 5. `strip_unsupported_formats`
/// 6. Recursively set `additionalProperties: false` on all object schemas
///
/// # Example
///
/// ```rust
/// use adk_model::openai::OpenAiStrictSchemaAdapter;
/// use adk_core::SchemaAdapter;
/// use serde_json::json;
///
/// let adapter = OpenAiStrictSchemaAdapter;
/// let schema = json!({
///     "type": "object",
///     "properties": {
///         "name": { "type": "string" },
///         "address": {
///             "type": "object",
///             "properties": {
///                 "street": { "type": "string" }
///             }
///         }
///     }
/// });
///
/// let result = adapter.normalize_schema(schema);
/// assert_eq!(result["additionalProperties"], false);
/// assert_eq!(result["properties"]["address"]["additionalProperties"], false);
/// ```
#[derive(Debug)]
pub struct OpenAiStrictSchemaAdapter;

impl SchemaAdapter for OpenAiStrictSchemaAdapter {
    fn normalize_schema(&self, mut schema: Value) -> Value {
        schema_utils::strip_schema_keyword(&mut schema);
        schema_utils::strip_conditional_keywords(&mut schema);
        schema_utils::convert_const_to_enum(&mut schema);
        schema_utils::add_implicit_object_type(&mut schema);
        schema_utils::strip_unsupported_formats(&mut schema, OPENAI_ALLOWED_FORMATS);
        set_additional_properties_false(&mut schema);
        schema
    }

    /// Returns `{"type": "object", "properties": {}, "additionalProperties": false}`.
    ///
    /// OpenAI strict mode requires `additionalProperties: false` even on the
    /// empty fallback schema.
    fn empty_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }
}

/// Schema adapter for OpenAI non-strict mode.
///
/// Applies minimal safe fixes without destroying schema semantics. Does NOT
/// remove `$ref`, `$defs`, `anyOf`, `oneOf`, `additionalProperties`, or
/// collapse type arrays.
///
/// Transform order:
/// 1. `strip_schema_keyword`
/// 2. `strip_conditional_keywords`
/// 3. `convert_const_to_enum`
/// 4. `add_implicit_object_type`
/// 5. `strip_unsupported_formats`
///
/// # Example
///
/// ```rust
/// use adk_model::openai::OpenAiSchemaAdapter;
/// use adk_core::SchemaAdapter;
/// use serde_json::json;
///
/// let adapter = OpenAiSchemaAdapter;
/// let schema = json!({
///     "$schema": "http://json-schema.org/draft-07/schema#",
///     "type": "object",
///     "properties": {
///         "name": { "type": "string" }
///     },
///     "$ref": "#/$defs/Base",
///     "anyOf": [{"type": "string"}, {"type": "number"}],
///     "additionalProperties": true
/// });
///
/// let result = adapter.normalize_schema(schema);
/// // $schema removed
/// assert!(result.get("$schema").is_none());
/// // $ref, anyOf, additionalProperties preserved
/// assert!(result.get("$ref").is_some());
/// assert!(result.get("anyOf").is_some());
/// assert!(result.get("additionalProperties").is_some());
/// ```
#[derive(Debug)]
pub struct OpenAiSchemaAdapter;

impl SchemaAdapter for OpenAiSchemaAdapter {
    fn normalize_schema(&self, mut schema: Value) -> Value {
        schema_utils::strip_schema_keyword(&mut schema);
        schema_utils::strip_conditional_keywords(&mut schema);
        schema_utils::convert_const_to_enum(&mut schema);
        schema_utils::add_implicit_object_type(&mut schema);
        schema_utils::strip_unsupported_formats(&mut schema, OPENAI_ALLOWED_FORMATS);
        schema
    }
}

/// Recursively sets `additionalProperties: false` on all object schemas.
///
/// An "object schema" is identified by having `"type": "object"` or having
/// a `"properties"` key. This function traverses into all nested sub-schema
/// locations: `properties`, `items`, `allOf`, `anyOf`, `oneOf`, `not`,
/// `patternProperties`, `prefixItems`, `$defs`, and `additionalProperties`
/// (when it is itself a schema object).
fn set_additional_properties_false(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    let is_object_schema = obj.get("type").and_then(|t| t.as_str()).is_some_and(|t| t == "object")
        || obj.contains_key("properties");

    if is_object_schema {
        obj.insert("additionalProperties".to_string(), Value::Bool(false));
    }

    // Recurse into properties
    if let Some(props) = obj.get_mut("properties") {
        if let Some(props_obj) = props.as_object_mut() {
            for value in props_obj.values_mut() {
                set_additional_properties_false(value);
            }
        }
    }

    // Recurse into items (single schema or array)
    if let Some(items) = obj.get_mut("items") {
        if items.is_object() {
            set_additional_properties_false(items);
        } else if let Some(arr) = items.as_array_mut() {
            for item in arr.iter_mut() {
                set_additional_properties_false(item);
            }
        }
    }

    // Recurse into additionalProperties when it's a schema object (not boolean)
    if let Some(additional) = obj.get_mut("additionalProperties") {
        if additional.is_object() {
            set_additional_properties_false(additional);
        }
    }

    // Recurse into allOf, anyOf, oneOf
    for keyword in &["allOf", "anyOf", "oneOf"] {
        if let Some(arr_val) = obj.get_mut(*keyword) {
            if let Some(arr) = arr_val.as_array_mut() {
                for sub in arr.iter_mut() {
                    set_additional_properties_false(sub);
                }
            }
        }
    }

    // Recurse into not
    if let Some(not_schema) = obj.get_mut("not") {
        if not_schema.is_object() {
            set_additional_properties_false(not_schema);
        }
    }

    // Recurse into patternProperties
    if let Some(pattern_props) = obj.get_mut("patternProperties") {
        if let Some(pp_obj) = pattern_props.as_object_mut() {
            for value in pp_obj.values_mut() {
                set_additional_properties_false(value);
            }
        }
    }

    // Recurse into prefixItems
    if let Some(prefix_items) = obj.get_mut("prefixItems") {
        if let Some(arr) = prefix_items.as_array_mut() {
            for item in arr.iter_mut() {
                set_additional_properties_false(item);
            }
        }
    }

    // Recurse into $defs
    if let Some(defs) = obj.get_mut("$defs") {
        if let Some(defs_obj) = defs.as_object_mut() {
            for value in defs_obj.values_mut() {
                set_additional_properties_false(value);
            }
        }
    }

    // Recurse into definitions (Draft 4-7)
    if let Some(defs) = obj.get_mut("definitions") {
        if let Some(defs_obj) = defs.as_object_mut() {
            for value in defs_obj.values_mut() {
                set_additional_properties_false(value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- OpenAiStrictSchemaAdapter tests ---

    #[test]
    fn test_strict_strips_schema_keyword() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("$schema").is_none());
    }

    #[test]
    fn test_strict_strips_conditional_keywords() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {},
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
    fn test_strict_converts_const_to_enum() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "string",
            "const": "fixed_value"
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("const").is_none());
        assert_eq!(result["enum"], json!(["fixed_value"]));
    }

    #[test]
    fn test_strict_adds_implicit_object_type() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "properties": {
                "name": { "type": "string" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_strict_strips_unsupported_formats() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "created": { "type": "string", "format": "date-time" },
                "hostname": { "type": "string", "format": "hostname" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["properties"]["created"]["format"], "date-time");
        assert!(result["properties"]["hostname"].get("format").is_none());
    }

    #[test]
    fn test_strict_sets_additional_properties_false_on_root() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["additionalProperties"], false);
    }

    #[test]
    fn test_strict_sets_additional_properties_false_recursively() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" },
                        "geo": {
                            "type": "object",
                            "properties": {
                                "lat": { "type": "number" }
                            }
                        }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["additionalProperties"], false);
        assert_eq!(result["properties"]["address"]["additionalProperties"], false);
        assert_eq!(
            result["properties"]["address"]["properties"]["geo"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn test_strict_sets_additional_properties_in_defs() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "item": { "$ref": "#/$defs/Item" }
            },
            "$defs": {
                "Item": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["$defs"]["Item"]["additionalProperties"], false);
    }

    #[test]
    fn test_strict_preserves_ref() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "item": { "$ref": "#/$defs/Item" }
            },
            "$defs": {
                "Item": {
                    "type": "object",
                    "properties": { "name": { "type": "string" } }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["properties"]["item"]["$ref"], "#/$defs/Item");
        assert!(result.get("$defs").is_some());
    }

    #[test]
    fn test_strict_preserves_any_of() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "value": {
                    "anyOf": [
                        { "type": "string" },
                        { "type": "null" }
                    ]
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result["properties"]["value"].get("anyOf").is_some());
    }

    #[test]
    fn test_strict_preserves_one_of() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "value": {
                    "oneOf": [
                        { "type": "string" },
                        { "type": "number" }
                    ]
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result["properties"]["value"].get("oneOf").is_some());
    }

    #[test]
    fn test_strict_preserves_type_arrays() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "nullable_name": {
                    "type": ["string", "null"]
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["properties"]["nullable_name"]["type"], json!(["string", "null"]));
    }

    #[test]
    fn test_strict_empty_schema() {
        let adapter = OpenAiStrictSchemaAdapter;
        let empty = adapter.empty_schema();
        assert_eq!(
            empty,
            json!({"type": "object", "properties": {}, "additionalProperties": false})
        );
    }

    #[test]
    fn test_strict_sets_additional_properties_in_any_of_sub_schemas() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "value": {
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": { "a": { "type": "string" } }
                        },
                        { "type": "null" }
                    ]
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        let any_of = result["properties"]["value"]["anyOf"].as_array().unwrap();
        assert_eq!(any_of[0]["additionalProperties"], false);
    }

    #[test]
    fn test_strict_idempotent() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "nested": {
                    "type": "object",
                    "properties": {
                        "value": { "type": "number" }
                    }
                }
            }
        });
        let first = adapter.normalize_schema(schema);
        let second = adapter.normalize_schema(first.clone());
        assert_eq!(first, second);
    }

    // --- OpenAiSchemaAdapter tests ---

    #[test]
    fn test_non_strict_strips_schema_keyword() {
        let adapter = OpenAiSchemaAdapter;
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("$schema").is_none());
    }

    #[test]
    fn test_non_strict_strips_conditional_keywords() {
        let adapter = OpenAiSchemaAdapter;
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
    fn test_non_strict_converts_const_to_enum() {
        let adapter = OpenAiSchemaAdapter;
        let schema = json!({
            "type": "string",
            "const": "fixed_value"
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("const").is_none());
        assert_eq!(result["enum"], json!(["fixed_value"]));
    }

    #[test]
    fn test_non_strict_adds_implicit_object_type() {
        let adapter = OpenAiSchemaAdapter;
        let schema = json!({
            "properties": {
                "name": { "type": "string" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_non_strict_strips_unsupported_formats() {
        let adapter = OpenAiSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "created": { "type": "string", "format": "date-time" },
                "hostname": { "type": "string", "format": "hostname" },
                "id": { "type": "string", "format": "uuid" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["properties"]["created"]["format"], "date-time");
        assert!(result["properties"]["hostname"].get("format").is_none());
        assert_eq!(result["properties"]["id"]["format"], "uuid");
    }

    #[test]
    fn test_non_strict_preserves_ref() {
        let adapter = OpenAiSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "item": { "$ref": "#/$defs/Item" }
            },
            "$defs": {
                "Item": { "type": "object" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["properties"]["item"]["$ref"], "#/$defs/Item");
        assert!(result.get("$defs").is_some());
    }

    #[test]
    fn test_non_strict_preserves_any_of() {
        let adapter = OpenAiSchemaAdapter;
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
    fn test_non_strict_preserves_one_of() {
        let adapter = OpenAiSchemaAdapter;
        let schema = json!({
            "oneOf": [
                { "type": "boolean" },
                { "type": "null" }
            ]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("oneOf").is_some());
    }

    #[test]
    fn test_non_strict_preserves_additional_properties() {
        let adapter = OpenAiSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "additionalProperties": true
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["additionalProperties"], true);
    }

    #[test]
    fn test_non_strict_does_not_collapse_type_arrays() {
        let adapter = OpenAiSchemaAdapter;
        let schema = json!({
            "type": ["string", "null"]
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["type"], json!(["string", "null"]));
    }

    #[test]
    fn test_non_strict_empty_schema() {
        let adapter = OpenAiSchemaAdapter;
        let empty = adapter.empty_schema();
        assert_eq!(empty, json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn test_non_strict_idempotent() {
        let adapter = OpenAiSchemaAdapter;
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "properties": {
                "name": { "type": "string", "const": "test", "format": "hostname" }
            },
            "if": { "const": true },
            "then": { "required": ["name"] }
        });
        let first = adapter.normalize_schema(schema);
        let second = adapter.normalize_schema(first.clone());
        assert_eq!(first, second);
    }

    #[test]
    fn test_non_strict_preserves_all_formats_in_allowed_list() {
        let adapter = OpenAiSchemaAdapter;
        for format in OPENAI_ALLOWED_FORMATS {
            let schema = json!({ "type": "string", "format": format });
            let result = adapter.normalize_schema(schema);
            assert_eq!(result["format"], *format, "format '{format}' should be preserved");
        }
    }

    /// Verifies OpenAI strict mode preserves `$ref`/`$defs`, `anyOf`, and type arrays
    /// while recursively adding `additionalProperties: false` to all object schemas.
    ///
    /// **Validates: Requirements 4.1, 4.2, 4.3**
    #[test]
    fn test_strict_preserves_ref_defs_and_adds_additional_properties() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "$defs": {
                "Address": {
                    "type": "object",
                    "properties": {
                        "street": { "type": "string" }
                    }
                }
            },
            "properties": {
                "home": { "$ref": "#/$defs/Address" },
                "name": { "type": ["string", "null"] },
                "status": {
                    "anyOf": [
                        { "type": "string" },
                        { "type": "null" }
                    ]
                }
            }
        });

        let result = adapter.normalize_schema(schema);

        // 1. $ref and $defs are preserved (Requirement 4.1)
        assert!(result.get("$defs").is_some(), "$defs should be preserved");
        assert_eq!(
            result["properties"]["home"]["$ref"], "#/$defs/Address",
            "$ref should be preserved"
        );

        // 2. anyOf is preserved (Requirement 4.2)
        assert!(result["properties"]["status"].get("anyOf").is_some(), "anyOf should be preserved");
        let any_of = result["properties"]["status"]["anyOf"].as_array().unwrap();
        assert_eq!(any_of.len(), 2, "anyOf should retain both sub-schemas");
        assert_eq!(any_of[0]["type"], "string");
        assert_eq!(any_of[1]["type"], "null");

        // 3. additionalProperties: false added to root object (Requirement 4.3)
        assert_eq!(
            result["additionalProperties"], false,
            "root object should have additionalProperties: false"
        );

        // 4. additionalProperties: false added recursively to nested object schemas (Requirement 4.4)
        assert_eq!(
            result["$defs"]["Address"]["additionalProperties"], false,
            "nested object in $defs should have additionalProperties: false"
        );

        // 5. Type arrays are preserved (Requirement 4.5)
        assert_eq!(
            result["properties"]["name"]["type"],
            json!(["string", "null"]),
            "type arrays should be preserved"
        );
    }

    /// Verifies OpenAI strict mode with `oneOf` keyword preservation.
    ///
    /// **Validates: Requirements 4.2**
    #[test]
    fn test_strict_preserves_one_of_with_object_schemas() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "payload": {
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string" }
                            }
                        },
                        { "type": "null" }
                    ]
                }
            }
        });

        let result = adapter.normalize_schema(schema);

        // oneOf is preserved
        assert!(
            result["properties"]["payload"].get("oneOf").is_some(),
            "oneOf should be preserved"
        );
        let one_of = result["properties"]["payload"]["oneOf"].as_array().unwrap();
        assert_eq!(one_of.len(), 2);

        // additionalProperties: false added to object sub-schemas within oneOf
        assert_eq!(
            one_of[0]["additionalProperties"], false,
            "object sub-schema in oneOf should have additionalProperties: false"
        );

        // Root also has additionalProperties: false
        assert_eq!(result["additionalProperties"], false);
    }

    #[test]
    fn test_strict_all_transforms_combined() {
        let adapter = OpenAiStrictSchemaAdapter;
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "properties": {
                "status": { "type": "string", "const": "active" },
                "host": { "type": "string", "format": "hostname" },
                "nested": {
                    "type": "object",
                    "properties": {
                        "value": { "type": "number", "format": "float" }
                    }
                }
            },
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
        // const converted to enum
        assert!(result["properties"]["status"].get("const").is_none());
        assert_eq!(result["properties"]["status"]["enum"], json!(["active"]));
        // unsupported format stripped
        assert!(result["properties"]["host"].get("format").is_none());
        // allowed format preserved
        assert_eq!(result["properties"]["nested"]["properties"]["value"]["format"], "float");
        // additionalProperties: false set recursively
        assert_eq!(result["additionalProperties"], false);
        assert_eq!(result["properties"]["nested"]["additionalProperties"], false);
    }
}
