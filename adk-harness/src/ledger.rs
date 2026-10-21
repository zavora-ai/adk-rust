//! The action ledger a harness puts on its runs (spec 3.1 §7.12, §9.2 I37).
//!
//! `bash`, `write_file` and `edit_file` are `NonIdempotent`, so with
//! `RunConfig::action_ledger` set the agent records each call before it executes and
//! answers a re-issued call from its record. The agent keys a record by the call's
//! idempotency key, `{app}/{user}/{session}/{invocation}/{call id}`, and a held call that a
//! later run resumes by `adk_agent::held_call_ledger_key`, which every resumption of the
//! same held call shares, so a call that ran in a run that never saved its result is
//! answered from its record, not run again.

use std::sync::Arc;

use adk_core::ActionLedger;
#[cfg(feature = "runner")]
use adk_core::{ActionOutcome, ActionRecord};
#[cfg(feature = "runner")]
use serde_json::{Value, json};

/// The ledger key every resumption of the held call `call_id` with `fingerprint` shares:
/// the key `LlmAgent` records a resumed call under.
#[cfg(feature = "runner")]
pub(crate) fn held_call_key(
    app: &str,
    user: &str,
    session: &str,
    call_id: &str,
    fingerprint: &str,
) -> String {
    adk_agent::held_call_ledger_key(app, user, session, call_id, fingerprint)
}

/// The function response for a held call that ran but whose result was never saved,
/// from its ledger record; the shapes the agent answers a re-issued call with.
#[cfg(feature = "runner")]
pub(crate) fn recorded_answer(name: &str, record: &ActionRecord) -> Value {
    match &record.outcome {
        Some(ActionOutcome::Succeeded { result_digest }) => json!({
            "status": "already_succeeded",
            "result_digest": result_digest,
            "detail": format!("this call to '{name}' already completed; it was not executed again"),
        }),
        Some(ActionOutcome::Failed { error }) => json!({
            "status": "already_failed",
            "error": error,
            "detail": format!("this call to '{name}' already failed; it was not executed again"),
        }),
        _ => adk_core::outcome_unknown_response(format!(
            "an earlier attempt of this call to '{name}' began but recorded no outcome, so it \
             may have taken effect; it was not executed again"
        )),
    }
}

/// The harness's action ledger and the one each run records in (spec 3.1 §7.12).
#[derive(Clone, Debug)]
pub(crate) struct HarnessLedgers {
    store: Arc<dyn ActionLedger>,
}

impl HarnessLedgers {
    pub(crate) fn new(store: Arc<dyn ActionLedger>) -> Self {
        Self { store }
    }

    /// The ledger records are kept in.
    pub(crate) fn store(&self) -> &Arc<dyn ActionLedger> {
        &self.store
    }

    /// The ledger for a run that carries `existing`: the run's own, else the harness's.
    pub(crate) fn for_run(
        &self,
        existing: Option<&Arc<dyn ActionLedger>>,
    ) -> Arc<dyn ActionLedger> {
        existing.cloned().unwrap_or_else(|| self.store.clone())
    }
}

#[cfg(all(test, feature = "runner"))]
mod tests {
    use super::*;
    use adk_core::{
        ActionOutcome, ActionRecord, HeldToolCall, InMemoryActionLedger, ToolConfirmationRequest,
        ToolEffect,
    };
    use serde_json::json;

    fn held(call_id: &str) -> HeldToolCall {
        let args = json!({ "command": "echo once" });
        HeldToolCall {
            request: ToolConfirmationRequest::new("bash", args.clone())
                .with_function_call_id(call_id),
            id: Some(call_id.to_string()),
            name: "bash".into(),
            args,
        }
    }

    /// The agent keys a resumed call by `adk_agent::held_call_ledger_key`, so the record of
    /// one resumption answers the next without any key rewriting by the harness.
    #[tokio::test]
    async fn every_resumption_of_a_held_call_shares_one_record() {
        let store: Arc<dyn ActionLedger> = Arc::new(InMemoryActionLedger::new());
        let ledger = HarnessLedgers::new(store.clone()).for_run(None);
        let call = held("call-1");
        let fingerprint = call.request.fingerprint();

        let first_run = held_call_key("app", "alice", "s1", "call-1", &fingerprint);
        ledger
            .begin(&ActionRecord::new(&first_run, "bash", &call.args, ToolEffect::NonIdempotent))
            .await
            .unwrap();
        ledger
            .complete(&first_run, ActionOutcome::Succeeded { result_digest: "d".into() })
            .await
            .unwrap();

        let next_run =
            adk_agent::held_call_ledger_key("app", "alice", "s1", "call-1", &fingerprint);
        assert_eq!(next_run, first_run, "every resumption computes the same key");
        let again = ledger.get(&next_run).await.unwrap().unwrap();
        assert!(again.is_completed());
        assert_eq!(again.key, first_run);
        assert!(!next_run.contains("/inv-"), "the key names no invocation: {next_run}");

        let other =
            held_call_key("app", "alice", "s1", "call-1", &held("call-2").request.fingerprint());
        assert_eq!(other, first_run, "the fingerprint ignores the call id");
        let rewritten = held_call_key("app", "alice", "s1", "call-2", &fingerprint);
        assert!(ledger.get(&rewritten).await.unwrap().is_none(), "another call has its own record");
    }

    #[test]
    fn a_run_records_in_its_own_ledger_else_in_the_store() {
        let store: Arc<dyn ActionLedger> = Arc::new(InMemoryActionLedger::new());
        let ledgers = HarnessLedgers::new(store.clone());
        assert!(Arc::ptr_eq(&ledgers.for_run(None), &store));
        let runs: Arc<dyn ActionLedger> = Arc::new(InMemoryActionLedger::new());
        assert!(Arc::ptr_eq(&ledgers.for_run(Some(&runs)), &runs));
        assert!(Arc::ptr_eq(ledgers.store(), &store));
    }
}
