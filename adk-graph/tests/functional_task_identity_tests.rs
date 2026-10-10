//! A `#[task]` call is identified by its position in the run and its
//! arguments, not by the task's name alone.
//!
//! The execution log used to key every call of a task by the function name, and
//! the log is consulted within the same run, so a task called in a loop
//! returned the first item's cached result for every later item.

#![cfg(feature = "functional")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::error::{GraphError, Result};
use adk_graph::functional::TaskContext;
use adk_graph::node::ExecutionConfig;
use adk_graph::state::State;
use adk_rust_macros::{entrypoint, task};
use serde_json::{Value, json};

static DOUBLE_RUNS: AtomicUsize = AtomicUsize::new(0);

#[task]
async fn double(_ctx: &mut TaskContext, n: i64) -> Result<Value> {
    DOUBLE_RUNS.fetch_add(1, Ordering::SeqCst);
    Ok(json!(n * 2))
}

#[entrypoint]
async fn double_twice(ctx: &mut TaskContext) -> Result<Value> {
    let first = __task_double(ctx, 1).await?;
    let second = __task_double(ctx, 2).await?;
    ctx.set("results", json!([first, second]));
    Ok(Value::Null)
}

#[tokio::test]
async fn two_calls_with_different_arguments_return_their_own_results() {
    let checkpointer = Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>;
    let state = DoubleTwiceAgent::new(checkpointer)
        .invoke(State::new(), ExecutionConfig::new("double-twice"))
        .await
        .expect("the workflow completes");

    assert_eq!(state.get("results"), Some(&json!([2, 4])));
    assert_eq!(DOUBLE_RUNS.load(Ordering::SeqCst), 2, "each call must execute");
}

static SAME_ARG_RUNS: AtomicUsize = AtomicUsize::new(0);

#[task]
async fn stamp(_ctx: &mut TaskContext, label: String) -> Result<Value> {
    let run = SAME_ARG_RUNS.fetch_add(1, Ordering::SeqCst);
    Ok(json!(format!("{label}-{run}")))
}

#[entrypoint]
async fn stamp_twice(ctx: &mut TaskContext) -> Result<Value> {
    let first = __task_stamp(ctx, "same".to_string()).await?;
    let second = __task_stamp(ctx, "same".to_string()).await?;
    ctx.set("stamps", json!([first, second]));
    Ok(Value::Null)
}

/// Equal arguments are still two calls: the ordinal tells them apart.
#[tokio::test]
async fn two_calls_with_equal_arguments_both_execute() {
    let checkpointer = Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>;
    let state = StampTwiceAgent::new(checkpointer)
        .invoke(State::new(), ExecutionConfig::new("stamp-twice"))
        .await
        .expect("the workflow completes");

    assert_eq!(state.get("stamps"), Some(&json!(["same-0", "same-1"])));
}

static PROCESS_RUNS: AtomicUsize = AtomicUsize::new(0);
static FINISH_FAILS: AtomicBool = AtomicBool::new(true);

#[task]
async fn process(_ctx: &mut TaskContext, item: String) -> Result<Value> {
    PROCESS_RUNS.fetch_add(1, Ordering::SeqCst);
    Ok(json!(format!("processed {item}")))
}

/// Named something other than `ctx`, which the wrapper used to assume.
#[task]
async fn finish(context: &mut TaskContext) -> Result<Value> {
    if FINISH_FAILS.swap(false, Ordering::SeqCst) {
        return Err(GraphError::Other("transient failure".to_string()));
    }
    context.set("finished", json!(true));
    Ok(json!("finished"))
}

#[entrypoint]
async fn process_all(ctx: &mut TaskContext) -> Result<Value> {
    let mut outputs = Vec::new();
    for item in ["a", "b", "c"] {
        outputs.push(__task_process(ctx, item.to_string()).await?);
    }
    ctx.set("outputs", json!(outputs));
    __task_finish(ctx).await
}

/// A resumed run replays each completed call from the log, matched by position
/// and arguments, and executes only what had not completed.
#[tokio::test]
async fn a_resumed_run_replays_each_matching_call() {
    let checkpointer = Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>;
    let agent = ProcessAllAgent::new(Arc::clone(&checkpointer));

    let first = agent.invoke(State::new(), ExecutionConfig::new("process-all")).await;
    assert!(first.is_err(), "the first run fails in `finish`");
    assert_eq!(PROCESS_RUNS.load(Ordering::SeqCst), 3);

    let latest = checkpointer
        .load("process-all")
        .await
        .expect("the checkpointer reads")
        .expect("the failed run left a checkpoint");
    let state = agent
        .invoke(
            State::new(),
            ExecutionConfig::new("process-all").with_resume_from(&latest.checkpoint_id),
        )
        .await
        .expect("the resumed run completes");

    assert_eq!(
        PROCESS_RUNS.load(Ordering::SeqCst),
        3,
        "completed calls must be replayed from the log, not executed again"
    );
    assert_eq!(
        state.get("outputs"),
        Some(&json!(["processed a", "processed b", "processed c"])),
        "each replayed call must return its own result"
    );
    assert_eq!(state.get("finished"), Some(&json!(true)));
}
