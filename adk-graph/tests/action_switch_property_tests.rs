#![cfg(feature = "action")]
//! Property tests for switch condition evaluation determinism.
//!
//! - **Property 5: Switch Condition Determinism** — Same state always produces same routing decision
//! - Tests all 12 operators with various input types (string, number, array, null)

use std::collections::HashMap;

use adk_action::{EvaluationMode, ExpressionMode, SwitchCondition};
use adk_graph::action::switch::evaluate_switch_conditions;
use proptest::prelude::*;
use serde_json::{Value, json};

// ── Helpers ───────────────────────────────────────────────────────────

fn make_condition(
    id: &str,
    field: &str,
    operator: &str,
    value: &str,
    port: &str,
) -> SwitchCondition {
    SwitchCondition {
        id: id.to_string(),
        name: id.to_string(),
        expression: ExpressionMode {
            field: field.to_string(),
            operator: operator.to_string(),
            value: value.to_string(),
        },
        output_port: port.to_string(),
    }
}

/// Helper to unwrap and compare switch results (ActionError doesn't impl PartialEq).
fn assert_switch_ok(result: Result<Vec<String>, adk_action::ActionError>, expected: Vec<&str>) {
    let ports = result.expect("expected Ok result");
    let expected: Vec<String> = expected.into_iter().map(String::from).collect();
    assert_eq!(ports, expected);
}

// ── Property 5: Switch Condition Evaluation Determinism ───────────────
//
// **Feature: action-node-graph-standardization, Property 5: Switch Condition Determinism**
// *For any* SwitchNodeConfig with FirstMatch evaluation mode and a fixed state,
// evaluating the conditions SHALL always select the same output port.
// **Validates: Requirements 6.2, 6.3**

fn arb_state_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(json!(null)),
        Just(json!("")),
        Just(json!("hello")),
        Just(json!("world")),
        Just(json!(0)),
        Just(json!(42)),
        Just(json!(100)),
        Just(json!(-1)),
        Just(json!(3.14)),
        Just(json!(true)),
        Just(json!(false)),
        Just(json!([])),
        Just(json!(["a", "b", "c"])),
        Just(json!({"key": "value"})),
    ]
}

fn arb_operator() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("eq".to_string()),
        Just("neq".to_string()),
        Just("gt".to_string()),
        Just("lt".to_string()),
        Just("gte".to_string()),
        Just("lte".to_string()),
        Just("contains".to_string()),
        Just("startsWith".to_string()),
        Just("endsWith".to_string()),
        Just("matches".to_string()),
        Just("in".to_string()),
        Just("empty".to_string()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// For any state value and operator, evaluating the same conditions twice
    /// with the same state produces the same result.
    #[test]
    fn prop_switch_determinism(
        state_val in arb_state_value(),
        operator in arb_operator(),
    ) {
        let mut state = HashMap::new();
        state.insert("field".to_string(), state_val);

        let conditions = vec![
            make_condition("c1", "field", &operator, "hello", "port_a"),
            make_condition("c2", "field", &operator, "42", "port_b"),
        ];

        let result1 = evaluate_switch_conditions(
            &conditions, &state, &EvaluationMode::FirstMatch, Some("default"),
        );
        let result2 = evaluate_switch_conditions(
            &conditions, &state, &EvaluationMode::FirstMatch, Some("default"),
        );

        // Both should succeed (we have a default branch)
        let ports1 = result1.expect("first eval should succeed");
        let ports2 = result2.expect("second eval should succeed");
        prop_assert_eq!(&ports1, &ports2, "same state must produce same routing");
    }

    /// For any state value, AllMatch mode also produces deterministic results.
    #[test]
    fn prop_switch_all_match_determinism(
        state_val in arb_state_value(),
        operator in arb_operator(),
    ) {
        let mut state = HashMap::new();
        state.insert("field".to_string(), state_val);

        let conditions = vec![
            make_condition("c1", "field", &operator, "hello", "port_a"),
            make_condition("c2", "field", &operator, "42", "port_b"),
        ];

        let result1 = evaluate_switch_conditions(
            &conditions, &state, &EvaluationMode::AllMatch, Some("default"),
        );
        let result2 = evaluate_switch_conditions(
            &conditions, &state, &EvaluationMode::AllMatch, Some("default"),
        );

        let ports1 = result1.expect("first eval should succeed");
        let ports2 = result2.expect("second eval should succeed");
        prop_assert_eq!(&ports1, &ports2, "AllMatch must be deterministic");
    }
}

// ── Operator-specific tests ───────────────────────────────────────────

#[test]
fn test_eq_operator_string() {
    let mut state = HashMap::new();
    state.insert("status".to_string(), json!("active"));
    let conditions = vec![
        make_condition("c1", "status", "eq", "active", "active_port"),
        make_condition("c2", "status", "eq", "inactive", "inactive_port"),
    ];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["active_port"],
    );
}

#[test]
fn test_eq_operator_number() {
    let mut state = HashMap::new();
    state.insert("count".to_string(), json!(42));
    let conditions = vec![
        make_condition("c1", "count", "eq", "42", "match_port"),
        make_condition("c2", "count", "eq", "0", "zero_port"),
    ];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["match_port"],
    );
}

#[test]
fn test_neq_operator() {
    let mut state = HashMap::new();
    state.insert("status".to_string(), json!("active"));
    let conditions = vec![make_condition("c1", "status", "neq", "inactive", "not_inactive")];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["not_inactive"],
    );
}

#[test]
fn test_gt_operator() {
    let mut state = HashMap::new();
    state.insert("score".to_string(), json!(85));
    let conditions = vec![
        make_condition("c1", "score", "gt", "90", "excellent"),
        make_condition("c2", "score", "gt", "80", "good"),
        make_condition("c3", "score", "gt", "70", "average"),
    ];
    assert_switch_ok(
        evaluate_switch_conditions(
            &conditions,
            &state,
            &EvaluationMode::FirstMatch,
            Some("default"),
        ),
        vec!["good"],
    );
}

#[test]
fn test_lt_operator() {
    let mut state = HashMap::new();
    state.insert("temp".to_string(), json!(15));
    let conditions = vec![
        make_condition("c1", "temp", "lt", "0", "freezing"),
        make_condition("c2", "temp", "lt", "20", "cold"),
    ];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["cold"],
    );
}

#[test]
fn test_gte_operator() {
    let mut state = HashMap::new();
    state.insert("age".to_string(), json!(18));
    let conditions = vec![make_condition("c1", "age", "gte", "18", "adult")];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["adult"],
    );
}

#[test]
fn test_lte_operator() {
    let mut state = HashMap::new();
    state.insert("priority".to_string(), json!(3));
    let conditions = vec![make_condition("c1", "priority", "lte", "3", "low_priority")];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["low_priority"],
    );
}

#[test]
fn test_contains_operator_string() {
    let mut state = HashMap::new();
    state.insert("message".to_string(), json!("hello world"));
    let conditions = vec![
        make_condition("c1", "message", "contains", "world", "has_world"),
        make_condition("c2", "message", "contains", "foo", "has_foo"),
    ];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["has_world"],
    );
}

#[test]
fn test_contains_operator_array() {
    let mut state = HashMap::new();
    state.insert("tags".to_string(), json!(["rust", "ai", "graph"]));
    let conditions = vec![make_condition("c1", "tags", "contains", "ai", "has_ai")];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["has_ai"],
    );
}

#[test]
fn test_starts_with_operator() {
    let mut state = HashMap::new();
    state.insert("url".to_string(), json!("https://example.com"));
    let conditions = vec![
        make_condition("c1", "url", "startsWith", "https://", "secure"),
        make_condition("c2", "url", "startsWith", "http://", "insecure"),
    ];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["secure"],
    );
}

#[test]
fn test_ends_with_operator() {
    let mut state = HashMap::new();
    state.insert("filename".to_string(), json!("report.pdf"));
    let conditions = vec![
        make_condition("c1", "filename", "endsWith", ".pdf", "pdf_file"),
        make_condition("c2", "filename", "endsWith", ".csv", "csv_file"),
    ];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["pdf_file"],
    );
}

#[test]
fn test_matches_operator() {
    let mut state = HashMap::new();
    state.insert("email".to_string(), json!("user@example.com"));
    let conditions =
        vec![make_condition("c1", "email", "matches", r"^[\w.]+@[\w.]+\.\w+$", "valid_email")];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["valid_email"],
    );
}

#[test]
fn test_in_operator() {
    let mut state = HashMap::new();
    state.insert("role".to_string(), json!("admin"));
    let conditions =
        vec![make_condition("c1", "role", "in", "admin,moderator,owner", "privileged")];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["privileged"],
    );
}

#[test]
fn test_empty_operator_null() {
    let mut state = HashMap::new();
    state.insert("data".to_string(), json!(null));
    let conditions = vec![make_condition("c1", "data", "empty", "", "is_empty")];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["is_empty"],
    );
}

#[test]
fn test_empty_operator_empty_string() {
    let mut state = HashMap::new();
    state.insert("name".to_string(), json!(""));
    let conditions = vec![make_condition("c1", "name", "empty", "", "is_empty")];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["is_empty"],
    );
}

#[test]
fn test_empty_operator_empty_array() {
    let mut state = HashMap::new();
    state.insert("items".to_string(), json!([]));
    let conditions = vec![make_condition("c1", "items", "empty", "", "no_items")];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["no_items"],
    );
}

#[test]
fn test_exists_operator() {
    let mut state = HashMap::new();
    state.insert("token".to_string(), json!("abc123"));
    let conditions = vec![
        make_condition("c1", "token", "exists", "", "has_token"),
        make_condition("c2", "missing_field", "exists", "", "has_missing"),
    ];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None),
        vec!["has_token"],
    );
}

#[test]
fn test_no_match_with_default() {
    let mut state = HashMap::new();
    state.insert("status".to_string(), json!("unknown"));
    let conditions = vec![
        make_condition("c1", "status", "eq", "active", "active_port"),
        make_condition("c2", "status", "eq", "inactive", "inactive_port"),
    ];
    assert_switch_ok(
        evaluate_switch_conditions(
            &conditions,
            &state,
            &EvaluationMode::FirstMatch,
            Some("fallback"),
        ),
        vec!["fallback"],
    );
}

#[test]
fn test_no_match_without_default_errors() {
    let mut state = HashMap::new();
    state.insert("status".to_string(), json!("unknown"));
    let conditions = vec![make_condition("c1", "status", "eq", "active", "active_port")];
    let result = evaluate_switch_conditions(&conditions, &state, &EvaluationMode::FirstMatch, None);
    assert!(result.is_err(), "should error when no match and no default");
}

#[test]
fn test_all_match_mode_returns_all_matching() {
    let mut state = HashMap::new();
    state.insert("score".to_string(), json!(95));
    let conditions = vec![
        make_condition("c1", "score", "gt", "90", "excellent"),
        make_condition("c2", "score", "gt", "80", "good"),
        make_condition("c3", "score", "gt", "70", "average"),
        make_condition("c4", "score", "gt", "99", "perfect"),
    ];
    assert_switch_ok(
        evaluate_switch_conditions(&conditions, &state, &EvaluationMode::AllMatch, None),
        vec!["excellent", "good", "average"],
    );
}
