//! Property-based tests for schema utilities in adk-core.
//!
//! - P5: Name Truncation Safety (Task 1.3)
//! - P4: Ref Resolution Termination (Task 2.3)

use std::borrow::Cow;

use proptest::prelude::*;
use serde_json::{Map, Value, json};

use adk_core::schema_utils::{resolve_refs, truncate_tool_name};

// ============================================================================
// P5: Name Truncation Safety
// **Validates: Requirements 14.4, 14.5**
//
// For any UTF-8 string, `truncate_tool_name(name, 64)` produces a valid UTF-8
// string of at most 64 bytes.
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: schema-dialect, Property 5: Name Truncation Safety**
    /// *For any* UTF-8 string, `truncate_tool_name(name, 64)` produces a valid
    /// UTF-8 string of at most 64 bytes.
    /// **Validates: Requirements 14.4, 14.5**
    #[test]
    fn prop_truncate_tool_name_produces_valid_utf8_within_limit(name in "\\PC{0,200}") {
        let result = truncate_tool_name(&name, 64);
        // Result must be at most 64 bytes
        prop_assert!(
            result.len() <= 64,
            "Result length {} exceeds 64 bytes for input {:?}",
            result.len(),
            name
        );
        // Result must be valid UTF-8 (guaranteed by Cow<str> but verify bytes)
        prop_assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "Result is not valid UTF-8 for input {:?}",
            name
        );
    }

    /// **Feature: schema-dialect, Property 5: Name Truncation Safety (prefix preservation)**
    /// *For any* UTF-8 string that is longer than 64 bytes, the truncated result
    /// is a prefix of the original string.
    /// **Validates: Requirements 14.4, 14.5**
    #[test]
    fn prop_truncate_tool_name_preserves_prefix(name in "\\PC{0,200}") {
        let result = truncate_tool_name(&name, 64);
        // The result must be a prefix of the original
        prop_assert!(
            name.starts_with(result.as_ref()),
            "Result {:?} is not a prefix of input {:?}",
            result,
            name
        );
    }

    /// **Feature: schema-dialect, Property 5: Name Truncation Safety (short strings unchanged)**
    /// *For any* UTF-8 string of at most 64 bytes, `truncate_tool_name` returns
    /// the original string unchanged (borrowed).
    /// **Validates: Requirements 14.4, 14.5**
    #[test]
    fn prop_truncate_tool_name_short_strings_unchanged(name in "[a-z]{0,64}") {
        let result = truncate_tool_name(&name, 64);
        prop_assert_eq!(result.as_ref(), name.as_str());
        // Should be borrowed (no allocation) for short ASCII strings
        prop_assert!(matches!(result, Cow::Borrowed(_)));
    }

    /// **Feature: schema-dialect, Property 5: Name Truncation Safety (multi-byte boundary)**
    /// *For any* string with multi-byte characters, truncation never splits a
    /// character (result is always valid UTF-8 at a char boundary).
    /// **Validates: Requirements 14.4, 14.5**
    #[test]
    fn prop_truncate_tool_name_multibyte_boundary(
        // Generate strings with emoji and CJK characters that are multi-byte
        name in prop::string::string_regex("[\\x{1F600}-\\x{1F64F}\\x{4E00}-\\x{9FFF}a-z]{1,50}").unwrap()
    ) {
        let result = truncate_tool_name(&name, 64);
        // Must be valid UTF-8
        prop_assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        // Must be at most 64 bytes
        prop_assert!(result.len() <= 64);
        // Must be a char boundary in the original string
        if result.len() < name.len() {
            prop_assert!(name.is_char_boundary(result.len()));
        }
    }
}

// ============================================================================
// P4: Ref Resolution Termination
// **Validates: Requirements 3.3, 7.4**
//
// For any schema with circular `$ref` chains, `resolve_refs` terminates within
// 10 recursive calls. The depth counter tracks $ref resolution depth — when
// depth > 10, the function stops resolving further refs and returns.
// ============================================================================

/// Strategy to generate a definition name.
fn arb_def_name() -> impl Strategy<Value = String> {
    "[A-Z][a-zA-Z]{2,10}"
}

/// Strategy to generate a schema with a self-referencing definition.
/// The definition references itself via `$ref`.
fn arb_self_referencing_schema() -> impl Strategy<Value = (Value, Map<String, Value>)> {
    arb_def_name().prop_map(|def_name| {
        // Definition that references itself
        let def_schema = json!({
            "type": "object",
            "properties": {
                "child": { "$ref": format!("#/definitions/{def_name}") }
            }
        });

        let mut definitions = Map::new();
        definitions.insert(def_name.clone(), def_schema);

        // Root schema references the self-referencing definition
        let schema = json!({
            "type": "object",
            "properties": {
                "root": { "$ref": format!("#/definitions/{def_name}") }
            }
        });

        (schema, definitions)
    })
}

/// Strategy to generate a schema with mutually-referencing definitions.
/// Definition A references B, and B references A.
fn arb_mutual_referencing_schema() -> impl Strategy<Value = (Value, Map<String, Value>)> {
    (arb_def_name(), arb_def_name()).prop_filter("names must differ", |(a, b)| a != b).prop_map(
        |(name_a, name_b)| {
            // A references B
            let def_a = json!({
                "type": "object",
                "properties": {
                    "link_to_b": { "$ref": format!("#/definitions/{name_b}") }
                }
            });

            // B references A
            let def_b = json!({
                "type": "object",
                "properties": {
                    "link_to_a": { "$ref": format!("#/definitions/{name_a}") }
                }
            });

            let mut definitions = Map::new();
            definitions.insert(name_a.clone(), def_a);
            definitions.insert(name_b.clone(), def_b);

            // Root references A
            let schema = json!({
                "type": "object",
                "properties": {
                    "entry": { "$ref": format!("#/definitions/{name_a}") }
                }
            });

            (schema, definitions)
        },
    )
}

/// Strategy to generate a schema with a chain of references (A -> B -> C -> A).
fn arb_chain_referencing_schema() -> impl Strategy<Value = (Value, Map<String, Value>)> {
    (arb_def_name(), arb_def_name(), arb_def_name())
        .prop_filter("names must all differ", |(a, b, c)| a != b && b != c && a != c)
        .prop_map(|(name_a, name_b, name_c)| {
            // A -> B -> C -> A (circular chain of length 3)
            let def_a = json!({
                "type": "object",
                "properties": {
                    "next": { "$ref": format!("#/definitions/{name_b}") }
                }
            });

            let def_b = json!({
                "type": "object",
                "properties": {
                    "next": { "$ref": format!("#/definitions/{name_c}") }
                }
            });

            let def_c = json!({
                "type": "object",
                "properties": {
                    "next": { "$ref": format!("#/definitions/{name_a}") }
                }
            });

            let mut definitions = Map::new();
            definitions.insert(name_a.clone(), def_a);
            definitions.insert(name_b.clone(), def_b);
            definitions.insert(name_c.clone(), def_c);

            let schema = json!({
                "type": "object",
                "properties": {
                    "start": { "$ref": format!("#/definitions/{name_a}") }
                }
            });

            (schema, definitions)
        })
}

/// Counts the maximum structural depth of the resolved schema.
/// This verifies that the output is bounded (not infinitely deep).
fn max_depth(value: &Value) -> usize {
    match value {
        Value::Object(map) => {
            let child_max = map.values().map(|v| max_depth(v)).max().unwrap_or(0);
            1 + child_max
        }
        Value::Array(arr) => {
            let child_max = arr.iter().map(|v| max_depth(v)).max().unwrap_or(0);
            1 + child_max
        }
        _ => 1,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: schema-dialect, Property 4: Ref Resolution Termination (self-referencing)**
    /// *For any* schema with a self-referencing `$ref` chain, `resolve_refs`
    /// terminates (does not hang or stack overflow) and produces a finite schema.
    /// **Validates: Requirements 3.3, 7.4**
    #[test]
    fn prop_resolve_refs_terminates_self_referencing(
        (mut schema, definitions) in arb_self_referencing_schema()
    ) {
        // This must terminate (not hang/stack overflow)
        resolve_refs(&mut schema, &definitions, 0);

        // Output must be finite — bounded depth proves termination
        let depth = max_depth(&schema);
        prop_assert!(
            depth < 200,
            "Resolved schema has unbounded depth {depth}, indicating non-termination"
        );

        // Output must serialize to valid JSON
        prop_assert!(serde_json::to_string(&schema).is_ok());
    }

    /// **Feature: schema-dialect, Property 4: Ref Resolution Termination (mutual references)**
    /// *For any* schema with mutually-referencing `$ref` chains (A -> B -> A),
    /// `resolve_refs` terminates and produces a finite schema.
    /// **Validates: Requirements 3.3, 7.4**
    #[test]
    fn prop_resolve_refs_terminates_mutual_referencing(
        (mut schema, definitions) in arb_mutual_referencing_schema()
    ) {
        // This must terminate (not hang/stack overflow)
        resolve_refs(&mut schema, &definitions, 0);

        // Output must be finite
        let depth = max_depth(&schema);
        prop_assert!(
            depth < 200,
            "Resolved schema has unbounded depth {depth}, indicating non-termination"
        );

        // Output must serialize to valid JSON
        prop_assert!(serde_json::to_string(&schema).is_ok());
    }

    /// **Feature: schema-dialect, Property 4: Ref Resolution Termination (chain references)**
    /// *For any* schema with a circular chain (A -> B -> C -> A), `resolve_refs`
    /// terminates and produces a finite schema.
    /// **Validates: Requirements 3.3, 7.4**
    #[test]
    fn prop_resolve_refs_terminates_chain_referencing(
        (mut schema, definitions) in arb_chain_referencing_schema()
    ) {
        // This must terminate (not hang/stack overflow)
        resolve_refs(&mut schema, &definitions, 0);

        // Output must be finite
        let depth = max_depth(&schema);
        prop_assert!(
            depth < 200,
            "Resolved schema has unbounded depth {depth}, indicating non-termination"
        );

        // Output must serialize to valid JSON
        prop_assert!(serde_json::to_string(&schema).is_ok());
    }

    /// **Feature: schema-dialect, Property 4: Ref Resolution Termination (output is valid JSON)**
    /// *For any* schema with circular refs, the resolved output is a valid JSON
    /// value (the function never panics).
    /// **Validates: Requirements 3.3, 7.4**
    #[test]
    fn prop_resolve_refs_output_is_valid_json(
        (mut schema, definitions) in prop_oneof![
            arb_self_referencing_schema(),
            arb_mutual_referencing_schema(),
            arb_chain_referencing_schema(),
        ]
    ) {
        resolve_refs(&mut schema, &definitions, 0);

        // Output must serialize to valid JSON
        let serialized = serde_json::to_string(&schema);
        prop_assert!(
            serialized.is_ok(),
            "Resolved schema failed to serialize: {:?}",
            serialized.err()
        );
    }
}
