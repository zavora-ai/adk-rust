//! End-to-end tests verifying that `GeminiSchemaAdapter` produces output
//! equivalent to the old `sanitize_schema` function for simple schemas
//! (no `$ref`, `anyOf`, `oneOf`, `allOf`).
//!
//! **Validates: Requirements 9.1, 9.3**

use adk_core::SchemaAdapter;
use adk_gemini::GeminiSchemaAdapter;
use serde_json::json;

/// Test the exact example from the task description:
/// A schema with `$schema`, `additionalProperties`, type arrays, and `exclusiveMinimum`
/// should be normalized to the expected output matching old `sanitize_schema` behavior.
#[test]
fn test_e2e_simple_schema_matches_old_sanitize_schema() {
    let adapter = GeminiSchemaAdapter::new();

    let input = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "name": { "type": ["string", "null"] },
            "count": { "type": "integer", "exclusiveMinimum": 0 }
        },
        "additionalProperties": false
    });

    let expected = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "count": { "type": "integer" }
        }
    });

    let result = adapter.normalize_schema(input);
    assert_eq!(result, expected);
}

/// Verify that a schema with only basic types and no special keywords
/// passes through with just the type preserved.
#[test]
fn test_e2e_minimal_object_schema() {
    let adapter = GeminiSchemaAdapter::new();

    let input = json!({
        "type": "object",
        "properties": {
            "message": { "type": "string" }
        }
    });

    let expected = json!({
        "type": "object",
        "properties": {
            "message": { "type": "string" }
        }
    });

    let result = adapter.normalize_schema(input);
    assert_eq!(result, expected);
}

/// Verify that `$schema` is stripped, `additionalProperties` is removed,
/// and `items` on a non-array type is removed — all behaviors from old sanitize_schema.
#[test]
fn test_e2e_strips_schema_and_additional_properties_and_items_on_non_array() {
    let adapter = GeminiSchemaAdapter::new();

    let input = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "tags": {
                "type": "array",
                "items": { "type": "string" }
            },
            "label": {
                "type": "string",
                "items": { "type": "number" }
            }
        },
        "additionalProperties": true
    });

    let expected = json!({
        "type": "object",
        "properties": {
            "tags": {
                "type": "array",
                "items": { "type": "string" }
            },
            "label": {
                "type": "string"
            }
        }
    });

    let result = adapter.normalize_schema(input);
    assert_eq!(result, expected);
}

/// Verify that type arrays are collapsed to the first non-null type,
/// matching old sanitize_schema behavior.
#[test]
fn test_e2e_collapses_type_arrays() {
    let adapter = GeminiSchemaAdapter::new();

    let input = json!({
        "type": "object",
        "properties": {
            "age": { "type": ["integer", "null"] },
            "score": { "type": ["number", "null"] },
            "active": { "type": ["boolean", "null"] }
        }
    });

    let expected = json!({
        "type": "object",
        "properties": {
            "age": { "type": "integer" },
            "score": { "type": "number" },
            "active": { "type": "boolean" }
        }
    });

    let result = adapter.normalize_schema(input);
    assert_eq!(result, expected);
}

/// Verify that unsupported format values are stripped while allowed ones are preserved.
#[test]
fn test_e2e_format_handling() {
    let adapter = GeminiSchemaAdapter::new();

    let input = json!({
        "type": "object",
        "properties": {
            "created_at": { "type": "string", "format": "date-time" },
            "email": { "type": "string", "format": "email" },
            "hostname": { "type": "string", "format": "hostname" },
            "count": { "type": "integer", "format": "int32" }
        }
    });

    let expected = json!({
        "type": "object",
        "properties": {
            "created_at": { "type": "string", "format": "date-time" },
            "email": { "type": "string", "format": "email" },
            "hostname": { "type": "string" },
            "count": { "type": "integer", "format": "int32" }
        }
    });

    let result = adapter.normalize_schema(input);
    assert_eq!(result, expected);
}

/// Verify that `exclusiveMinimum` and `exclusiveMaximum` are removed.
#[test]
fn test_e2e_removes_exclusive_min_max() {
    let adapter = GeminiSchemaAdapter::new();

    let input = json!({
        "type": "object",
        "properties": {
            "temperature": {
                "type": "number",
                "exclusiveMinimum": 0,
                "exclusiveMaximum": 100
            }
        }
    });

    let expected = json!({
        "type": "object",
        "properties": {
            "temperature": { "type": "number" }
        }
    });

    let result = adapter.normalize_schema(input);
    assert_eq!(result, expected);
}

/// Verify that conditional keywords (if/then/else) are stripped.
#[test]
fn test_e2e_strips_conditional_keywords() {
    let adapter = GeminiSchemaAdapter::new();

    let input = json!({
        "type": "object",
        "properties": {
            "mode": { "type": "string" }
        },
        "if": { "properties": { "mode": { "const": "advanced" } } },
        "then": { "required": ["extra"] },
        "else": { "required": [] }
    });

    let expected = json!({
        "type": "object",
        "properties": {
            "mode": { "type": "string" }
        }
    });

    let result = adapter.normalize_schema(input);
    assert_eq!(result, expected);
}

/// Verify that `const` is converted to a single-element `enum` array.
#[test]
fn test_e2e_const_to_enum_conversion() {
    let adapter = GeminiSchemaAdapter::new();

    let input = json!({
        "type": "object",
        "properties": {
            "version": { "type": "string", "const": "v1" }
        }
    });

    let expected = json!({
        "type": "object",
        "properties": {
            "version": { "type": "string", "enum": ["v1"] }
        }
    });

    let result = adapter.normalize_schema(input);
    assert_eq!(result, expected);
}

/// Verify that null values are stripped from enum arrays.
#[test]
fn test_e2e_strips_null_from_enums() {
    let adapter = GeminiSchemaAdapter::new();

    let input = json!({
        "type": "object",
        "properties": {
            "status": {
                "type": "string",
                "enum": ["active", null, "inactive"]
            }
        }
    });

    let expected = json!({
        "type": "object",
        "properties": {
            "status": {
                "type": "string",
                "enum": ["active", "inactive"]
            }
        }
    });

    let result = adapter.normalize_schema(input);
    assert_eq!(result, expected);
}

/// Verify that implicit `"type": "object"` is added when `properties` exists
/// without a `type` field.
#[test]
fn test_e2e_adds_implicit_object_type() {
    let adapter = GeminiSchemaAdapter::new();

    let input = json!({
        "properties": {
            "name": { "type": "string" }
        }
    });

    let expected = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }
        }
    });

    let result = adapter.normalize_schema(input);
    assert_eq!(result, expected);
}

/// Verify a complex schema combining multiple transforms that the old
/// sanitize_schema would have applied — all without $ref/anyOf/oneOf/allOf.
#[test]
fn test_e2e_complex_schema_without_combiners() {
    let adapter = GeminiSchemaAdapter::new();

    let input = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "user": {
                "type": "object",
                "properties": {
                    "name": { "type": ["string", "null"] },
                    "email": { "type": "string", "format": "email" },
                    "age": { "type": "integer", "exclusiveMinimum": 0 }
                },
                "additionalProperties": false
            },
            "tags": {
                "type": "array",
                "items": { "type": "string" }
            },
            "status": {
                "type": "string",
                "enum": ["active", null, "disabled"],
                "const": "active"
            }
        },
        "additionalProperties": false,
        "if": { "properties": { "status": { "const": "active" } } },
        "then": { "required": ["user"] }
    });

    let expected = json!({
        "type": "object",
        "properties": {
            "user": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "email": { "type": "string", "format": "email" },
                    "age": { "type": "integer" }
                }
            },
            "tags": {
                "type": "array",
                "items": { "type": "string" }
            },
            "status": {
                "type": "string",
                "enum": ["active"]
            }
        }
    });

    let result = adapter.normalize_schema(input);
    assert_eq!(result, expected);
}
