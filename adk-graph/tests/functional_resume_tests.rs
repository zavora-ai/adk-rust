//! A resumed `#[entrypoint]` run keeps its execution log, honours the
//! checkpoint it was asked to resume from, and delivers interrupt values.
//!
//! The generated `invoke` read the log from the thread's latest checkpoint and
//! then wrote a pre-execution checkpoint without it. A crash during that resumed
//! run left the log-less checkpoint as the latest, so the next resume ran every
//! completed task again. It also built its `TaskContext` without resume values,
//! so an interrupt could never be answered through the generated API.

#![cfg(feature = "functional")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::error::{GraphError, Result};
use adk_graph::functional::TaskContext;
use adk_graph::node::ExecutionConfig;
use adk_graph::state::State;
use adk_rust_macros::{entrypoint, task};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The id of the thread's latest checkpoint, as a caller resuming it reads it.
async fn latest_checkpoint_id(checkpointer: &Arc<dyn Checkpointer>, thread_id: &str) -> String {
    checkpointer
        .load(thread_id)
        .await
        .expect("the checkpointer reads")
        .expect("the thread has a checkpoint")
        .checkpoint_id
}

// ─── Crash during a resumed run ──────────────────────────────────────────────

static PAYMENTS: AtomicUsize = AtomicUsize::new(0);
static SHIP_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

#[task]
async fn charge_card(_ctx: &mut TaskContext) -> Result<Value> {
    PAYMENTS.fetch_add(1, Ordering::SeqCst);
    Ok(json!("charged"))
}

/// Never finishes on its first two attempts, standing in for a process that
/// dies mid-task: nothing after this point is written.
#[task]
async fn ship_order(_ctx: &mut TaskContext) -> Result<Value> {
    if SHIP_ATTEMPTS.fetch_add(1, Ordering::SeqCst) < 2 {
        std::future::pending::<()>().await;
    }
    Ok(json!("shipped"))
}

#[entrypoint]
async fn fulfil_order(ctx: &mut TaskContext) -> Result<Value> {
    let charge = __task_charge_card(ctx).await?;
    let shipment = __task_ship_order(ctx).await?;
    ctx.set("charge", charge);
    ctx.set("shipment", shipment);
    Ok(Value::Null)
}

#[tokio::test(start_paused = true)]
async fn a_crash_during_a_resumed_run_does_not_repeat_completed_tasks() {
    let checkpointer = Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>;
    let agent = FulfilOrderAgent::new(Arc::clone(&checkpointer));
    let thread = "order-7";

    // The first run charges the card and crashes while shipping.
    let crashed = tokio::time::timeout(
        Duration::from_secs(1),
        agent.invoke(State::new(), ExecutionConfig::new(thread)),
    )
    .await;
    assert!(crashed.is_err(), "the first run must crash inside `ship_order`");

    // The resumed run crashes in the same place.
    let resume_from = latest_checkpoint_id(&checkpointer, thread).await;
    let crashed = tokio::time::timeout(
        Duration::from_secs(1),
        agent.invoke(State::new(), ExecutionConfig::new(thread).with_resume_from(&resume_from)),
    )
    .await;
    assert!(crashed.is_err(), "the resumed run must crash inside `ship_order` too");

    // The third run resumes from whatever the second left as the latest.
    let resume_from = latest_checkpoint_id(&checkpointer, thread).await;
    let state = agent
        .invoke(State::new(), ExecutionConfig::new(thread).with_resume_from(&resume_from))
        .await
        .expect("the third run completes");

    assert_eq!(
        (PAYMENTS.load(Ordering::SeqCst), SHIP_ATTEMPTS.load(Ordering::SeqCst)),
        (1, 3),
        "the card must be charged once across both crashes"
    );
    assert_eq!(
        (state.get("charge"), state.get("shipment")),
        (Some(&json!("charged")), Some(&json!("shipped")))
    );
}

// ─── Interrupt and resume through the generated API ─────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Approval {
    approved: bool,
    approver: String,
}

static QUOTES: AtomicUsize = AtomicUsize::new(0);
static REFUNDS: AtomicUsize = AtomicUsize::new(0);

#[task]
async fn quote_refund(_ctx: &mut TaskContext) -> Result<Value> {
    QUOTES.fetch_add(1, Ordering::SeqCst);
    Ok(json!(120))
}

#[task]
async fn issue_refund(_ctx: &mut TaskContext, amount: i64) -> Result<Value> {
    REFUNDS.fetch_add(1, Ordering::SeqCst);
    Ok(json!(format!("refunded {amount}")))
}

#[entrypoint]
async fn approve_refund(ctx: &mut TaskContext) -> Result<Value> {
    let amount = __task_quote_refund(ctx).await?.as_i64().unwrap_or_default();
    let approval: Approval = ctx.interrupt("approve the refund").await?;
    ctx.set("approver", json!(approval.approver));
    if approval.approved {
        let receipt = __task_issue_refund(ctx, amount).await?;
        ctx.set("receipt", receipt);
    }
    Ok(Value::Null)
}

#[tokio::test]
async fn an_interrupt_resumes_with_the_supplied_value() {
    let checkpointer = Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>;
    let agent = ApproveRefundAgent::new(Arc::clone(&checkpointer));
    let thread = "refund-42";

    let suspended = agent
        .invoke(State::new(), ExecutionConfig::new(thread))
        .await
        .expect_err("the first run must suspend at the interrupt");
    assert!(suspended.to_string().contains("interrupt-1"), "{suspended}");

    // Resume from the interrupt's own checkpoint, not the thread's latest.
    let interrupt_checkpoint = checkpointer
        .list(thread)
        .await
        .expect("the checkpointer reads")
        .into_iter()
        .find(|checkpoint| {
            checkpoint.metadata.get("continuation_key") == Some(&json!("interrupt-1"))
        })
        .expect("the interrupt writes a checkpoint");

    let state = agent
        .invoke(
            State::new(),
            ExecutionConfig::new(thread)
                .with_resume_from(&interrupt_checkpoint.checkpoint_id)
                .with_resume_value("interrupt-1", json!({ "approved": true, "approver": "alice" })),
        )
        .await
        .expect("the supplied value must resume the workflow");

    assert_eq!(
        (state.get("approver"), state.get("receipt")),
        (Some(&json!("alice")), Some(&json!("refunded 120")))
    );
    assert_eq!(
        (QUOTES.load(Ordering::SeqCst), REFUNDS.load(Ordering::SeqCst)),
        (1, 1),
        "the quote completed before the interrupt and must replay from the log"
    );
}

// ─── Resuming another thread's checkpoint ────────────────────────────────────

static LEDGER_WRITES: AtomicUsize = AtomicUsize::new(0);

#[task]
async fn write_ledger(ctx: &mut TaskContext) -> Result<Value> {
    LEDGER_WRITES.fetch_add(1, Ordering::SeqCst);
    let owner = ctx.get::<String>("owner").unwrap_or_default();
    Ok(json!(format!("ledger of {owner}")))
}

#[entrypoint]
async fn keep_ledger(ctx: &mut TaskContext) -> Result<Value> {
    let ledger = __task_write_ledger(ctx).await?;
    ctx.set("ledger", ledger);
    Ok(Value::Null)
}

#[tokio::test]
async fn a_checkpoint_of_another_thread_is_refused() {
    let checkpointer = Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>;
    let agent = KeepLedgerAgent::new(Arc::clone(&checkpointer));

    let alice = State::from([("owner".to_string(), json!("alice"))]);
    agent.invoke(alice, ExecutionConfig::new("alice-ledger")).await.expect("alice's run completes");
    let alices_checkpoint = latest_checkpoint_id(&checkpointer, "alice-ledger").await;

    for resume_from in [alices_checkpoint.as_str(), "no-such-checkpoint"] {
        let error = agent
            .invoke(
                State::new(),
                ExecutionConfig::new("mallory-ledger").with_resume_from(resume_from),
            )
            .await
            .expect_err("a checkpoint outside the thread must not be resumed");
        assert!(
            matches!(&error, GraphError::CheckpointError(message) if message.contains("no checkpoint with id")),
            "{error}"
        );
    }

    assert_eq!(LEDGER_WRITES.load(Ordering::SeqCst), 1, "a refused resume must not run any task");
    assert!(
        checkpointer.load("mallory-ledger").await.expect("the checkpointer reads").is_none(),
        "a refused resume must not write to the thread"
    );
}
