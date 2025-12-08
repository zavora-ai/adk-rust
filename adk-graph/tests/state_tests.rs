//! State management tests

use adk_graph::state::{Channel, Checkpoint, Reducer, State, StateSchema};
use serde_json::json;
use std::sync::Arc;

#[test]
fn test_overwrite_reducer() {
    let schema = StateSchema::simple(&["value"]);
    let mut state = State::new();

    schema.apply_update(&mut state, "value", json!(1));
    assert_eq!(state.get("value"), Some(&json!(1)));

    schema.apply_update(&mut state, "value", json!(2));
    assert_eq!(state.get("value"), Some(&json!(2)));
}

#[test]
fn test_append_reducer() {
    let schema = StateSchema::builder().list_channel("messages").build();
    let mut state = schema.initialize_state();

    schema.apply_update(&mut state, "messages", json!({"role": "user", "content": "hi"}));
    assert_eq!(state.get("messages"), Some(&json!([{"role": "user", "content": "hi"}])));

    schema.apply_update(&mut state, "messages", json!([{"role": "assistant", "content": "hello"}]));
    assert_eq!(
        state.get("messages"),
        Some(&json!([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"}
        ]))
    );
}

#[test]
fn test_sum_reducer() {
    let schema = StateSchema::builder().counter_channel("count").build();
    let mut state = schema.initialize_state();

    assert_eq!(state.get("count"), Some(&json!(0)));

    schema.apply_update(&mut state, "count", json!(5));
    assert_eq!(state.get("count"), Some(&json!(5.0)));

    schema.apply_update(&mut state, "count", json!(3));
    assert_eq!(state.get("count"), Some(&json!(8.0)));
}

#[test]
fn test_custom_reducer() {
    let schema = StateSchema::builder()
        .channel_with_reducer(
            "max",
            Reducer::Custom(Arc::new(|a, b| {
                let a_num = a.as_f64().unwrap_or(f64::MIN);
                let b_num = b.as_f64().unwrap_or(f64::MIN);
                json!(a_num.max(b_num))
            })),
        )
        .build();
    let mut state = State::new();

    schema.apply_update(&mut state, "max", json!(5));
    schema.apply_update(&mut state, "max", json!(3));
    schema.apply_update(&mut state, "max", json!(8));
    assert_eq!(state.get("max"), Some(&json!(8.0)));
}

#[test]
fn test_state_schema_simple() {
    let schema = StateSchema::simple(&["input", "output", "count"]);

    assert!(schema.channels.contains_key("input"));
    assert!(schema.channels.contains_key("output"));
    assert!(schema.channels.contains_key("count"));
}

#[test]
fn test_state_schema_builder() {
    let schema = StateSchema::builder()
        .channel("name")
        .list_channel("items")
        .counter_channel("count")
        .channel_with_default("status", json!("pending"))
        .build();

    assert!(schema.channels.contains_key("name"));
    assert!(schema.channels.contains_key("items"));
    assert!(schema.channels.contains_key("count"));
    assert!(schema.channels.contains_key("status"));
    assert_eq!(schema.get_default("status"), Some(&json!("pending")));
}

#[test]
fn test_channel_builders() {
    let basic = Channel::new("basic");
    assert_eq!(basic.name, "basic");
    assert!(basic.default.is_none());

    let list = Channel::list("items");
    assert_eq!(list.name, "items");
    assert_eq!(list.default, Some(json!([])));

    let counter = Channel::counter("count");
    assert_eq!(counter.name, "count");
    assert_eq!(counter.default, Some(json!(0)));

    let custom =
        Channel::new("custom").with_reducer(Reducer::Append).with_default(json!(["initial"]));
    assert_eq!(custom.default, Some(json!(["initial"])));
}

#[test]
fn test_initialize_state() {
    let schema = StateSchema::builder()
        .channel_with_default("name", json!("default"))
        .counter_channel("count")
        .list_channel("items")
        .build();

    let state = schema.initialize_state();

    assert_eq!(state.get("name"), Some(&json!("default")));
    assert_eq!(state.get("count"), Some(&json!(0)));
    assert_eq!(state.get("items"), Some(&json!([])));
}

#[test]
fn test_checkpoint_creation() {
    let mut state = State::new();
    state.insert("value".to_string(), json!(42));

    let checkpoint = Checkpoint::new("thread-1", state.clone(), 3, vec!["node_a".to_string()]);

    assert_eq!(checkpoint.thread_id, "thread-1");
    assert_eq!(checkpoint.step, 3);
    assert_eq!(checkpoint.state.get("value"), Some(&json!(42)));
    assert_eq!(checkpoint.pending_nodes, vec!["node_a".to_string()]);
}

#[test]
fn test_checkpoint_with_metadata() {
    let state = State::new();
    let checkpoint = Checkpoint::new("thread-1", state, 0, vec![])
        .with_metadata("source", json!("test"))
        .with_metadata("priority", json!(5));

    assert_eq!(checkpoint.metadata.get("source"), Some(&json!("test")));
    assert_eq!(checkpoint.metadata.get("priority"), Some(&json!(5)));
}
