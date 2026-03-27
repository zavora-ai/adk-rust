//! Property-based tests for variable interpolation utilities.
//!
//! These tests verify the idempotence, correctness, and edge-case behavior
//! of `interpolate_variables()` and `get_nested_value()`.

use adk_action::{get_nested_value, interpolate_variables};
use proptest::prelude::*;
use serde_json::json;
use std::collections::HashMap;

// ── Generators ────────────────────────────────────────────────────────

/// Generates strings that do NOT contain `{{` patterns.
/// Uses a character set that excludes `{` entirely.
fn arb_no_braces_template() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 _.,!?:;/\\-]{0,100}"
}

/// Generates valid variable names (alphanumeric + underscore, starting with a letter).
fn arb_var_name() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_]{0,10}"
}

/// Generates simple string values for state.
fn arb_string_value() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 _]{1,30}"
}

// ── Property 4: Variable Interpolation Idempotence ────────────────────

// **Feature: action-node-graph-standardization, Property 4: Variable Interpolation Idempotence**
// *For any* template string containing no `{{variable}}` patterns,
// `interpolate_variables(template, state)` SHALL return the template unchanged
// regardless of state contents.
// **Validates: Requirements 4.2, 9.2**
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn prop_interpolation_idempotence_no_braces(
        template in arb_no_braces_template(),
        key in arb_var_name(),
        value in arb_string_value(),
    ) {
        let mut state = HashMap::new();
        state.insert(key, json!(value));

        let result = interpolate_variables(&template, &state);
        prop_assert_eq!(&result, &template,
            "template without {{{{}}}} patterns should be unchanged");
    }

    #[test]
    fn prop_interpolation_idempotence_empty_state(
        template in arb_no_braces_template(),
    ) {
        let state = HashMap::new();
        let result = interpolate_variables(&template, &state);
        prop_assert_eq!(&result, &template,
            "template without {{{{}}}} patterns should be unchanged with empty state");
    }
}

// ── Test: existing key produces the value ─────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn prop_interpolation_existing_key(
        key in arb_var_name(),
        value in arb_string_value(),
    ) {
        let mut state = HashMap::new();
        state.insert(key.clone(), json!(value.clone()));

        let template = format!("{{{{{key}}}}}");
        let result = interpolate_variables(&template, &state);
        prop_assert_eq!(&result, &value,
            "{{{{key}}}} with existing key should produce the value");
    }

    #[test]
    fn prop_interpolation_missing_key_produces_empty(
        key in arb_var_name(),
    ) {
        let state: HashMap<String, serde_json::Value> = HashMap::new();

        let template = format!("{{{{{key}}}}}");
        let result = interpolate_variables(&template, &state);
        prop_assert_eq!(&result, "",
            "{{{{key}}}} with missing key should produce empty string");
    }

    #[test]
    fn prop_interpolation_preserves_surrounding_text(
        prefix in "[a-zA-Z ]{0,20}",
        key in arb_var_name(),
        value in arb_string_value(),
        suffix in "[a-zA-Z ]{0,20}",
    ) {
        let mut state = HashMap::new();
        state.insert(key.clone(), json!(value.clone()));

        let template = format!("{prefix}{{{{{key}}}}}{suffix}");
        let expected = format!("{prefix}{value}{suffix}");
        let result = interpolate_variables(&template, &state);
        prop_assert_eq!(&result, &expected);
    }
}

// ── Test: nested dot-notation resolution ──────────────────────────────

#[test]
fn test_nested_dot_notation_resolves() {
    let mut state = HashMap::new();
    state.insert(
        "a".to_string(),
        json!({
            "b": {
                "c": "deep_value"
            }
        }),
    );

    let result = interpolate_variables("{{a.b.c}}", &state);
    assert_eq!(result, "deep_value");
}

#[test]
fn test_nested_dot_notation_missing_intermediate() {
    let mut state = HashMap::new();
    state.insert("a".to_string(), json!({"b": "leaf"}));

    // a.b.c doesn't exist because a.b is a string, not an object
    let result = interpolate_variables("{{a.b.c}}", &state);
    assert_eq!(result, "");
}

#[test]
fn test_nested_dot_notation_numeric_value() {
    let mut state = HashMap::new();
    state.insert("config".to_string(), json!({"port": 8080}));

    let result = interpolate_variables("Port: {{config.port}}", &state);
    assert_eq!(result, "Port: 8080");
}

#[test]
fn test_nested_dot_notation_null_value() {
    let mut state = HashMap::new();
    state.insert("data".to_string(), json!({"value": null}));

    let result = interpolate_variables("{{data.value}}", &state);
    assert_eq!(result, "");
}

#[test]
fn test_nested_dot_notation_boolean_value() {
    let mut state = HashMap::new();
    state.insert("flags".to_string(), json!({"enabled": true}));

    let result = interpolate_variables("{{flags.enabled}}", &state);
    assert_eq!(result, "true");
}

#[test]
fn test_deeply_nested_four_levels() {
    let mut state = HashMap::new();
    state.insert("a".to_string(), json!({"b": {"c": {"d": "found"}}}));

    assert_eq!(get_nested_value(&state, "a.b.c.d"), Some(&json!("found")));
    let result = interpolate_variables("{{a.b.c.d}}", &state);
    assert_eq!(result, "found");
}

// ── Test: multiple variables in one template ──────────────────────────

#[test]
fn test_multiple_variables_in_template() {
    let mut state = HashMap::new();
    state.insert("first".to_string(), json!("Alice"));
    state.insert("last".to_string(), json!("Smith"));

    let result = interpolate_variables("Hello {{first}} {{last}}!", &state);
    assert_eq!(result, "Hello Alice Smith!");
}

#[test]
fn test_mixed_existing_and_missing_variables() {
    let mut state = HashMap::new();
    state.insert("name".to_string(), json!("Bob"));

    let result = interpolate_variables("{{name}} ({{role}})", &state);
    assert_eq!(result, "Bob ()");
}

// ── Test: get_nested_value edge cases ─────────────────────────────────

#[test]
fn test_get_nested_value_root_key() {
    let mut state = HashMap::new();
    state.insert("key".to_string(), json!("value"));

    assert_eq!(get_nested_value(&state, "key"), Some(&json!("value")));
}

#[test]
fn test_get_nested_value_missing_root() {
    let state: HashMap<String, serde_json::Value> = HashMap::new();
    assert_eq!(get_nested_value(&state, "missing"), None);
}

#[test]
fn test_get_nested_value_non_object_intermediate() {
    let mut state = HashMap::new();
    state.insert("x".to_string(), json!(42));

    assert_eq!(get_nested_value(&state, "x.y"), None);
}

#[test]
fn test_get_nested_value_array_intermediate() {
    let mut state = HashMap::new();
    state.insert("arr".to_string(), json!([1, 2, 3]));

    // Arrays are not objects, so dot-notation traversal returns None
    assert_eq!(get_nested_value(&state, "arr.0"), None);
}
