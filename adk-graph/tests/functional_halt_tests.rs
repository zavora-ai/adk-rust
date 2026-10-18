//! A pause and a cancellation stop a functional workflow without failing it.
//!
//! `#[task(retry)]` retried any error, a suspension included: each pause waited
//! through the whole backoff, re-asked under a new key each attempt, and was then
//! recorded as a failed task. The `#[entrypoint]` built its own cancellation
//! token, so a caller had no way to cancel a run.

#![cfg(feature = "functional")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::error::{GraphError, Result};
use adk_graph::functional::{ExecutionLog, FunctionalError, TaskContext, TaskStatus};
use adk_graph::node::ExecutionConfig;
use adk_graph::state::State;
use adk_rust_macros::{entrypoint, task};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[task(retry(max_attempts = 3, backoff = "1s"))]
async fn approve(ctx: &mut TaskContext) -> Result<Value> {
    let approved: bool = ctx.interrupt("approve the refund?").await?;
    Ok(json!(approved))
}

#[entrypoint]
async fn refund(ctx: &mut TaskContext) -> Result<Value> {
    let approved = __task_approve(ctx).await?;
    ctx.set("approved", approved.clone());
    Ok(approved)
}

#[tokio::test]
async fn a_retried_task_suspends_at_once_under_its_first_key() {
    let checkpointer: Arc<dyn Checkpointer> = Arc::new(MemoryCheckpointer::new());
    let workflow = RefundAgent::new(Arc::clone(&checkpointer));

    let started = Instant::now();
    let outcome = workflow.invoke(State::new(), ExecutionConfig::new("refund-halt")).await;
    let elapsed = started.elapsed();

    match outcome {
        Err(GraphError::Suspended { continuation_key, .. }) => {
            assert_eq!(continuation_key, "interrupt-1");
        }
        other => panic!("expected a suspension, got {other:?}"),
    }
    assert!(elapsed < Duration::from_millis(500), "suspended after {elapsed:?}");

    let latest = checkpointer.load("refund-halt").await.unwrap().expect("a checkpoint");
    let log: ExecutionLog =
        serde_json::from_value(latest.metadata["execution_log"].clone()).expect("a log");
    assert!(
        log.tasks.values().all(|record| record.status != TaskStatus::Failed),
        "a suspension is not a failure: {:?}",
        log.tasks
    );

    let state = workflow
        .invoke(
            State::new(),
            ExecutionConfig::new("refund-halt")
                .with_resume_from(&latest.checkpoint_id)
                .with_resume_value("interrupt-1", json!(true)),
        )
        .await
        .unwrap();
    assert_eq!(state.get("approved"), Some(&json!(true)));
}

static STARTED: AtomicUsize = AtomicUsize::new(0);
static SENT: AtomicUsize = AtomicUsize::new(0);

#[task]
async fn draft(ctx: &mut TaskContext) -> Result<Value> {
    let _ = ctx;
    STARTED.fetch_add(1, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(json!("draft"))
}

#[task]
async fn send(ctx: &mut TaskContext) -> Result<Value> {
    let _ = ctx;
    SENT.fetch_add(1, Ordering::SeqCst);
    Ok(json!("sent"))
}

#[entrypoint]
async fn reply(ctx: &mut TaskContext) -> Result<Value> {
    let text = __task_draft(ctx).await?;
    __task_send(ctx).await?;
    ctx.set("reply", text);
    Ok(json!("done"))
}

#[tokio::test]
async fn a_caller_cancels_a_running_workflow() {
    let checkpointer: Arc<dyn Checkpointer> = Arc::new(MemoryCheckpointer::new());
    let workflow = ReplyAgent::new(Arc::clone(&checkpointer));
    let token = CancellationToken::new();
    let canceller = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        canceller.cancel();
    });

    let started = Instant::now();
    let outcome = workflow
        .invoke(State::new(), ExecutionConfig::new("reply-cancel").with_cancellation_token(token))
        .await;

    assert!(matches!(outcome, Err(GraphError::Cancelled)), "{outcome:?}");
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "returned after {:?}",
        started.elapsed()
    );
    assert_eq!((STARTED.load(Ordering::SeqCst), SENT.load(Ordering::SeqCst)), (1, 0));
    // Nothing is written for the cancelled run: the latest checkpoint is the one
    // written before the workflow started, so a resume runs the draft again.
    let latest = checkpointer.load("reply-cancel").await.unwrap().expect("a checkpoint");
    assert_eq!(latest.metadata.get("phase"), Some(&json!("pre_execution")));
}

#[test]
fn a_functional_suspension_converts_to_a_typed_graph_error() {
    let suspended = GraphError::from(FunctionalError::Suspended {
        continuation_key: "interrupt-2".to_string(),
        message: "approve?".to_string(),
    });
    assert!(matches!(
        suspended,
        GraphError::Suspended { ref continuation_key, .. } if continuation_key == "interrupt-2"
    ));
    assert!(matches!(GraphError::from(FunctionalError::Cancelled), GraphError::Cancelled));
}
