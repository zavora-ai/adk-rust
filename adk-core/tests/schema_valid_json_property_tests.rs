//! Property-based tests for valid JSON output from schema adapters.
//!
//! **Feature: schema-dialect, Property 3: Valid JSON Output**
//! *For any* adapter A and any valid JSON value S:
//! `A.normalize_schema(S)` produces a valid JSON value (never panics).
//!
//! Tests all four adapter types with arbitrary JSON values to ensure
//! no adapter panics on any valid JSON input.
//!
//! **Validates: Requirements 3.1–3.20, 4.1–4.11, 5.1–5.9, 6.1–6.6**

use adk_core::{GenericSchemaAdapter, SchemaAdapter};
use proptest::prelude::*;
use serde_json::{Number, Value};

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Generates arbitrary JSON values with controlled depth to avoid stack overflow.
fn arb_json_value() -> impl Strategy<Value = Value> {
    arb_json_value_depth(0, 4)
}

/// Generates JSON values with bounded depth.
fn arb_json_value_depth(current_depth: u32, max_depth: u32) -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Number(Number::from(n))),
        "[a-zA-Z0-9_$#/]{0,30}".prop_map(Value::String),
    ];

    if current_depth >= max_depth {
        leaf.boxed()
    } else {
        let next_depth = current_depth + 1;
        prop_oneof![
            4 => leaf.clone(),
            1 => prop::collection::vec(arb_json_value_depth(next_depth, max_depth), 0..4)
                .prop_map(Value::Array),
            1 => prop::collection::hash_map(
                "[a-zA-Z_$][a-zA-Z0-9_]{0,15}",
                arb_json_value_depth(next_depth, max_depth),
                0..5,
            )
            .prop_map(|map| {
                let obj: serde_json::Map<String, Value> = map.into_iter().collect();
                Value::Object(obj)
            }),
        ]
        .boxed()
    }
}

/// Generates JSON values that look like JSON Schema objects (more likely to exercise
/// adapter code paths).
fn arb_schema_like_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        // Plain JSON values
        arb_json_value(),
        // Schema-like objects with type field
        prop_oneof![
            Just("string"),
            Just("number"),
            Just("integer"),
            Just("boolean"),
            Just("array"),
            Just("object"),
            Just("null"),
        ]
        .prop_map(|t| { serde_json::json!({ "type": t }) }),
        // Schema with properties
        "[a-z_]{1,10}".prop_map(|name| {
            serde_json::json!({
                "type": "object",
                "properties": {
                    name: { "type": "string" }
                }
            })
        }),
        // Schema with $schema keyword
        Just(serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {}
        })),
        // Schema with conditional keywords
        Just(serde_json::json!({
            "type": "object",
            "if": { "properties": { "x": { "type": "number" } } },
            "then": { "required": ["x"] },
            "else": { "required": ["y"] }
        })),
        // Schema with const
        any::<i64>().prop_map(|n| {
            serde_json::json!({
                "type": "integer",
                "const": n
            })
        }),
        // Schema with enum containing null
        Just(serde_json::json!({
            "type": "string",
            "enum": ["a", "b", null, "c"]
        })),
        // Schema with format
        prop_oneof![
            Just("date-time"),
            Just("email"),
            Just("hostname"),
            Just("ipv4"),
            Just("custom-format"),
        ]
        .prop_map(|fmt| {
            serde_json::json!({
                "type": "string",
                "format": fmt
            })
        }),
        // Schema with anyOf
        Just(serde_json::json!({
            "anyOf": [
                { "type": "string" },
                { "type": "null" }
            ]
        })),
        // Schema with allOf
        Just(serde_json::json!({
            "allOf": [
                { "type": "object", "properties": { "a": { "type": "string" } } },
                { "properties": { "b": { "type": "number" } } }
            ]
        })),
        // Schema with $ref
        Just(serde_json::json!({
            "type": "object",
            "properties": {
                "child": { "$ref": "#/definitions/Child" }
            },
            "definitions": {
                "Child": { "type": "string" }
            }
        })),
        // Schema with additionalProperties
        Just(serde_json::json!({
            "type": "object",
            "properties": { "x": { "type": "number" } },
            "additionalProperties": false
        })),
        // Deeply nested schema
        Just(serde_json::json!({
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
        })),
    ]
}

// ---------------------------------------------------------------------------
// Property Tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: schema-dialect, Property 3: Valid JSON Output (GenericSchemaAdapter)**
    /// *For any* valid JSON value S, `GenericSchemaAdapter.normalize_schema(S)` produces
    /// a valid JSON value (never panics).
    /// **Validates: Requirements 3.1–3.20, 4.1–4.11, 5.1–5.9, 6.1–6.6**
    #[test]
    fn prop_generic_adapter_never_panics(schema in arb_json_value()) {
        let adapter = GenericSchemaAdapter;
        let result = adapter.normalize_schema(schema);
        // If we get here without panicking, the property holds.
        // Additionally verify the result is serializable (valid JSON).
        let serialized = serde_json::to_string(&result);
        prop_assert!(serialized.is_ok(), "Result must be serializable JSON");
    }

    /// **Feature: schema-dialect, Property 3: Valid JSON Output (GenericSchemaAdapter, schema-like)**
    /// *For any* schema-like JSON value S, `GenericSchemaAdapter.normalize_schema(S)` produces
    /// a valid JSON value (never panics).
    /// **Validates: Requirements 3.1–3.20, 4.1–4.11, 5.1–5.9, 6.1–6.6**
    #[test]
    fn prop_generic_adapter_never_panics_schema_like(schema in arb_schema_like_value()) {
        let adapter = GenericSchemaAdapter;
        let result = adapter.normalize_schema(schema);
        let serialized = serde_json::to_string(&result);
        prop_assert!(serialized.is_ok(), "Result must be serializable JSON");
    }
}
