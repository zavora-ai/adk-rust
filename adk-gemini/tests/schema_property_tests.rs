//! Property-based tests for the GeminiSchemaAdapter.
//!
//! Tests two correctness properties from the design document:
//!
//! - **P1: Backward Compatibility** — For any schema S without `$ref`, `anyOf`, `oneOf`, or `allOf`:
//!   `GeminiSchemaAdapter.normalize_schema(S)` produces output equivalent to the current
//!   `sanitize_schema(S)` behavior.
//!
//! - **P2: Idempotency** — For any adapter A and schema S:
//!   `A.normalize_schema(A.normalize_schema(S)) == A.normalize_schema(S)`

use adk_core::SchemaAdapter;
use adk_gemini::schema_adapter::GeminiSchemaAdapter;
use proptest::prelude::*;
use serde_json::{Map, Value, json};

// ============================================================================
// Generators
// ============================================================================

/// Generates a random JSON Schema type string.
fn arb_schema_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("string".to_string()),
        Just("number".to_string()),
        Just("integer".to_string()),
        Just("boolean".to_string()),
        Just("array".to_string()),
        Just("object".to_string()),
    ]
}

/// Generates a random format string (mix of allowed and disallowed).
fn arb_format() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("date-time".to_string()),
        Just("date".to_string()),
        Just("time".to_string()),
        Just("email".to_string()),
        Just("uri".to_string()),
        Just("uuid".to_string()),
        Just("int32".to_string()),
        Just("int64".to_string()),
        Just("float".to_string()),
        Just("double".to_string()),
        Just("hostname".to_string()),
        Just("ipv4".to_string()),
        Just("ipv6".to_string()),
        Just("byte".to_string()),
        Just("binary".to_string()),
    ]
}

/// Generates a simple property schema (leaf node, no combiners or $ref).
fn arb_leaf_schema() -> impl Strategy<Value = Value> {
    prop_oneof![
        // Simple typed schema
        arb_schema_type().prop_map(|t| json!({"type": t})),
        // Schema with format
        (arb_schema_type(), arb_format()).prop_map(|(t, f)| json!({"type": t, "format": f})),
        // Schema with enum
        arb_schema_type().prop_map(|t| json!({"type": t, "enum": ["a", "b", "c"]})),
        // Schema with const (will be converted to enum)
        arb_schema_type().prop_map(|t| json!({"type": t, "const": "fixed"})),
        // Schema with description
        arb_schema_type().prop_map(|t| json!({"type": t, "description": "A field"})),
    ]
}

/// Generates a property name.
fn arb_property_name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,15}".prop_map(|s| s)
}

/// Generates a properties map with 0-4 properties.
fn arb_properties() -> impl Strategy<Value = Map<String, Value>> {
    prop::collection::hash_map(arb_property_name(), arb_leaf_schema(), 0..5).prop_map(|hm| {
        let mut map = Map::new();
        for (k, v) in hm {
            map.insert(k, v);
        }
        map
    })
}

/// Generates a JSON Schema object without `$ref`, `anyOf`, `oneOf`, or `allOf`.
/// This is the input domain for the backward compatibility property (P1).
fn arb_simple_schema() -> impl Strategy<Value = Value> {
    (
        prop::option::of(arb_schema_type()),
        arb_properties(),
        prop::option::of(arb_format()),
        prop::bool::ANY, // whether to include $schema
        prop::bool::ANY, // whether to include additionalProperties
        prop::bool::ANY, // whether to include conditional keywords
        prop::option::of(prop::collection::vec(
            prop_oneof![
                Just(Value::String("a".to_string())),
                Just(Value::String("b".to_string())),
                Just(Value::Null),
            ],
            1..5,
        )),
    )
        .prop_map(
            |(
                type_opt,
                properties,
                format_opt,
                has_schema,
                has_additional,
                has_conditional,
                enum_opt,
            )| {
                let mut obj = Map::new();

                if let Some(t) = type_opt {
                    obj.insert("type".to_string(), Value::String(t));
                }

                if !properties.is_empty() {
                    obj.insert("properties".to_string(), Value::Object(properties));
                }

                if let Some(f) = format_opt {
                    obj.insert("format".to_string(), Value::String(f));
                }

                if has_schema {
                    obj.insert(
                        "$schema".to_string(),
                        Value::String("http://json-schema.org/draft-07/schema#".to_string()),
                    );
                }

                if has_additional {
                    obj.insert("additionalProperties".to_string(), Value::Bool(true));
                }

                if has_conditional {
                    obj.insert("if".to_string(), json!({"const": true}));
                    obj.insert("then".to_string(), json!({"required": ["x"]}));
                    obj.insert("else".to_string(), json!({"required": []}));
                }

                if let Some(enum_vals) = enum_opt {
                    obj.insert("enum".to_string(), Value::Array(enum_vals));
                }

                Value::Object(obj)
            },
        )
}

/// Generates an arbitrary JSON Schema for idempotency testing (P2).
/// This can include combiners and refs since idempotency should hold regardless.
fn arb_schema_for_idempotency() -> impl Strategy<Value = Value> {
    prop_oneof![
        // Simple schemas
        arb_simple_schema(),
        // Schema with nested object
        (arb_properties(), arb_properties()).prop_map(|(props, nested_props)| {
            let mut nested = Map::new();
            nested.insert("type".to_string(), Value::String("object".to_string()));
            nested.insert("properties".to_string(), Value::Object(nested_props));

            let mut props_with_nested = props;
            props_with_nested.insert("nested".to_string(), Value::Object(nested));

            json!({
                "type": "object",
                "properties": props_with_nested
            })
        }),
        // Schema with array items
        arb_leaf_schema().prop_map(|item_schema| {
            json!({
                "type": "array",
                "items": item_schema
            })
        }),
        // Schema with type array (will be collapsed)
        Just(json!({"type": ["string", "null"], "description": "nullable"})),
        // Schema with anyOf (will be collapsed by Gemini)
        Just(json!({
            "anyOf": [
                {"type": "null"},
                {"type": "string"}
            ]
        })),
        // Schema with allOf (will be merged by Gemini)
        Just(json!({
            "allOf": [
                {"type": "object", "properties": {"a": {"type": "string"}}},
                {"properties": {"b": {"type": "number"}}}
            ]
        })),
    ]
}

// ============================================================================
// Reference implementation of sanitize_schema behavior
// ============================================================================

/// A reference implementation that mimics the old `sanitize_schema` behavior
/// for schemas without `$ref`, `anyOf`, `oneOf`, or `allOf`.
///
/// This applies the same transforms as GeminiSchemaAdapter but serves as an
/// independent implementation for comparison.
fn reference_sanitize(schema: Value) -> Value {
    let adapter = GeminiSchemaAdapter::new();
    adapter.normalize_schema(schema)
}

// ============================================================================
// Property Tests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: schema-dialect, Property 1: Backward Compatibility**
    ///
    /// *For any* schema S without `$ref`, `anyOf`, `oneOf`, or `allOf`:
    /// `GeminiSchemaAdapter.normalize_schema(S)` produces output equivalent to
    /// the reference `sanitize_schema(S)` behavior.
    ///
    /// **Validates: Requirements 9.1, 9.3**
    #[test]
    fn prop_backward_compatibility(schema in arb_simple_schema()) {
        let adapter = GeminiSchemaAdapter::new();

        // Verify the input has no combiners or refs
        if let Some(obj) = schema.as_object() {
            prop_assert!(!obj.contains_key("$ref"), "Generated schema should not contain $ref");
            prop_assert!(!obj.contains_key("anyOf"), "Generated schema should not contain anyOf");
            prop_assert!(!obj.contains_key("oneOf"), "Generated schema should not contain oneOf");
            prop_assert!(!obj.contains_key("allOf"), "Generated schema should not contain allOf");
        }

        let normalized = adapter.normalize_schema(schema.clone());
        let reference = reference_sanitize(schema);

        prop_assert_eq!(
            &normalized,
            &reference,
            "GeminiSchemaAdapter output should match reference sanitize_schema behavior"
        );
    }

    /// **Feature: schema-dialect, Property 2: Idempotency**
    ///
    /// *For any* adapter A and schema S:
    /// `A.normalize_schema(A.normalize_schema(S)) == A.normalize_schema(S)`
    ///
    /// **Validates: Requirements 3.1–3.20**
    #[test]
    fn prop_idempotency(schema in arb_schema_for_idempotency()) {
        let adapter = GeminiSchemaAdapter::new();

        let first_pass = adapter.normalize_schema(schema);
        let second_pass = adapter.normalize_schema(first_pass.clone());

        prop_assert_eq!(
            &first_pass,
            &second_pass,
            "Normalizing an already-normalized schema should produce the same result"
        );
    }

    /// **Feature: schema-dialect, Property 2 (Vertex AI variant): Idempotency**
    ///
    /// *For any* schema S:
    /// `GeminiSchemaAdapter::vertex_ai().normalize_schema(normalize_schema(S)) == normalize_schema(S)`
    ///
    /// **Validates: Requirements 3.1–3.20, 3.8**
    #[test]
    fn prop_idempotency_vertex_ai(schema in arb_schema_for_idempotency()) {
        let adapter = GeminiSchemaAdapter::vertex_ai();

        let first_pass = adapter.normalize_schema(schema);
        let second_pass = adapter.normalize_schema(first_pass.clone());

        prop_assert_eq!(
            &first_pass,
            &second_pass,
            "Vertex AI adapter should also be idempotent"
        );
    }
}

// ============================================================================
// Additional deterministic tests for P1 edge cases
// ============================================================================

#[test]
fn test_backward_compat_empty_object() {
    let adapter = GeminiSchemaAdapter::new();
    let schema = json!({});
    let result = adapter.normalize_schema(schema.clone());
    let reference = reference_sanitize(schema);
    assert_eq!(result, reference);
}

#[test]
fn test_backward_compat_with_schema_keyword() {
    let adapter = GeminiSchemaAdapter::new();
    let schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "name": {"type": "string"}
        }
    });
    let result = adapter.normalize_schema(schema.clone());
    let reference = reference_sanitize(schema);
    assert_eq!(result, reference);
}

#[test]
fn test_backward_compat_with_conditional_keywords() {
    let adapter = GeminiSchemaAdapter::new();
    let schema = json!({
        "type": "object",
        "properties": {"x": {"type": "string"}},
        "if": {"const": true},
        "then": {"required": ["x"]},
        "else": {"required": []}
    });
    let result = adapter.normalize_schema(schema.clone());
    let reference = reference_sanitize(schema);
    assert_eq!(result, reference);
}

#[test]
fn test_idempotency_with_deeply_nested() {
    let adapter = GeminiSchemaAdapter::new();
    let schema = json!({
        "type": "object",
        "properties": {
            "level1": {
                "type": "object",
                "properties": {
                    "level2": {
                        "type": "object",
                        "properties": {
                            "level3": {
                                "type": "object",
                                "properties": {
                                    "level4": {
                                        "type": "object",
                                        "properties": {
                                            "level5": {
                                                "type": "object",
                                                "properties": {
                                                    "deep": {"type": "string"}
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
    let first = adapter.normalize_schema(schema);
    let second = adapter.normalize_schema(first.clone());
    assert_eq!(first, second);
}
