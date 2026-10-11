//! Conformance cases for [`ActionLedger`] backends.
//!
//! | Case | Contract |
//! |------|----------|
//! | [`begin_complete_round_trip`] | `get` returns what `begin` stored, then the outcome `complete` recorded |
//! | [`recorded_call_is_answered_without_executing`] | A replayed call is answered from its record; the tool runs once |
//! | [`incomplete_call_is_outcome_unknown_after_restart`] | A begun, never-completed call reads back without an outcome after a restart and cannot begin again |
//! | [`concurrent_begin_has_one_winner`] | Concurrent `begin` calls for one key, across two handles, admit exactly one |
//! | [`outcomes_are_write_once`] | `complete` fails for an unknown key or a recorded outcome; concurrent completes admit one |
//!
//! Timestamps must round-trip with at least millisecond precision. The suite builds
//! records whose `started_at` is a whole millisecond and compares them whole.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::json;

use super::unique;
use crate::{ActionLedger, ActionOutcome, ActionRecord, ToolEffect, json_digest};

/// Opens an [`ActionLedger`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::conformance::ActionLedgerHarness;
/// use adk_core::{ActionLedger, InMemoryActionLedger, async_trait};
///
/// struct InMemory(Arc<InMemoryActionLedger>);
///
/// #[async_trait]
/// impl ActionLedgerHarness for InMemory {
///     async fn open(&self) -> Arc<dyn ActionLedger> {
///         self.0.clone()
///     }
/// }
/// ```
#[async_trait]
pub trait ActionLedgerHarness: Send + Sync {
    /// Opens the ledger.
    ///
    /// For a durable backend every call returns a new handle over the same stored data,
    /// as a restarted process or a second process would see it. A process-local backend
    /// returns the same ledger every time. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn ActionLedger>;
}

fn whole_millisecond(at: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(at.timestamp_millis()).expect("in range")
}

fn record(key: &str, effect: ToolEffect) -> ActionRecord {
    let mut record = ActionRecord::new(
        key,
        "conformance_transfer",
        &json!({ "amount": 5, "to": "acct-1" }),
        effect,
    );
    record.started_at = whole_millisecond(record.started_at);
    record
}

fn succeeded(value: serde_json::Value) -> ActionOutcome {
    ActionOutcome::Succeeded { result_digest: json_digest(&value) }
}

async fn read(ledger: &dyn ActionLedger, key: &str) -> Option<ActionRecord> {
    ledger.get(key).await.expect("the ledger can be read")
}

/// `get` returns exactly what `begin` stored, and after `complete` the same record with
/// the outcome and a completion time.
pub async fn begin_complete_round_trip<H: ActionLedgerHarness + ?Sized>(harness: &H) {
    let ledger = harness.open().await;
    assert_eq!(read(&*ledger, &unique("missing")).await, None);

    for (effect, outcome) in [
        (ToolEffect::NonIdempotent, succeeded(json!({ "receipt": "r-1" }))),
        (ToolEffect::Idempotent, ActionOutcome::Failed { error: "card declined".into() }),
    ] {
        let key = unique("call");
        let begun = record(&key, effect);
        ledger.begin(&begun).await.expect("begin a new key");
        assert_eq!(read(&*ledger, &key).await, Some(begun.clone()), "the begun record");

        ledger.complete(&key, outcome.clone()).await.expect("complete a begun key");
        let stored = read(&*ledger, &key).await.expect("the record exists");
        assert!(stored.completed_at.is_some(), "complete records when it happened");
        let expected =
            ActionRecord { outcome: Some(outcome), completed_at: stored.completed_at, ..begun };
        assert_eq!(stored, expected, "the completed record");
    }
}

/// What a dispatcher does with a call, following the [`ActionLedger`] decision table.
#[derive(Debug, PartialEq)]
enum Dispatch {
    Executed(ActionOutcome),
    Recorded(ActionOutcome),
    OutcomeUnknown,
}

async fn dispatch(
    ledger: &dyn ActionLedger,
    call: &ActionRecord,
    executions: &AtomicUsize,
) -> Dispatch {
    match read(ledger, &call.key).await {
        Some(ActionRecord { outcome: Some(outcome), .. }) => Dispatch::Recorded(outcome),
        Some(_) => Dispatch::OutcomeUnknown,
        None => {
            ledger.begin(call).await.expect("begin");
            executions.fetch_add(1, Ordering::SeqCst);
            let outcome = succeeded(json!({ "receipt": call.key }));
            ledger.complete(&call.key, outcome.clone()).await.expect("complete");
            Dispatch::Executed(outcome)
        }
    }
}

/// A call replayed under the same key — by the same process or after a restart — is
/// answered from its record, and the tool executes once.
pub async fn recorded_call_is_answered_without_executing<H: ActionLedgerHarness + ?Sized>(
    harness: &H,
) {
    let key = unique("call");
    let call = record(&key, ToolEffect::NonIdempotent);
    let executions = AtomicUsize::new(0);

    let first = dispatch(&*harness.open().await, &call, &executions).await;
    let Dispatch::Executed(outcome) = first else {
        panic!("a new key executes, got {first:?}");
    };
    let ledger = harness.open().await;
    assert_eq!(dispatch(&*ledger, &call, &executions).await, Dispatch::Recorded(outcome));
    assert_eq!(executions.load(Ordering::SeqCst), 1, "the tool ran once");

    let error = ledger.begin(&call).await.expect_err("a recorded key cannot begin again");
    assert_eq!(error.code, "tool.action_ledger.duplicate_key", "{error}");
}

/// A call that began and never completed — the process died mid-call — reads back
/// without an outcome after a restart, is answered as outcome-unknown, and cannot begin
/// again. Its outcome can still be recorded later.
pub async fn incomplete_call_is_outcome_unknown_after_restart<H: ActionLedgerHarness + ?Sized>(
    harness: &H,
) {
    let key = unique("call");
    let call = record(&key, ToolEffect::NonIdempotent);
    harness.open().await.begin(&call).await.expect("begin, then the process dies");

    let ledger = harness.open().await;
    assert_eq!(read(&*ledger, &key).await, Some(call.clone()), "the begun record survives");
    let executions = AtomicUsize::new(0);
    assert_eq!(dispatch(&*ledger, &call, &executions).await, Dispatch::OutcomeUnknown);
    assert_eq!(executions.load(Ordering::SeqCst), 0, "the tool does not run again");
    let error = ledger.begin(&call).await.expect_err("an incomplete key cannot begin again");
    assert_eq!(error.code, "tool.action_ledger.duplicate_key", "{error}");

    let outcome = ActionOutcome::Failed { error: "reconciled by an operator".into() };
    ledger.complete(&key, outcome.clone()).await.expect("complete after the restart");
    assert_eq!(read(&*ledger, &key).await.and_then(|stored| stored.outcome), Some(outcome));
}

/// Concurrent `begin` calls for one key, split across two handles when the backend is
/// durable, admit exactly one; the rest fail with `tool.action_ledger.duplicate_key`.
pub async fn concurrent_begin_has_one_winner<H: ActionLedgerHarness + ?Sized>(harness: &H) {
    let first = harness.open().await;
    let second = harness.open().await;
    let call = record(&unique("call"), ToolEffect::NonIdempotent);

    let tasks: Vec<_> = (0..32)
        .map(|i| {
            let ledger = if i % 2 == 0 { first.clone() } else { second.clone() };
            let call = call.clone();
            tokio::spawn(async move { ledger.begin(&call).await })
        })
        .collect();
    let mut winners = 0;
    for task in tasks {
        match task.await.expect("begin task completes") {
            Ok(()) => winners += 1,
            Err(error) => assert_eq!(error.code, "tool.action_ledger.duplicate_key", "{error}"),
        }
    }
    assert_eq!(winners, 1, "exactly one begin wins");
}

/// `complete` fails for a key that never began and for a record that already has an
/// outcome, and concurrent completes of one record admit exactly one.
pub async fn outcomes_are_write_once<H: ActionLedgerHarness + ?Sized>(harness: &H) {
    let ledger = harness.open().await;
    let error = ledger
        .complete(&unique("missing"), succeeded(json!(null)))
        .await
        .expect_err("complete needs a begun record");
    assert!(error.is_not_found(), "{error}");

    let key = unique("call");
    ledger.begin(&record(&key, ToolEffect::NonIdempotent)).await.expect("begin");
    let first = succeeded(json!({ "receipt": 1 }));
    ledger.complete(&key, first.clone()).await.expect("first complete");
    let error = ledger
        .complete(&key, succeeded(json!({ "receipt": 2 })))
        .await
        .expect_err("outcomes are write-once");
    assert_eq!(error.code, "tool.action_ledger.already_completed", "{error}");
    assert_eq!(read(&*ledger, &key).await.and_then(|stored| stored.outcome), Some(first));

    let key = unique("call");
    ledger.begin(&record(&key, ToolEffect::NonIdempotent)).await.expect("begin");
    let second = harness.open().await;
    let tasks: Vec<_> = (0..16)
        .map(|i| {
            let ledger = if i % 2 == 0 { ledger.clone() } else { second.clone() };
            let key = key.clone();
            let outcome = succeeded(json!({ "receipt": i }));
            tokio::spawn(
                async move { ledger.complete(&key, outcome.clone()).await.map(|()| outcome) },
            )
        })
        .collect();
    let mut winners = Vec::new();
    for task in tasks {
        match task.await.expect("complete task completes") {
            Ok(outcome) => winners.push(outcome),
            Err(error) => assert_eq!(error.code, "tool.action_ledger.already_completed", "{error}"),
        }
    }
    assert_eq!(winners.len(), 1, "exactly one complete wins");
    assert_eq!(read(&*ledger, &key).await.and_then(|stored| stored.outcome), winners.pop());
}
