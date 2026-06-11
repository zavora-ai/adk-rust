//! Property-based tests for the OpenAiStrictSchemaAdapter.
//!
//! Tests correctness property P6 from the design document:
//!
//! - **P6: OpenAI Strict additionalProperties** — For any schema S normalized by
//!   `OpenAiStrictSchemaAdapter`: every object schema node in the output has
//!   `"additionalProperties": false`.

#![cfg(feature = "openai")]

use adk_core::SchemaAdapter;
use adk_model::openai::OpenAiStrictSchemaAdapter;
use proptest::prelude::*;
use serde_json::{Map, Value, json};

// ============================================================================
// Generators
// ============================================================================

/// Generates a property name.
fn arb_property_name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,12}".prop_map(|s| s)
}

/// Generates a leaf schema (no nested objects).
fn arb_leaf_schema() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(json!({"type": "string"})),
        Just(json!({"type": "number"})),
        Just(json!({"type": "integer"})),
        Just(json!({"type": "boolean"})),
        Just(json!({"type": "string", "format": "date-time"})),
        Just(json!({"type": "string", "enum": ["a", "b"]})),
        Just(json!({"type": ["string", "null"]})),
    ]
}

/// Generates a properties map with leaf schemas.
fn arb_leaf_properties() -> impl Strategy<Value = Map<String, Value>> {
    prop::collection::hash_map(arb_property_name(), arb_leaf_schema(), 1..4).prop_map(|hm| {
        let mut map = Map::new();
        for (k, v) in hm {
            map.insert(k, v);
        }
        map
    })
}

/// Generates an object schema with nested objects (up to 2 levels deep).
fn arb_object_schema() -> impl Strategy<Value = Value> {
    (arb_leaf_properties(), prop::option::of(arb_leaf_properties())).prop_map(
        |(props, nested_props_opt)| {
            let mut properties = props;

            // Optionally add a nested object property
            if let Some(nested_props) = nested_props_opt {
                let nested_obj = json!({
                    "type": "object",
                    "properties": Value::Object(nested_props)
                });
                properties.insert("nested_obj".to_string(), nested_obj);
            }

            json!({
                "type": "object",
                "properties": Value::Object(properties)
            })
        },
    )
}

/// Generates schemas that exercise various structural patterns.
fn arb_schema_for_strict() -> impl Strategy<Value = Value> {
    prop_oneof![
        // Simple object schema
        arb_object_schema(),
        // Object with $defs containing object schemas
        arb_leaf_properties().prop_map(|props| {
            json!({
                "type": "object",
                "properties": {
                    "item": {"$ref": "#/$defs/Item"}
                },
                "$defs": {
                    "Item": {
                        "type": "object",
                        "properties": Value::Object(props)
                    }
                }
            })
        }),
        // Object with anyOf containing object sub-schemas
        arb_leaf_properties().prop_map(|props| {
            json!({
                "type": "object",
                "properties": {
                    "value": {
                        "anyOf": [
                            {
                                "type": "object",
                                "properties": Value::Object(props)
                            },
                            {"type": "null"}
                        ]
                    }
                }
            })
        }),
        // Object with oneOf containing object sub-schemas
        arb_leaf_properties().prop_map(|props| {
            json!({
                "type": "object",
                "properties": {
                    "payload": {
                        "oneOf": [
                            {
                                "type": "object",
                                "properties": Value::Object(props)
                            },
                            {"type": "string"}
                        ]
                    }
                }
            })
        }),
        // Object with array items that are objects
        arb_leaf_properties().prop_map(|props| {
            json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": Value::Object(props)
                        }
                    }
                }
            })
        }),
        // Schema with properties but no explicit type (implicit object)
        arb_leaf_properties().prop_map(|props| {
            json!({
                "properties": Value::Object(props)
            })
        }),
        // Object with allOf containing object sub-schemas
        (arb_leaf_properties(), arb_leaf_properties()).prop_map(|(props1, props2)| {
            json!({
                "type": "object",
                "properties": {
                    "merged": {
                        "allOf": [
                            {
                                "type": "object",
                                "properties": Value::Object(props1)
                            },
                            {
                                "type": "object",
                                "properties": Value::Object(props2)
                            }
                        ]
                    }
                }
            })
        }),
    ]
}

// ============================================================================
// Verification helpers
// ============================================================================

/// Recursively checks that every object schema node in the given value has
/// `"additionalProperties": false`.
///
/// An "object schema" is identified by having `"type": "object"` or having
/// a `"properties"` key.
///
/// Returns a list of paths where the property is missing or not `false`.
fn find_missing_additional_properties(schema: &Value, path: &str) -> Vec<String> {
    let mut violations = Vec::new();

    let Some(obj) = schema.as_object() else {
        return violations;
    };

    let is_object_schema = obj.get("type").and_then(|t| t.as_str()).is_some_and(|t| t == "object")
        || obj.contains_key("properties");

    if is_object_schema {
        match obj.get("additionalProperties") {
            Some(Value::Bool(false)) => {} // correct
            other => {
                violations
                    .push(format!("{path}: expected additionalProperties=false, got {:?}", other));
            }
        }
    }

    // Recurse into properties
    if let Some(props) = obj.get("properties")
        && let Some(props_obj) = props.as_object()
    {
        for (key, value) in props_obj {
            let child_path = format!("{path}.properties.{key}");
            violations.extend(find_missing_additional_properties(value, &child_path));
        }
    }

    // Recurse into items
    if let Some(items) = obj.get("items") {
        if items.is_object() {
            violations.extend(find_missing_additional_properties(items, &format!("{path}.items")));
        } else if let Some(arr) = items.as_array() {
            for (i, item) in arr.iter().enumerate() {
                violations.extend(find_missing_additional_properties(
                    item,
                    &format!("{path}.items[{i}]"),
                ));
            }
        }
    }

    // Recurse into allOf, anyOf, oneOf
    for keyword in &["allOf", "anyOf", "oneOf"] {
        if let Some(arr_val) = obj.get(*keyword)
            && let Some(arr) = arr_val.as_array()
        {
            for (i, sub) in arr.iter().enumerate() {
                violations.extend(find_missing_additional_properties(
                    sub,
                    &format!("{path}.{keyword}[{i}]"),
                ));
            }
        }
    }

    // Recurse into $defs
    if let Some(defs) = obj.get("$defs")
        && let Some(defs_obj) = defs.as_object()
    {
        for (key, value) in defs_obj {
            violations
                .extend(find_missing_additional_properties(value, &format!("{path}.$defs.{key}")));
        }
    }

    // Recurse into definitions
    if let Some(defs) = obj.get("definitions")
        && let Some(defs_obj) = defs.as_object()
    {
        for (key, value) in defs_obj {
            violations.extend(find_missing_additional_properties(
                value,
                &format!("{path}.definitions.{key}"),
            ));
        }
    }

    // Recurse into not
    if let Some(not_schema) = obj.get("not")
        && not_schema.is_object()
    {
        violations.extend(find_missing_additional_properties(not_schema, &format!("{path}.not")));
    }

    // Recurse into patternProperties
    if let Some(pattern_props) = obj.get("patternProperties")
        && let Some(pp_obj) = pattern_props.as_object()
    {
        for (key, value) in pp_obj {
            violations.extend(find_missing_additional_properties(
                value,
                &format!("{path}.patternProperties.{key}"),
            ));
        }
    }

    // Recurse into prefixItems
    if let Some(prefix_items) = obj.get("prefixItems")
        && let Some(arr) = prefix_items.as_array()
    {
        for (i, item) in arr.iter().enumerate() {
            violations.extend(find_missing_additional_properties(
                item,
                &format!("{path}.prefixItems[{i}]"),
            ));
        }
    }

    violations
}

// ============================================================================
// Property Tests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: schema-dialect, Property 6: OpenAI Strict additionalProperties**
    ///
    /// *For any* schema S normalized by `OpenAiStrictSchemaAdapter`:
    /// every object schema node in the output has `"additionalProperties": false`.
    ///
    /// **Validates: Requirements 4.3, 4.4**
    #[test]
    fn prop_openai_strict_additional_properties(schema in arb_schema_for_strict()) {
        let adapter = OpenAiStrictSchemaAdapter;
        let normalized = adapter.normalize_schema(schema);

        let violations = find_missing_additional_properties(&normalized, "root");

        prop_assert!(
            violations.is_empty(),
            "All object schema nodes must have additionalProperties: false.\nViolations:\n{}",
            violations.join("\n")
        );
    }
}

// ============================================================================
// Additional deterministic tests for P6 edge cases
// ============================================================================

#[test]
fn test_strict_additional_properties_simple_object() {
    let adapter = OpenAiStrictSchemaAdapter;
    let schema = json!({
        "type": "object",
        "properties": {
            "name": {"type": "string"}
        }
    });
    let result = adapter.normalize_schema(schema);
    let violations = find_missing_additional_properties(&result, "root");
    assert!(violations.is_empty(), "Violations: {violations:?}");
}

#[test]
fn test_strict_additional_properties_deeply_nested() {
    let adapter = OpenAiStrictSchemaAdapter;
    let schema = json!({
        "type": "object",
        "properties": {
            "level1": {
                "type": "object",
                "properties": {
                    "level2": {
                        "type": "object",
                        "properties": {
                            "value": {"type": "string"}
                        }
                    }
                }
            }
        }
    });
    let result = adapter.normalize_schema(schema);
    let violations = find_missing_additional_properties(&result, "root");
    assert!(violations.is_empty(), "Violations: {violations:?}");
}

#[test]
fn test_strict_additional_properties_in_defs() {
    let adapter = OpenAiStrictSchemaAdapter;
    let schema = json!({
        "type": "object",
        "properties": {
            "item": {"$ref": "#/$defs/Item"}
        },
        "$defs": {
            "Item": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"}
                }
            }
        }
    });
    let result = adapter.normalize_schema(schema);
    let violations = find_missing_additional_properties(&result, "root");
    assert!(violations.is_empty(), "Violations: {violations:?}");
}

#[test]
fn test_strict_additional_properties_in_any_of() {
    let adapter = OpenAiStrictSchemaAdapter;
    let schema = json!({
        "type": "object",
        "properties": {
            "value": {
                "anyOf": [
                    {
                        "type": "object",
                        "properties": {"a": {"type": "string"}}
                    },
                    {"type": "null"}
                ]
            }
        }
    });
    let result = adapter.normalize_schema(schema);
    let violations = find_missing_additional_properties(&result, "root");
    assert!(violations.is_empty(), "Violations: {violations:?}");
}

#[test]
fn test_strict_additional_properties_in_array_items() {
    let adapter = OpenAiStrictSchemaAdapter;
    let schema = json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "integer"}
                    }
                }
            }
        }
    });
    let result = adapter.normalize_schema(schema);
    let violations = find_missing_additional_properties(&result, "root");
    assert!(violations.is_empty(), "Violations: {violations:?}");
}

#[test]
fn test_strict_additional_properties_implicit_object() {
    let adapter = OpenAiStrictSchemaAdapter;
    // Schema with properties but no explicit type — should get type: object added
    // and then additionalProperties: false
    let schema = json!({
        "properties": {
            "name": {"type": "string"}
        }
    });
    let result = adapter.normalize_schema(schema);
    let violations = find_missing_additional_properties(&result, "root");
    assert!(violations.is_empty(), "Violations: {violations:?}");
    assert_eq!(result["type"], "object");
}
