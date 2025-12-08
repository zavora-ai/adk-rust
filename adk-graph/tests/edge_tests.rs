//! Edge and routing tests

use adk_graph::edge::{EdgeTarget, Router, END, START};
use adk_graph::state::State;
use serde_json::json;

#[test]
fn test_edge_target_from_str() {
    let end: EdgeTarget = END.into();
    assert_eq!(end, EdgeTarget::End);

    let node: EdgeTarget = "my_node".into();
    assert_eq!(node, EdgeTarget::Node("my_node".to_string()));
}

#[test]
fn test_start_end_constants() {
    assert_eq!(START, "__start__");
    assert_eq!(END, "__end__");
}

#[test]
fn test_by_field_router() {
    let router = Router::by_field("action");

    let mut state = State::new();
    state.insert("action".to_string(), json!("process"));

    let result = router(&state);
    assert_eq!(result, "process");

    state.insert("action".to_string(), json!(123));
    let result2 = router(&state);
    assert_eq!(result2, END);
}

#[test]
fn test_by_bool_router() {
    let router = Router::by_bool("is_valid", "success", "failure");

    let mut state = State::new();
    state.insert("is_valid".to_string(), json!(true));
    assert_eq!(router(&state), "success");

    state.insert("is_valid".to_string(), json!(false));
    assert_eq!(router(&state), "failure");

    // Missing field defaults to false
    let empty_state = State::new();
    assert_eq!(router(&empty_state), "failure");
}

#[test]
fn test_has_tool_calls_router() {
    let router = Router::has_tool_calls("messages", "handle_tools", "respond");

    // No tool calls
    let mut state = State::new();
    state.insert(
        "messages".to_string(),
        json!([
            {"role": "user", "content": "Hello"}
        ]),
    );
    assert_eq!(router(&state), "respond");

    // With tool calls
    state.insert(
        "messages".to_string(),
        json!([
            {"role": "assistant", "tool_calls": [{"name": "search", "args": {}}]}
        ]),
    );
    assert_eq!(router(&state), "handle_tools");
}

#[test]
fn test_max_iterations_router() {
    let router = Router::max_iterations("iteration_count", 3, "continue", "stop");

    let mut state = State::new();
    state.insert("iteration_count".to_string(), json!(1));
    assert_eq!(router(&state), "continue");

    state.insert("iteration_count".to_string(), json!(3));
    assert_eq!(router(&state), "stop");

    state.insert("iteration_count".to_string(), json!(5));
    assert_eq!(router(&state), "stop");
}

#[test]
fn test_on_error_router() {
    let router = Router::on_error("error", "handle_error", "continue");

    // No error
    let mut state = State::new();
    state.insert("error".to_string(), json!(null));
    assert_eq!(router(&state), "continue");

    // With error
    state.insert("error".to_string(), json!("Something went wrong"));
    assert_eq!(router(&state), "handle_error");
}

#[test]
fn test_custom_router() {
    let router = Router::custom(|state| {
        let score = state.get("score").and_then(|v| v.as_i64()).unwrap_or(0);

        match score {
            0..=49 => "low".to_string(),
            50..=79 => "medium".to_string(),
            _ => "high".to_string(),
        }
    });

    let mut state = State::new();

    state.insert("score".to_string(), json!(25));
    assert_eq!(router(&state), "low");

    state.insert("score".to_string(), json!(65));
    assert_eq!(router(&state), "medium");

    state.insert("score".to_string(), json!(90));
    assert_eq!(router(&state), "high");
}

#[test]
fn test_edge_target_equality() {
    assert_eq!(EdgeTarget::End, EdgeTarget::End);
    assert_eq!(EdgeTarget::Node("a".to_string()), EdgeTarget::Node("a".to_string()));
    assert_ne!(EdgeTarget::Node("a".to_string()), EdgeTarget::Node("b".to_string()));
    assert_ne!(EdgeTarget::Node("end".to_string()), EdgeTarget::End);
}
