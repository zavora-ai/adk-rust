//! Validation example for durable resume-from-checkpoint in adk-graph.
//!
//! Validates:
//! - MemoryCheckpointer save/load round-trip (Req 5.1–5.4, 6.1–6.3)
//! - Graph executor resumes from checkpoint (Req 8.1–8.4)
//! - Fresh start when no checkpoint exists (Req 8.3)
//! - StreamEvent::Resumed emitted on resume (design doc)
//!
//! Run: cargo run --manifest-path examples/competitive_graph_resume/Cargo.toml

use adk_graph::prelude::*;
use futures::StreamExt;
use std::pin::pin;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Competitive Improvements: Graph Durable Resume Validation ===\n");

    validate_checkpointer_roundtrip().await;
    validate_fresh_start().await;
    validate_resume_from_checkpoint().await;
    validate_stream_resume_event().await;

    println!("\n=== All graph resume validations passed ===");
}

async fn validate_checkpointer_roundtrip() {
    println!("--- MemoryCheckpointer round-trip ---");

    let cp = MemoryCheckpointer::default();

    let mut state = State::new();
    state.insert("counter".to_string(), json!(42));

    let checkpoint = Checkpoint::new("thread-1", state, 3, vec!["node_b".to_string()]);

    let id = cp.save(&checkpoint).await.expect("save should succeed");
    assert!(!id.is_empty());
    println!("  ✓ save() returns non-empty checkpoint ID");

    let loaded = cp.load("thread-1").await.expect("load should succeed");
    assert!(loaded.is_some());
    let loaded = loaded.unwrap();
    assert_eq!(loaded.thread_id, "thread-1");
    assert_eq!(loaded.state.get("counter"), Some(&json!(42)));
    assert_eq!(loaded.pending_nodes, vec!["node_b"]);
    assert_eq!(loaded.step, 3);
    println!("  ✓ load() returns saved state, pending_nodes, and step");

    let by_id = cp.load_by_id(&id).await.expect("load_by_id should succeed");
    assert!(by_id.is_some());
    println!("  ✓ load_by_id() finds checkpoint by ID");

    let missing = cp.load("nonexistent").await.expect("load should succeed");
    assert!(missing.is_none());
    println!("  ✓ load() returns None for unknown thread");

    let list = cp.list("thread-1").await.expect("list should succeed");
    assert_eq!(list.len(), 1);
    println!("  ✓ list() returns saved checkpoints");

    cp.delete("thread-1").await.expect("delete should succeed");
    let after_delete = cp.load("thread-1").await.expect("load should succeed");
    assert!(after_delete.is_none());
    println!("  ✓ delete() removes checkpoints");
}

fn build_add_double_graph(
    name: &str,
    checkpointer: Arc<MemoryCheckpointer>,
) -> GraphAgent {
    GraphAgent::builder(name)
        .description("add_one then double")
        .state_schema(
            StateSchemaBuilder::default()
                .channel_with_default("value", json!(0))
                .build(),
        )
        .node_fn("add_one", |ctx: NodeContext| async move {
            let val = ctx.state.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(val + 1)))
        })
        .node_fn("double", |ctx: NodeContext| async move {
            let val = ctx.state.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("value", json!(val * 2)))
        })
        .edge(START, "add_one")
        .edge("add_one", "double")
        .edge("double", END)
        .checkpointer_arc(checkpointer)
        .build()
        .expect("graph should build")
}

async fn validate_fresh_start() {
    println!("\n--- Fresh start (no checkpoint) ---");

    let cp = Arc::new(MemoryCheckpointer::default());
    let graph = build_add_double_graph("fresh_test", cp);

    let mut input = State::new();
    input.insert("value".to_string(), json!(5));

    let config = ExecutionConfig::new("fresh-thread");
    let result = graph.invoke(input, config).await.expect("invoke should succeed");

    let value = result.get("value").and_then(|v| v.as_i64()).unwrap_or(-1);
    assert_eq!(value, 12, "expected (5+1)*2 = 12, got {value}");
    println!("  ✓ Fresh execution produces correct result: (5+1)*2 = {value}");
}

async fn validate_resume_from_checkpoint() {
    println!("\n--- Resume from checkpoint ---");

    let cp = Arc::new(MemoryCheckpointer::default());

    // Pre-populate a checkpoint as if "add_one" already ran
    let mut saved_state = State::new();
    saved_state.insert("value".to_string(), json!(6));
    let checkpoint = Checkpoint::new("resume-thread", saved_state, 1, vec!["double".to_string()]);
    cp.save(&checkpoint).await.expect("pre-save should succeed");

    let graph = build_add_double_graph("resume_test", cp);

    let input = State::new();
    let config = ExecutionConfig::new("resume-thread");
    let result = graph.invoke(input, config).await.expect("invoke should succeed");

    let value = result.get("value").and_then(|v| v.as_i64()).unwrap_or(-1);
    assert_eq!(value, 12, "expected resumed 6*2 = 12, got {value}");
    println!("  ✓ Resumed execution skips completed nodes: 6*2 = {value}");
}

async fn validate_stream_resume_event() {
    println!("\n--- StreamEvent::Resumed on resume ---");

    let cp = Arc::new(MemoryCheckpointer::default());

    let mut saved_state = State::new();
    saved_state.insert("value".to_string(), json!(10));
    let checkpoint = Checkpoint::new("stream-resume", saved_state, 1, vec!["double".to_string()]);
    cp.save(&checkpoint).await.unwrap();

    let graph = build_add_double_graph("stream_resume_test", cp);

    let input = State::new();
    let config = ExecutionConfig::new("stream-resume");
    let mut stream = pin!(graph.stream(input, config, StreamMode::Values));

    let mut saw_resumed = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("stream event should be ok");
        if let StreamEvent::Resumed { step, pending_nodes } = &event {
            saw_resumed = true;
            assert_eq!(*step, 1);
            assert_eq!(pending_nodes, &["double"]);
            println!("  ✓ Received Resumed event: step={step}, pending={pending_nodes:?}");
        }
    }

    assert!(saw_resumed, "should have received a Resumed stream event");
    println!("  ✓ StreamEvent::Resumed emitted on checkpoint resume");
}
