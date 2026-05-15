//! Unit tests for schema normalization edge cases.
//!
//! Tests null handling in enums, const-to-enum conversion, and format value handling
//! across different adapter behaviors. Since the shared utilities are in `adk-core`,
//! we test them directly here. Adapter-specific behavior (Gemini, OpenAI, Anthropic)
//! is validated by testing the utility functions that each adapter composes.
//!
//! **Validates: Requirements 11.1–11.3, 12.1–12.5, 13.1–13.6**

use adk_core::schema_utils;
use adk_core::{GenericSchemaAdapter, SchemaAdapter};
use serde_json::json;

// ===========================================================================
// Task 12.4: Null handling in enums
// ===========================================================================

/// **Validates: Requirement 11.1**
/// WHEN the Gemini_Adapter encounters an `enum` array containing a JSON null value,
/// THE Gemini_Adapter SHALL remove the null value from the array.
///
/// Tests that `strip_null_from_enum` removes null values from enum arrays.
/// (Gemini adapter calls this utility.)
#[test]
fn test_gemini_strips_null_from_enum_arrays() {
    let mut schema = json!({
        "type": "string",
        "enum": ["active", null, "inactive", null]
    });

    schema_utils::strip_null_from_enum(&mut schema);

    assert_eq!(schema["enum"], json!(["active", "inactive"]));
}

/// **Validates: Requirement 11.1**
/// Tests that null stripping works with a single null among valid values.
#[test]
fn test_gemini_strips_single_null_from_enum() {
    let mut schema = json!({
        "type": "string",
        "enum": ["yes", "no", null]
    });

    schema_utils::strip_null_from_enum(&mut schema);

    assert_eq!(schema["enum"], json!(["yes", "no"]));
}

/// **Validates: Requirement 11.2**
/// IF removing null values results in an empty `enum` array,
/// THEN THE Gemini_Adapter SHALL remove the `enum` keyword entirely.
#[test]
fn test_gemini_removes_empty_enum_after_null_stripping() {
    let mut schema = json!({
        "type": "string",
        "enum": [null, null]
    });

    schema_utils::strip_null_from_enum(&mut schema);

    assert!(
        schema.get("enum").is_none(),
        "enum keyword should be removed when all values are null"
    );
}

/// **Validates: Requirement 11.2**
/// Tests that a single-null enum is removed entirely.
#[test]
fn test_gemini_removes_enum_with_only_null() {
    let mut schema = json!({
        "type": "string",
        "enum": [null]
    });

    schema_utils::strip_null_from_enum(&mut schema);

    assert!(schema.get("enum").is_none());
}

/// **Validates: Requirement 11.3**
/// THE OpenAi_Adapter and OpenAiStrict_Adapter SHALL preserve null values in `enum` arrays.
///
/// The GenericSchemaAdapter (used by OpenAI non-strict) does NOT call `strip_null_from_enum`,
/// so null values are preserved. We verify this by running the full adapter pipeline.
#[test]
fn test_openai_preserves_null_in_enums() {
    let adapter = GenericSchemaAdapter;
    let schema = json!({
        "type": "string",
        "enum": ["active", null, "inactive"]
    });

    let result = adapter.normalize_schema(schema);

    // GenericSchemaAdapter does not strip null from enums
    assert_eq!(result["enum"], json!(["active", null, "inactive"]));
}

/// **Validates: Requirement 11.3**
/// Tests that OpenAI preserves an enum that is entirely null values.
#[test]
fn test_openai_preserves_all_null_enum() {
    let adapter = GenericSchemaAdapter;
    let schema = json!({
        "type": "string",
        "enum": [null, null]
    });

    let result = adapter.normalize_schema(schema);

    assert_eq!(result["enum"], json!([null, null]));
}

/// **Validates: Requirement 11.1**
/// Tests null stripping in nested schemas (recursive behavior).
#[test]
fn test_gemini_strips_null_from_nested_enums() {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "status": {
                "type": "string",
                "enum": ["on", null, "off"]
            },
            "mode": {
                "type": "string",
                "enum": [null]
            }
        }
    });

    schema_utils::strip_null_from_enum(&mut schema);

    assert_eq!(schema["properties"]["status"]["enum"], json!(["on", "off"]));
    assert!(schema["properties"]["mode"].get("enum").is_none());
}

/// Tests that enum arrays without null are left unchanged.
#[test]
fn test_strip_null_from_enum_no_null_present() {
    let mut schema = json!({
        "type": "string",
        "enum": ["a", "b", "c"]
    });

    schema_utils::strip_null_from_enum(&mut schema);

    assert_eq!(schema["enum"], json!(["a", "b", "c"]));
}

// ===========================================================================
// Task 12.5: Const-to-enum conversion
// ===========================================================================

/// **Validates: Requirement 12.1**
/// WHEN a schema contains a `const` keyword, THE Gemini_Adapter SHALL replace it
/// with an `enum` array containing only that value.
///
/// Tests via the shared utility that Gemini calls.
#[test]
fn test_gemini_converts_const_string_to_enum() {
    let mut schema = json!({
        "type": "string",
        "const": "fixed_value"
    });

    schema_utils::convert_const_to_enum(&mut schema);

    assert!(schema.get("const").is_none(), "const should be removed");
    assert_eq!(schema["enum"], json!(["fixed_value"]));
}

/// **Validates: Requirement 12.2**
/// WHEN a schema contains a `const` keyword, THE OpenAi_Adapter SHALL replace it
/// with an `enum` array containing only that value.
///
/// The GenericSchemaAdapter (same pipeline as OpenAI non-strict) calls convert_const_to_enum.
#[test]
fn test_openai_converts_const_to_enum() {
    let adapter = GenericSchemaAdapter;
    let schema = json!({
        "type": "integer",
        "const": 42
    });

    let result = adapter.normalize_schema(schema);

    assert!(result.get("const").is_none());
    assert_eq!(result["enum"], json!([42]));
}

/// **Validates: Requirement 12.3**
/// WHEN a schema contains a `const` keyword, THE OpenAiStrict_Adapter SHALL replace it
/// with an `enum` array containing only that value.
///
/// OpenAI strict also calls convert_const_to_enum (same shared utility).
#[test]
fn test_openai_strict_converts_const_to_enum() {
    // OpenAI strict uses the same convert_const_to_enum utility
    let mut schema = json!({
        "type": "boolean",
        "const": true
    });

    schema_utils::convert_const_to_enum(&mut schema);

    assert!(schema.get("const").is_none());
    assert_eq!(schema["enum"], json!([true]));
}

/// **Validates: Requirement 12.4**
/// THE Anthropic_Adapter SHALL preserve `const` keywords without modification.
///
/// Anthropic does NOT call convert_const_to_enum, so const is preserved.
/// We verify by checking that the utility is not applied (Anthropic's pipeline
/// only calls strip_schema_keyword, strip_conditional_keywords, add_implicit_object_type).
#[test]
fn test_anthropic_preserves_const() {
    // Simulate Anthropic's pipeline: only strip_schema_keyword, strip_conditional_keywords,
    // add_implicit_object_type — no convert_const_to_enum
    let mut schema = json!({
        "type": "string",
        "const": "preserved_value"
    });

    // Apply only what Anthropic applies
    schema_utils::strip_schema_keyword(&mut schema);
    schema_utils::strip_conditional_keywords(&mut schema);
    schema_utils::add_implicit_object_type(&mut schema);

    // const should still be present
    assert_eq!(schema["const"], json!("preserved_value"));
    assert!(schema.get("enum").is_none(), "enum should not be created");
}

/// **Validates: Requirement 12.5**
/// WHEN the `const` value is null and the adapter strips null from enums,
/// THE adapter SHALL remove both `const` and the resulting empty `enum`.
///
/// Gemini: convert_const_to_enum(null) → enum: [null] → strip_null_from_enum → remove enum
#[test]
fn test_null_const_with_null_stripping_adapter() {
    let mut schema = json!({
        "type": "string",
        "const": null
    });

    // Gemini pipeline: convert_const_to_enum then strip_null_from_enum
    schema_utils::convert_const_to_enum(&mut schema);
    assert_eq!(schema["enum"], json!([null]), "const null becomes enum [null]");
    assert!(schema.get("const").is_none());

    schema_utils::strip_null_from_enum(&mut schema);
    assert!(
        schema.get("enum").is_none(),
        "enum should be removed after stripping the only null value"
    );
}

/// **Validates: Requirement 12.1**
/// Tests const-to-enum conversion with nested schemas.
#[test]
fn test_const_to_enum_nested() {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "version": {
                "type": "string",
                "const": "v1"
            },
            "count": {
                "type": "integer",
                "const": 0
            }
        }
    });

    schema_utils::convert_const_to_enum(&mut schema);

    assert_eq!(schema["properties"]["version"]["enum"], json!(["v1"]));
    assert!(schema["properties"]["version"].get("const").is_none());
    assert_eq!(schema["properties"]["count"]["enum"], json!([0]));
    assert!(schema["properties"]["count"].get("const").is_none());
}

/// **Validates: Requirement 12.1**
/// Tests const-to-enum with an object const value.
#[test]
fn test_const_to_enum_object_value() {
    let mut schema = json!({
        "const": {"key": "value"}
    });

    schema_utils::convert_const_to_enum(&mut schema);

    assert!(schema.get("const").is_none());
    assert_eq!(schema["enum"], json!([{"key": "value"}]));
}

/// **Validates: Requirement 12.1**
/// Tests const-to-enum with an array const value.
#[test]
fn test_const_to_enum_array_value() {
    let mut schema = json!({
        "const": [1, 2, 3]
    });

    schema_utils::convert_const_to_enum(&mut schema);

    assert!(schema.get("const").is_none());
    assert_eq!(schema["enum"], json!([[1, 2, 3]]));
}

// ===========================================================================
// Task 12.6: Format value handling
// ===========================================================================

/// Allowed formats for Gemini/Generic/OpenAI adapters.
const ALLOWED_FORMATS: &[&str] =
    &["date-time", "date", "time", "email", "uri", "uuid", "int32", "int64", "float", "double"];

/// **Validates: Requirement 13.1**
/// THE Gemini_Adapter SHALL retain `format` values: date-time, date, time, email,
/// uri, uuid, int32, int64, float, double.
#[test]
fn test_gemini_retains_allowed_formats() {
    for &fmt in ALLOWED_FORMATS {
        let mut schema = json!({
            "type": "string",
            "format": fmt
        });

        schema_utils::strip_unsupported_formats(&mut schema, ALLOWED_FORMATS);

        assert_eq!(schema["format"], fmt, "format '{fmt}' should be retained");
    }
}

/// **Validates: Requirement 13.2**
/// WHEN a `format` value is not in the allowed list, THE Gemini_Adapter SHALL
/// remove the `format` keyword from that schema node.
#[test]
fn test_gemini_strips_unsupported_formats() {
    let unsupported = ["hostname", "ipv4", "ipv6", "iri", "json-pointer", "regex", "custom"];

    for fmt in unsupported {
        let mut schema = json!({
            "type": "string",
            "format": fmt
        });

        schema_utils::strip_unsupported_formats(&mut schema, ALLOWED_FORMATS);

        assert!(schema.get("format").is_none(), "format '{fmt}' should be stripped");
    }
}

/// **Validates: Requirement 13.6**
/// THE adapter SHALL apply format handling recursively to all nested schemas.
#[test]
fn test_recursive_format_handling_in_nested_schemas() {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "email_field": {
                "type": "string",
                "format": "email"
            },
            "hostname_field": {
                "type": "string",
                "format": "hostname"
            },
            "nested": {
                "type": "object",
                "properties": {
                    "date_field": {
                        "type": "string",
                        "format": "date-time"
                    },
                    "custom_field": {
                        "type": "string",
                        "format": "custom-format"
                    }
                }
            }
        }
    });

    schema_utils::strip_unsupported_formats(&mut schema, ALLOWED_FORMATS);

    // Allowed formats retained
    assert_eq!(schema["properties"]["email_field"]["format"], "email");
    assert_eq!(schema["properties"]["nested"]["properties"]["date_field"]["format"], "date-time");

    // Unsupported formats stripped
    assert!(schema["properties"]["hostname_field"].get("format").is_none());
    assert!(schema["properties"]["nested"]["properties"]["custom_field"].get("format").is_none());
}

/// **Validates: Requirement 13.5**
/// THE Anthropic_Adapter SHALL preserve all `format` values without modification.
///
/// Anthropic does NOT call strip_unsupported_formats, so all formats are preserved.
#[test]
fn test_anthropic_preserves_all_formats() {
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
        "iri",
        "json-pointer",
        "regex",
        "custom-format",
    ];

    for fmt in formats {
        // Simulate Anthropic's pipeline (no strip_unsupported_formats call)
        let mut schema = json!({
            "type": "string",
            "format": fmt
        });

        // Anthropic only applies these three transforms:
        schema_utils::strip_schema_keyword(&mut schema);
        schema_utils::strip_conditional_keywords(&mut schema);
        schema_utils::add_implicit_object_type(&mut schema);

        assert_eq!(schema["format"], fmt, "Anthropic should preserve format '{fmt}'");
    }
}

/// **Validates: Requirement 13.3, 13.4**
/// THE OpenAi_Adapter and OpenAiStrict_Adapter SHALL strip unsupported `format` values
/// using the same allowed list.
///
/// GenericSchemaAdapter uses the same pipeline as OpenAI non-strict.
#[test]
fn test_openai_strips_unsupported_formats() {
    let adapter = GenericSchemaAdapter;

    let schema = json!({
        "type": "string",
        "format": "hostname"
    });

    let result = adapter.normalize_schema(schema);
    assert!(result.get("format").is_none());
}

/// **Validates: Requirement 13.3, 13.4**
/// Tests that OpenAI retains allowed formats.
#[test]
fn test_openai_retains_allowed_formats() {
    let adapter = GenericSchemaAdapter;

    let schema = json!({
        "type": "string",
        "format": "date-time"
    });

    let result = adapter.normalize_schema(schema);
    assert_eq!(result["format"], "date-time");
}

/// **Validates: Requirement 13.6**
/// Tests format handling in array items.
#[test]
fn test_format_handling_in_array_items() {
    let mut schema = json!({
        "type": "array",
        "items": {
            "type": "string",
            "format": "ipv4"
        }
    });

    schema_utils::strip_unsupported_formats(&mut schema, ALLOWED_FORMATS);

    assert!(schema["items"].get("format").is_none());
}

/// **Validates: Requirement 13.6**
/// Tests format handling in additionalProperties.
#[test]
fn test_format_handling_in_additional_properties() {
    let mut schema = json!({
        "type": "object",
        "additionalProperties": {
            "type": "string",
            "format": "uri"
        }
    });

    schema_utils::strip_unsupported_formats(&mut schema, ALLOWED_FORMATS);

    // uri is allowed, should be retained
    assert_eq!(schema["additionalProperties"]["format"], "uri");
}

/// **Validates: Requirement 13.6**
/// Tests format handling in anyOf sub-schemas.
#[test]
fn test_format_handling_in_any_of() {
    let mut schema = json!({
        "anyOf": [
            { "type": "string", "format": "email" },
            { "type": "string", "format": "hostname" }
        ]
    });

    schema_utils::strip_unsupported_formats(&mut schema, ALLOWED_FORMATS);

    assert_eq!(schema["anyOf"][0]["format"], "email");
    assert!(schema["anyOf"][1].get("format").is_none());
}
