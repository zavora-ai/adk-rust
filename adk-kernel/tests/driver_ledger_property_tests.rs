//! A re-issued input-required round never executes a completed non-idempotent driver call
//! twice under the action ledger (spec 3.2 §7.7, §13 Q9, K-I4).
//!
//! Each case parks a `send_note` call on its server's question, records the decision, and
//! dispatches the held call one to four times as an agent resuming it would — keyed by the
//! held request, beginning a ledger record before each execution and completing it after —
//! with any dispatch crashing before or after its execution. The server records every
//! execution.

#![cfg(feature = "drivers")]

use crate::driver_support;

use std::sync::{Arc, OnceLock};

use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, ApprovalScope, ApprovalStore, InMemoryActionLedger,
    InMemoryApprovalStore, ReadonlyContext, RunConfig, Tool, ToolApproval, ToolEffect,
};
use adk_kernel::PartitionId;
use adk_kernel::drivers::{DriverStore, DriverSupervisor};
use adk_kernel::stores::InMemoryDriverStore;
use driver_support::{RunContext, Workdir};
use proptest::prelude::*;
use serde_json::json;

/// One driver process shared by every case, with its stores.
struct Shared {
    runtime: tokio::runtime::Runtime,
    work: Workdir,
    store: Arc<InMemoryDriverStore>,
    tool: Arc<dyn Tool>,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let work = Workdir::new();
        let store = Arc::new(InMemoryDriverStore::new());
        let tool = runtime.block_on(async {
            let drivers = DriverSupervisor::builder(store.clone()).home(work.path()).build();
            drivers
                .mount(work.manifest("notes", driver_support::UNSANDBOXED), "alice")
                .await
                .unwrap();
            let ctx = RunContext::new(None, "setup", "setup");
            let tools = drivers
                .toolset_for(&PartitionId::default(), &["notes".to_string()])
                .tools(ctx as Arc<dyn ReadonlyContext>)
                .await
                .unwrap();
            // The supervisor lives as long as the tool, which holds it.
            tools.into_iter().find(|tool| tool.name() == "send_note").unwrap()
        });
        Shared { runtime, work, store, tool }
    })
}

/// Where a dispatch crashes, if it does.
#[derive(Debug, Clone, Copy)]
enum Crash {
    None,
    /// After the ledger record is begun, before the tool runs.
    BeforeExecution,
    /// After the tool ran, before its outcome is recorded.
    AfterExecution,
}

fn crash() -> impl Strategy<Value = Crash> {
    prop_oneof![Just(Crash::None), Just(Crash::BeforeExecution), Just(Crash::AfterExecution)]
}

/// Dispatches `send_note` as an agent does under the action ledger; `true` when it ran.
async fn dispatch(
    ledger: &InMemoryActionLedger,
    ctx: Arc<RunContext>,
    text: &str,
    crash: Crash,
) -> bool {
    let tool = &shared().tool;
    let key = adk_core::ToolContext::idempotency_key(ctx.as_ref());
    let args = json!({ "text": text });
    if ledger.get(&key).await.unwrap().is_some() {
        return false;
    }
    ledger
        .begin(&ActionRecord::new(key.clone(), tool.name(), &args, ToolEffect::NonIdempotent))
        .await
        .unwrap();
    if matches!(crash, Crash::BeforeExecution) {
        return false;
    }
    let result = tool.execute(ctx, args).await;
    if matches!(crash, Crash::AfterExecution) {
        return true;
    }
    let outcome = match &result {
        Ok(value) => ActionOutcome::Succeeded { result_digest: adk_core::json_digest(value) },
        Err(error) => ActionOutcome::Failed { error: error.to_string() },
    };
    ledger.complete(&key, outcome).await.unwrap();
    true
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn a_reissued_round_never_executes_a_completed_call_twice(
        resumes in proptest::collection::vec(crash(), 1..5),
    ) {
        let shared = shared();
        let executed = shared.runtime.block_on(async {
            let session = format!("s-{}", uuid::Uuid::new_v4().simple());
            let call_id = "call-1";
            let text = uuid::Uuid::new_v4().simple().to_string();
            let approvals = Arc::new(InMemoryApprovalStore::new());
            let ledger = InMemoryActionLedger::new();
            let config = RunConfig::builder().approval_store(approvals.clone()).build();
            let scope = ApprovalScope::new("adk-rust", "alice", &session);

            // The first execution parks on the question and holds the call.
            let first = RunContext::new(Some(config.clone()), &session, call_id);
            dispatch(&ledger, first.clone(), &text, Crash::None).await;
            let park = shared.store.get_park(&scope, call_id).await.unwrap().expect("the call parked");
            assert_eq!(adk_core::ToolContext::actions(first.as_ref()).tool_confirmation.as_ref().map(|r| r.fingerprint()), Some(park.fingerprint.clone()));
            approvals.decide(&scope, &park.fingerprint, ToolApproval::approve()).await.unwrap();

            // Every resumption of the held call shares the held request's ledger key.
            let key = adk_agent::held_call_ledger_key("adk-rust", "alice", &session, call_id, &park.fingerprint);
            let mut reached = false;
            for crash in resumes.iter().copied() {
                let ctx = RunContext::keyed(Some(config.clone()), &session, call_id, key.clone());
                reached |= dispatch(&ledger, ctx, &text, crash).await;
            }
            let executions = shared.work.executions().into_iter().filter(|line| *line == text).count();
            (executions, reached)
        });
        let (executions, reached) = executed;
        prop_assert!(executions <= 1, "executed {executions} times");
        prop_assert_eq!(executions, usize::from(reached));
    }
}
