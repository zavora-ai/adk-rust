//! Gemini-specific schema normalization adapter.
//!
//! The [`GeminiSchemaAdapter`] applies all destructive transforms required by
//! Gemini's function-calling API. It composes shared utilities from
//! [`adk_core::schema_utils`] with Gemini-specific keyword removal to produce
//! schemas that Gemini accepts.
//!
//! # Transform Order
//!
//! 1. Resolve `$ref` references (inline from definitions/$defs, break cycles at depth 10)
//! 2. Strip `$schema` keyword
//! 3. Collapse `anyOf`/`oneOf` combiners (select first non-null sub-schema)
//! 4. Merge `allOf` sub-schemas
//! 5. Collapse type arrays (`["string", "null"]` → `"string"`)
//! 6. Strip conditional keywords (`if`/`then`/`else`)
//! 7. Convert `const` to single-element `enum`
//! 8. Strip null values from `enum` arrays
//! 9. Add implicit `"type": "object"` when `properties` exists
//! 10. Remove unsupported keywords recursively
//! 11. Strip unsupported `format` values
//! 12. Enforce nesting depth limit (5 levels)
//! 13. Remove `definitions`/`$defs` blocks
//!
//! # Example
//!
//! ```rust
//! use adk_gemini::schema_adapter::GeminiSchemaAdapter;
//! use adk_core::SchemaAdapter;
//! use serde_json::json;
//!
//! let adapter = GeminiSchemaAdapter::new();
//! let schema = json!({
//!     "$schema": "http://json-schema.org/draft-07/schema#",
//!     "type": "object",
//!     "properties": {
//!         "name": { "type": "string", "format": "hostname" }
//!     },
//!     "additionalProperties": true
//! });
//!
//! let normalized = adapter.normalize_schema(schema);
//! assert!(normalized.get("$schema").is_none());
//! assert!(normalized.get("additionalProperties").is_none());
//! assert!(normalized["properties"]["name"].get("format").is_none());
//! ```

use adk_core::SchemaAdapter;
use adk_core::schema_utils;
use serde_json::{Map, Value};
use std::borrow::Cow;

/// Allowed `format` values for the Gemini API.
const GEMINI_ALLOWED_FORMATS: &[&str] =
    &["date-time", "date", "time", "email", "uri", "uuid", "int32", "int64", "float", "double"];

/// Keywords that Gemini does not support and must be removed from all schema nodes
/// (standard API surface — removes `additionalProperties` entirely).
const UNSUPPORTED_KEYWORDS: &[&str] = &[
    "additionalProperties",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "not",
    "propertyNames",
    "patternProperties",
    "unevaluatedProperties",
];

/// Keywords that Gemini does not support on the Vertex AI surface.
/// Unlike the standard surface, Vertex AI requires `additionalProperties: false`
/// on object schemas rather than removing it.
const UNSUPPORTED_KEYWORDS_VERTEX: &[&str] = &[
    "exclusiveMinimum",
    "exclusiveMaximum",
    "not",
    "propertyNames",
    "patternProperties",
    "unevaluatedProperties",
];

/// Schema adapter for the Gemini API surface.
///
/// Applies all destructive transforms required by Gemini's function-calling API.
///
/// Two variants are supported:
/// - **Standard** (`GeminiSchemaAdapter::new()`): Removes `additionalProperties` entirely.
/// - **Vertex AI** (`GeminiSchemaAdapter::vertex_ai()`): Sets `additionalProperties: false`
///   on object schemas instead of removing it.
///
/// # Example
///
/// ```rust
/// use adk_gemini::schema_adapter::GeminiSchemaAdapter;
/// use adk_core::SchemaAdapter;
/// use serde_json::json;
///
/// let adapter = GeminiSchemaAdapter::new();
/// let schema = json!({
///     "anyOf": [
///         {"type": "null"},
///         {"type": "string", "minLength": 1}
///     ]
/// });
///
/// let normalized = adapter.normalize_schema(schema);
/// assert_eq!(normalized["type"], "string");
/// assert!(normalized.get("anyOf").is_none());
/// ```
#[derive(Debug)]
pub struct GeminiSchemaAdapter {
    /// When `true`, targets the Vertex AI surface which requires
    /// `additionalProperties: false` on object schemas.
    vertex_ai: bool,
}

impl GeminiSchemaAdapter {
    /// Creates a new `GeminiSchemaAdapter` for the standard Gemini API surface.
    ///
    /// This variant removes `additionalProperties` from all schema nodes.
    pub fn new() -> Self {
        Self { vertex_ai: false }
    }

    /// Creates a new `GeminiSchemaAdapter` for the Vertex AI surface.
    ///
    /// This variant sets `additionalProperties: false` on object schemas
    /// instead of removing the keyword entirely.
    pub fn vertex_ai() -> Self {
        Self { vertex_ai: true }
    }
}

impl Default for GeminiSchemaAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaAdapter for GeminiSchemaAdapter {
    fn normalize_schema(&self, mut schema: Value) -> Value {
        // Step 1: Extract definitions and resolve $ref references.
        // Always resolve refs — even with empty definitions — so that
        // unresolvable $ref values are replaced with {"type": "object"}.
        let definitions = extract_definitions(&schema);
        schema_utils::resolve_refs(&mut schema, &definitions, 0);

        // Step 2: Strip $schema keyword
        schema_utils::strip_schema_keyword(&mut schema);

        // Step 3: Collapse anyOf/oneOf combiners
        schema_utils::collapse_combiners(&mut schema);

        // Step 4: Merge allOf sub-schemas
        schema_utils::merge_all_of(&mut schema);

        // Step 5: Collapse type arrays
        schema_utils::collapse_type_arrays(&mut schema);

        // Step 6: Strip conditional keywords (if/then/else)
        schema_utils::strip_conditional_keywords(&mut schema);

        // Step 7: Convert const to single-element enum
        schema_utils::convert_const_to_enum(&mut schema);

        // Step 8: Strip null from enum arrays
        schema_utils::strip_null_from_enum(&mut schema);

        // Step 9: Add implicit object type
        schema_utils::add_implicit_object_type(&mut schema);

        // Step 10: Remove unsupported keywords recursively
        if self.vertex_ai {
            remove_unsupported_keywords_vertex(&mut schema);
        } else {
            remove_unsupported_keywords(&mut schema);
        }

        // Step 11: Strip unsupported format values
        schema_utils::strip_unsupported_formats(&mut schema, GEMINI_ALLOWED_FORMATS);

        // Step 12: Enforce nesting depth (max 5 levels)
        schema_utils::enforce_nesting_depth(&mut schema, 5, 0);

        // Step 13: Remove definitions/$defs blocks
        if let Some(obj) = schema.as_object_mut() {
            obj.remove("definitions");
            obj.remove("$defs");
        }

        schema
    }

    /// Truncates tool names exceeding 64 bytes at a valid UTF-8 character boundary.
    ///
    /// Preserves the prefix of the name, truncating from the end.
    fn normalize_tool_name<'a>(&self, name: &'a str) -> Cow<'a, str> {
        if name.len() <= 64 {
            Cow::Borrowed(name)
        } else {
            let mut end = 64;
            while end > 0 && !name.is_char_boundary(end) {
                end -= 1;
            }
            Cow::Owned(name[..end].to_string())
        }
    }

    /// Returns the fallback schema for tools with no `parameters_schema`.
    ///
    /// Gemini requires `{"type": "object", "properties": {}}` as the minimum
    /// valid function declaration parameters.
    fn empty_schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
}

/// Extracts and merges `definitions` and `$defs` from the top-level schema
/// into a single map for reference resolution.
fn extract_definitions(schema: &Value) -> Map<String, Value> {
    let mut defs = Map::new();

    if let Some(obj) = schema.as_object() {
        // Collect from "definitions" (Draft 4-7)
        if let Some(definitions) = obj.get("definitions").and_then(|v| v.as_object()) {
            for (key, value) in definitions {
                defs.insert(key.clone(), value.clone());
            }
        }

        // Collect from "$defs" (Draft 2019-09+)
        if let Some(dollar_defs) = obj.get("$defs").and_then(|v| v.as_object()) {
            for (key, value) in dollar_defs {
                defs.insert(key.clone(), value.clone());
            }
        }
    }

    defs
}

/// Recursively removes unsupported keywords from the schema and all nested sub-schemas.
///
/// Removes: `additionalProperties`, `exclusiveMinimum`, `exclusiveMaximum`,
/// `items` (when type is not "array"), `not`, `propertyNames`, `patternProperties`,
/// `unevaluatedProperties`.
fn remove_unsupported_keywords(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    // Remove standard unsupported keywords
    for keyword in UNSUPPORTED_KEYWORDS {
        obj.remove(*keyword);
    }

    // Remove `items` only when the schema type is NOT "array"
    let is_array_type = obj.get("type").and_then(|t| t.as_str()).is_some_and(|t| t == "array");
    if !is_array_type {
        obj.remove("items");
    }

    // Recurse into properties
    if let Some(props) = obj.get_mut("properties")
        && let Some(props_obj) = props.as_object_mut()
    {
        for value in props_obj.values_mut() {
            remove_unsupported_keywords(value);
        }
    }

    // Recurse into items (only present if type is "array")
    if let Some(items) = obj.get_mut("items") {
        if items.is_object() {
            remove_unsupported_keywords(items);
        } else if let Some(arr) = items.as_array_mut() {
            for item in arr.iter_mut() {
                remove_unsupported_keywords(item);
            }
        }
    }

    // Recurse into allOf, anyOf, oneOf (may still exist if not collapsed)
    for keyword in &["allOf", "anyOf", "oneOf"] {
        if let Some(arr_val) = obj.get_mut(*keyword)
            && let Some(arr) = arr_val.as_array_mut()
        {
            for sub in arr.iter_mut() {
                remove_unsupported_keywords(sub);
            }
        }
    }

    // Recurse into prefixItems
    if let Some(prefix_items) = obj.get_mut("prefixItems")
        && let Some(arr) = prefix_items.as_array_mut()
    {
        for item in arr.iter_mut() {
            remove_unsupported_keywords(item);
        }
    }
}

/// Recursively removes unsupported keywords for the Vertex AI surface.
///
/// Unlike the standard surface, Vertex AI requires `additionalProperties: false`
/// on object schemas. This function:
/// - Sets `additionalProperties` to `false` on object schemas (instead of removing it)
/// - Removes all other unsupported keywords the same as the standard surface
fn remove_unsupported_keywords_vertex(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    // Remove Vertex-specific unsupported keywords (does NOT include additionalProperties)
    for keyword in UNSUPPORTED_KEYWORDS_VERTEX {
        obj.remove(*keyword);
    }

    // For object schemas, set additionalProperties to false
    let is_object_type = obj.get("type").and_then(|t| t.as_str()).is_some_and(|t| t == "object");
    if is_object_type {
        obj.insert("additionalProperties".to_string(), Value::Bool(false));
    } else {
        // For non-object schemas, remove additionalProperties if present
        obj.remove("additionalProperties");
    }

    // Remove `items` only when the schema type is NOT "array"
    let is_array_type = obj.get("type").and_then(|t| t.as_str()).is_some_and(|t| t == "array");
    if !is_array_type {
        obj.remove("items");
    }

    // Recurse into properties
    if let Some(props) = obj.get_mut("properties")
        && let Some(props_obj) = props.as_object_mut()
    {
        for value in props_obj.values_mut() {
            remove_unsupported_keywords_vertex(value);
        }
    }

    // Recurse into items (only present if type is "array")
    if let Some(items) = obj.get_mut("items") {
        if items.is_object() {
            remove_unsupported_keywords_vertex(items);
        } else if let Some(arr) = items.as_array_mut() {
            for item in arr.iter_mut() {
                remove_unsupported_keywords_vertex(item);
            }
        }
    }

    // Recurse into allOf, anyOf, oneOf (may still exist if not collapsed)
    for keyword in &["allOf", "anyOf", "oneOf"] {
        if let Some(arr_val) = obj.get_mut(*keyword)
            && let Some(arr) = arr_val.as_array_mut()
        {
            for sub in arr.iter_mut() {
                remove_unsupported_keywords_vertex(sub);
            }
        }
    }

    // Recurse into prefixItems
    if let Some(prefix_items) = obj.get_mut("prefixItems")
        && let Some(arr) = prefix_items.as_array_mut()
    {
        for item in arr.iter_mut() {
            remove_unsupported_keywords_vertex(item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_strips_schema_keyword() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("$schema").is_none());
    }

    #[test]
    fn test_removes_additional_properties() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "additionalProperties": true
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("additionalProperties").is_none());
    }

    #[test]
    fn test_removes_exclusive_min_max() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "number",
            "exclusiveMinimum": 0,
            "exclusiveMaximum": 100
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("exclusiveMinimum").is_none());
        assert!(result.get("exclusiveMaximum").is_none());
    }

    #[test]
    fn test_removes_items_when_not_array() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "items": { "type": "string" }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("items").is_none());
    }

    #[test]
    fn test_preserves_items_when_array() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "array",
            "items": { "type": "string" }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("items").is_some());
        assert_eq!(result["items"]["type"], "string");
    }

    #[test]
    fn test_removes_not_keyword() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "string",
            "not": { "enum": ["bad"] }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("not").is_none());
    }

    #[test]
    fn test_removes_property_names() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "propertyNames": { "pattern": "^[a-z]+$" }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("propertyNames").is_none());
    }

    #[test]
    fn test_removes_pattern_properties() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "patternProperties": { "^S_": { "type": "string" } }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("patternProperties").is_none());
    }

    #[test]
    fn test_removes_unevaluated_properties() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "unevaluatedProperties": false
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("unevaluatedProperties").is_none());
    }

    #[test]
    fn test_collapses_any_of() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "anyOf": [
                { "type": "null" },
                { "type": "string", "minLength": 1 }
            ]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("anyOf").is_none());
        assert_eq!(result["type"], "string");
        assert_eq!(result["minLength"], 1);
    }

    #[test]
    fn test_collapses_one_of() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "oneOf": [
                { "type": "null" },
                { "type": "integer", "minimum": 0 }
            ]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("oneOf").is_none());
        assert_eq!(result["type"], "integer");
    }

    #[test]
    fn test_merges_all_of() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "allOf": [
                { "type": "object", "properties": { "a": { "type": "string" } } },
                { "properties": { "b": { "type": "number" } }, "required": ["b"] }
            ]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("allOf").is_none());
        assert_eq!(result["properties"]["a"]["type"], "string");
        assert_eq!(result["properties"]["b"]["type"], "number");
        assert_eq!(result["required"], json!(["b"]));
    }

    #[test]
    fn test_collapses_type_arrays() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": ["string", "null"],
            "minLength": 1
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["type"], "string");
    }

    #[test]
    fn test_strips_conditional_keywords() {
        let adapter = GeminiSchemaAdapter::new();
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
    fn test_converts_const_to_enum() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "string",
            "const": "fixed"
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("const").is_none());
        assert_eq!(result["enum"], json!(["fixed"]));
    }

    #[test]
    fn test_strips_null_from_enum() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "string",
            "enum": ["a", null, "b"]
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["enum"], json!(["a", "b"]));
    }

    #[test]
    fn test_removes_empty_enum_after_null_strip() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "string",
            "enum": [null]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("enum").is_none());
    }

    #[test]
    fn test_adds_implicit_object_type() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "properties": { "name": { "type": "string" } }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_strips_unsupported_formats() {
        let adapter = GeminiSchemaAdapter::new();
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
    fn test_preserves_all_allowed_formats() {
        let adapter = GeminiSchemaAdapter::new();
        for format in GEMINI_ALLOWED_FORMATS {
            let schema = json!({ "type": "string", "format": format });
            let result = adapter.normalize_schema(schema);
            assert_eq!(result["format"], *format, "format '{format}' should be preserved");
        }
    }

    #[test]
    fn test_enforces_nesting_depth() {
        let adapter = GeminiSchemaAdapter::new();
        // Create a schema nested 7 levels deep
        let schema = json!({
            "type": "object",
            "properties": {
                "l1": {
                    "type": "object",
                    "properties": {
                        "l2": {
                            "type": "object",
                            "properties": {
                                "l3": {
                                    "type": "object",
                                    "properties": {
                                        "l4": {
                                            "type": "object",
                                            "properties": {
                                                "l5": {
                                                    "type": "object",
                                                    "properties": {
                                                        "l6": { "type": "string" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        // At depth 5, the schema should be truncated to {"type": "object"}
        let l5 = &result["properties"]["l1"]["properties"]["l2"]["properties"]["l3"]["properties"]
            ["l4"]["properties"]["l5"];
        assert_eq!(l5, &json!({"type": "object"}));
    }

    #[test]
    fn test_resolves_refs() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": {
                "address": { "$ref": "#/definitions/Address" }
            },
            "definitions": {
                "Address": {
                    "type": "object",
                    "properties": {
                        "street": { "type": "string" }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        // $ref should be resolved
        assert!(result["properties"]["address"].get("$ref").is_none());
        assert_eq!(result["properties"]["address"]["type"], "object");
        assert_eq!(result["properties"]["address"]["properties"]["street"]["type"], "string");
        // definitions should be removed
        assert!(result.get("definitions").is_none());
    }

    #[test]
    fn test_resolves_dollar_defs() {
        let adapter = GeminiSchemaAdapter::new();
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
        assert!(result["properties"]["item"].get("$ref").is_none());
        assert_eq!(result["properties"]["item"]["type"], "object");
        assert!(result.get("$defs").is_none());
    }

    #[test]
    fn test_unresolvable_ref_becomes_object() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": {
                "unknown": { "$ref": "#/definitions/DoesNotExist" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["properties"]["unknown"], json!({"type": "object"}));
    }

    #[test]
    fn test_circular_ref_breaks() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": {
                "self_ref": { "$ref": "#/definitions/Node" }
            },
            "definitions": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "child": { "$ref": "#/definitions/Node" }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        // Should not panic and should terminate
        assert_eq!(result["properties"]["self_ref"]["type"], "object");
        assert!(result.get("definitions").is_none());
    }

    #[test]
    fn test_removes_definitions_and_defs() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "definitions": { "Foo": { "type": "string" } },
            "$defs": { "Bar": { "type": "number" } }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("definitions").is_none());
        assert!(result.get("$defs").is_none());
    }

    #[test]
    fn test_nested_unsupported_keywords_removed() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": {
                "inner": {
                    "type": "object",
                    "additionalProperties": false,
                    "exclusiveMinimum": 5,
                    "properties": {
                        "deep": {
                            "type": "number",
                            "exclusiveMaximum": 100
                        }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        let inner = &result["properties"]["inner"];
        assert!(inner.get("additionalProperties").is_none());
        assert!(inner.get("exclusiveMinimum").is_none());
        assert!(inner["properties"]["deep"].get("exclusiveMaximum").is_none());
    }

    #[test]
    fn test_full_transform_pipeline() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "definitions": {
                "Status": { "type": "string", "enum": ["active", null, "inactive"] }
            },
            "properties": {
                "name": { "type": ["string", "null"], "format": "hostname" },
                "status": { "$ref": "#/definitions/Status" },
                "config": {
                    "type": "object",
                    "additionalProperties": true,
                    "properties": {
                        "value": { "const": "fixed" }
                    }
                }
            },
            "if": { "properties": { "name": { "type": "string" } } },
            "then": { "required": ["status"] },
            "additionalProperties": false
        });
        let result = adapter.normalize_schema(schema);

        // $schema removed
        assert!(result.get("$schema").is_none());
        // definitions removed
        assert!(result.get("definitions").is_none());
        // conditional keywords removed
        assert!(result.get("if").is_none());
        assert!(result.get("then").is_none());
        // additionalProperties removed
        assert!(result.get("additionalProperties").is_none());
        // type array collapsed
        assert_eq!(result["properties"]["name"]["type"], "string");
        // unsupported format stripped
        assert!(result["properties"]["name"].get("format").is_none());
        // $ref resolved and null stripped from enum
        assert_eq!(result["properties"]["status"]["enum"], json!(["active", "inactive"]));
        // const converted to enum
        assert_eq!(result["properties"]["config"]["properties"]["value"]["enum"], json!(["fixed"]));
        // nested additionalProperties removed
        assert!(result["properties"]["config"].get("additionalProperties").is_none());
        // implicit type added
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_idempotent() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "name": { "type": ["string", "null"], "format": "hostname" },
                "items": { "type": "array", "items": { "type": "string" } }
            },
            "additionalProperties": true,
            "if": { "const": true },
            "then": { "required": ["name"] }
        });
        let first = adapter.normalize_schema(schema);
        let second = adapter.normalize_schema(first.clone());
        assert_eq!(first, second);
    }

    #[test]
    fn test_empty_schema() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({});
        let result = adapter.normalize_schema(schema);
        assert_eq!(result, json!({}));
    }

    #[test]
    fn test_array_items_nested_cleanup() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": true,
                "properties": {
                    "id": { "type": "integer", "exclusiveMinimum": 0 }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result["items"].get("additionalProperties").is_none());
        assert!(result["items"]["properties"]["id"].get("exclusiveMinimum").is_none());
    }

    // --- Task 4.2: Vertex AI surface variant tests ---

    #[test]
    fn test_vertex_ai_sets_additional_properties_false() {
        let adapter = GeminiSchemaAdapter::vertex_ai();
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "additionalProperties": true
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["additionalProperties"], json!(false));
    }

    #[test]
    fn test_vertex_ai_sets_additional_properties_false_on_nested_objects() {
        let adapter = GeminiSchemaAdapter::vertex_ai();
        let schema = json!({
            "type": "object",
            "properties": {
                "inner": {
                    "type": "object",
                    "properties": {
                        "value": { "type": "string" }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["additionalProperties"], json!(false));
        assert_eq!(result["properties"]["inner"]["additionalProperties"], json!(false));
    }

    #[test]
    fn test_vertex_ai_does_not_set_additional_properties_on_non_object() {
        let adapter = GeminiSchemaAdapter::vertex_ai();
        let schema = json!({
            "type": "string",
            "additionalProperties": true
        });
        let result = adapter.normalize_schema(schema);
        // Non-object schemas should have additionalProperties removed
        assert!(result.get("additionalProperties").is_none());
    }

    #[test]
    fn test_standard_mode_removes_additional_properties() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "additionalProperties": true
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("additionalProperties").is_none());
    }

    #[test]
    fn test_vertex_ai_still_removes_other_unsupported_keywords() {
        let adapter = GeminiSchemaAdapter::vertex_ai();
        let schema = json!({
            "type": "object",
            "properties": { "x": { "type": "number" } },
            "exclusiveMinimum": 0,
            "exclusiveMaximum": 100,
            "not": { "type": "null" },
            "propertyNames": { "pattern": "^[a-z]" },
            "patternProperties": { "^S_": { "type": "string" } },
            "unevaluatedProperties": false
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("exclusiveMinimum").is_none());
        assert!(result.get("exclusiveMaximum").is_none());
        assert!(result.get("not").is_none());
        assert!(result.get("propertyNames").is_none());
        assert!(result.get("patternProperties").is_none());
        assert!(result.get("unevaluatedProperties").is_none());
        // But additionalProperties: false is set
        assert_eq!(result["additionalProperties"], json!(false));
    }

    // --- Task 4.3: normalize_tool_name tests ---

    #[test]
    fn test_normalize_tool_name_short_name_unchanged() {
        let adapter = GeminiSchemaAdapter::new();
        let name = "get_weather";
        let result = adapter.normalize_tool_name(name);
        assert_eq!(result, "get_weather");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_normalize_tool_name_exactly_64_bytes() {
        let adapter = GeminiSchemaAdapter::new();
        let name = "a".repeat(64);
        let result = adapter.normalize_tool_name(&name);
        assert_eq!(result.len(), 64);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_normalize_tool_name_truncates_at_64_bytes() {
        let adapter = GeminiSchemaAdapter::new();
        let name = "a".repeat(100);
        let result = adapter.normalize_tool_name(&name);
        assert_eq!(result.len(), 64);
        assert_eq!(result.as_ref(), "a".repeat(64));
    }

    #[test]
    fn test_normalize_tool_name_multibyte_boundary() {
        let adapter = GeminiSchemaAdapter::new();
        // Each '日' is 3 bytes in UTF-8. 21 chars = 63 bytes.
        // Adding one more '日' would be 66 bytes, so truncation should stop at 63.
        let name = "日".repeat(22); // 66 bytes
        let result = adapter.normalize_tool_name(&name);
        assert!(result.len() <= 64);
        // Should be 63 bytes (21 chars × 3 bytes)
        assert_eq!(result.len(), 63);
        assert_eq!(result.as_ref(), "日".repeat(21));
        // Verify it's valid UTF-8
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[test]
    fn test_normalize_tool_name_emoji_boundary() {
        let adapter = GeminiSchemaAdapter::new();
        // '🎯' is 4 bytes. 16 emojis = 64 bytes exactly.
        let name = "🎯".repeat(16);
        assert_eq!(name.len(), 64);
        let result = adapter.normalize_tool_name(&name);
        assert_eq!(result.len(), 64);

        // 17 emojis = 68 bytes, should truncate to 16 emojis = 64 bytes
        let name = "🎯".repeat(17);
        let result = adapter.normalize_tool_name(&name);
        assert_eq!(result.len(), 64);
        assert_eq!(result.as_ref(), "🎯".repeat(16));
    }

    // --- Task 4.4: empty_schema tests ---

    #[test]
    fn test_empty_schema_returns_object_with_properties() {
        let adapter = GeminiSchemaAdapter::new();
        let result = adapter.empty_schema();
        assert_eq!(result, json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn test_empty_schema_vertex_ai_same_as_standard() {
        let adapter = GeminiSchemaAdapter::vertex_ai();
        let result = adapter.empty_schema();
        assert_eq!(result, json!({"type": "object", "properties": {}}));
    }
}
