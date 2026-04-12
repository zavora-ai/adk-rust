//! Integration tests for SharedState coordination primitives.

use adk_core::{SharedState, SharedStateError};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn test_wait_for_key_wakes_on_set() {
    let state = Arc::new(SharedState::new());
    let state2 = state.clone();

    let handle =
        tokio::spawn(async move { state2.wait_for_key("workbook", Duration::from_secs(5)).await });

    // Small delay to ensure waiter is blocked
    tokio::time::sleep(Duration::from_millis(50)).await;

    state.set_shared("workbook", serde_json::json!("wb-123")).await.unwrap();

    let result = handle.await.unwrap().unwrap();
    assert_eq!(result, serde_json::json!("wb-123"));
}

#[tokio::test]
async fn test_multiple_waiters_all_wake() {
    let state = Arc::new(SharedState::new());

    let mut handles = Vec::new();
    for _ in 0..3 {
        let s = state.clone();
        handles.push(tokio::spawn(async move {
            s.wait_for_key("handle", Duration::from_secs(5)).await
        }));
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    state.set_shared("handle", serde_json::json!(42)).await.unwrap();

    for handle in handles {
        let result = handle.await.unwrap().unwrap();
        assert_eq!(result, serde_json::json!(42));
    }
}

#[tokio::test]
async fn test_shared_state_serialize() {
    let state = SharedState::new();
    state.set_shared("a", serde_json::json!(1)).await.unwrap();
    state.set_shared("b", serde_json::json!("hello")).await.unwrap();

    let json = serde_json::to_value(&state).unwrap();
    assert_eq!(json["a"], 1);
    assert_eq!(json["b"], "hello");
}

#[tokio::test]
async fn test_shared_state_debug() {
    let state = SharedState::new();
    state.set_shared("key", serde_json::json!("val")).await.unwrap();
    let debug = format!("{state:?}");
    assert!(!debug.is_empty());
}

#[tokio::test]
async fn test_concurrent_set_and_get() {
    let state = Arc::new(SharedState::new());
    let mut handles = Vec::new();

    // 10 concurrent writers
    for i in 0..10 {
        let s = state.clone();
        handles.push(tokio::spawn(async move {
            s.set_shared(format!("key-{i}"), serde_json::json!(i)).await.unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // All 10 keys should be present
    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.len(), 10);
    for i in 0..10 {
        assert_eq!(snapshot[&format!("key-{i}")], serde_json::json!(i));
    }
}

#[tokio::test]
async fn test_wait_for_key_timeout() {
    let state = SharedState::new();
    let err = state.wait_for_key("missing", Duration::from_millis(10)).await.unwrap_err();
    assert!(matches!(err, SharedStateError::Timeout { .. }));
}
