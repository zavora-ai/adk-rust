//! Checkpoint tests

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::state::{Checkpoint, State};
use serde_json::json;

#[tokio::test]
async fn test_memory_checkpointer_save_and_load() {
    let checkpointer = MemoryCheckpointer::new();

    let mut state = State::new();
    state.insert("value".to_string(), json!(42));

    let checkpoint = Checkpoint::new("thread-1", state.clone(), 0, vec!["node_a".to_string()]);
    let checkpoint_id = checkpoint.checkpoint_id.clone();

    checkpointer.save(&checkpoint).await.unwrap();

    let retrieved = checkpointer.load("thread-1").await.unwrap();
    assert!(retrieved.is_some());

    let cp = retrieved.unwrap();
    assert_eq!(cp.thread_id, "thread-1");
    assert_eq!(cp.state.get("value"), Some(&json!(42)));
    assert_eq!(cp.pending_nodes, vec!["node_a".to_string()]);
    assert_eq!(cp.checkpoint_id, checkpoint_id);
}

#[tokio::test]
async fn test_memory_checkpointer_load_by_id() {
    let checkpointer = MemoryCheckpointer::new();

    let state = State::new();
    let checkpoint = Checkpoint::new("thread-1", state, 5, vec![]);
    let checkpoint_id = checkpoint.checkpoint_id.clone();

    checkpointer.save(&checkpoint).await.unwrap();

    let retrieved = checkpointer.load_by_id(&checkpoint_id).await.unwrap();
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().step, 5);

    // Non-existent ID should return None
    let not_found = checkpointer.load_by_id("nonexistent-id").await.unwrap();
    assert!(not_found.is_none());
}

#[tokio::test]
async fn test_memory_checkpointer_load_latest() {
    let checkpointer = MemoryCheckpointer::new();

    // Save multiple checkpoints
    let state1 = State::new();
    let cp1 = Checkpoint::new("thread-1", state1, 0, vec![]);
    checkpointer.save(&cp1).await.unwrap();

    let state2 = State::new();
    let cp2 = Checkpoint::new("thread-1", state2, 1, vec![]);
    checkpointer.save(&cp2).await.unwrap();

    // load() returns the latest (last saved)
    let latest = checkpointer.load("thread-1").await.unwrap();
    assert!(latest.is_some());
    assert_eq!(latest.unwrap().step, 1);
}

#[tokio::test]
async fn test_memory_checkpointer_list() {
    let checkpointer = MemoryCheckpointer::new();

    // Save checkpoints for different threads
    let state = State::new();
    checkpointer.save(&Checkpoint::new("thread-1", state.clone(), 0, vec![])).await.unwrap();
    checkpointer.save(&Checkpoint::new("thread-1", state.clone(), 1, vec![])).await.unwrap();
    checkpointer.save(&Checkpoint::new("thread-2", state.clone(), 0, vec![])).await.unwrap();

    let thread1_checkpoints = checkpointer.list("thread-1").await.unwrap();
    assert_eq!(thread1_checkpoints.len(), 2);

    let thread2_checkpoints = checkpointer.list("thread-2").await.unwrap();
    assert_eq!(thread2_checkpoints.len(), 1);

    let thread3_checkpoints = checkpointer.list("thread-3").await.unwrap();
    assert!(thread3_checkpoints.is_empty());
}

#[tokio::test]
async fn test_memory_checkpointer_delete() {
    let checkpointer = MemoryCheckpointer::new();

    let state = State::new();
    let checkpoint = Checkpoint::new("thread-1", state, 0, vec![]);

    checkpointer.save(&checkpoint).await.unwrap();

    // Verify it exists
    let exists = checkpointer.load("thread-1").await.unwrap();
    assert!(exists.is_some());

    // Delete it
    checkpointer.delete("thread-1").await.unwrap();

    // Verify it's gone
    let deleted = checkpointer.load("thread-1").await.unwrap();
    assert!(deleted.is_none());
}

#[tokio::test]
async fn test_memory_checkpointer_load_nonexistent() {
    let checkpointer = MemoryCheckpointer::new();

    let result = checkpointer.load("nonexistent").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_checkpoint_with_complex_state() {
    let checkpointer = MemoryCheckpointer::new();

    let mut state = State::new();
    state.insert(
        "messages".to_string(),
        json!([
            {"role": "user", "content": "Hello"},
            {"role": "assistant", "content": "Hi there!"}
        ]),
    );
    state.insert(
        "metadata".to_string(),
        json!({
            "session_id": "abc123",
            "user_id": "user456"
        }),
    );
    state.insert("count".to_string(), json!(42));

    let checkpoint = Checkpoint::new("complex-thread", state, 5, vec!["node_x".to_string()])
        .with_metadata("source", json!("test"))
        .with_metadata("version", json!(1));

    checkpointer.save(&checkpoint).await.unwrap();

    let retrieved = checkpointer.load("complex-thread").await.unwrap().unwrap();

    assert_eq!(retrieved.step, 5);
    assert_eq!(retrieved.pending_nodes, vec!["node_x".to_string()]);
    assert_eq!(retrieved.metadata.get("source"), Some(&json!("test")));
    assert_eq!(retrieved.metadata.get("version"), Some(&json!(1)));

    let messages = retrieved.state.get("messages").unwrap();
    assert!(messages.is_array());
    assert_eq!(messages.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_checkpoint_ordering() {
    let checkpointer = MemoryCheckpointer::new();

    // Save checkpoints in order
    for step in 0..5 {
        let mut state = State::new();
        state.insert("step".to_string(), json!(step));
        let checkpoint = Checkpoint::new("ordered-thread", state, step, vec![]);
        checkpointer.save(&checkpoint).await.unwrap();
    }

    // List should preserve order
    let checkpoints = checkpointer.list("ordered-thread").await.unwrap();
    assert_eq!(checkpoints.len(), 5);

    for (i, cp) in checkpoints.iter().enumerate() {
        assert_eq!(cp.step, i);
    }
}
