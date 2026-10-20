//! The action ledger a harness puts on its runs (spec 3.1 §7.12, §9.2 I37).
//!
//! `bash`, `write_file` and `edit_file` are `NonIdempotent`, so with
//! `RunConfig::action_ledger` set the agent records each call before it executes and
//! answers a re-issued call from its record. The agent keys a record by the call's
//! idempotency key, `{app}/{user}/{session}/{invocation}/{call id}`, and a held call that a
//! later run resumes executes under that later run's invocation. A resumed call's key is
//! therefore mapped to one every resumption of the same held call shares, so a call that
//! ran in a run that never saved its result is answered from its record, not run again.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use adk_core::{ActionLedger, ActionOutcome, ActionRecord, HeldToolCall, Result, async_trait};
#[cfg(feature = "runner")]
use serde_json::{Value, json};

/// Upper bound of remembered resumed-call keys; the oldest are dropped first.
const RESUMED_CAP: usize = 4096;
/// Upper bound of wrappers kept for ledgers that runs carry.
const WRAPPED_CAP: usize = 16;

/// The ledger key every resumption of the held call `call_id` with `fingerprint` shares.
pub(crate) fn held_call_key(
    app: &str,
    user: &str,
    session: &str,
    call_id: &str,
    fingerprint: &str,
) -> String {
    let digest = crate::fsutil::hex_digest(fingerprint.as_bytes());
    format!("{app}/{user}/{session}/held/{call_id}/{digest}")
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

/// Idempotency keys of resumed held calls, mapped to their shared key.
#[derive(Debug, Default)]
struct ResumedKeys {
    keys: HashMap<String, String>,
    order: VecDeque<String>,
}

impl ResumedKeys {
    fn insert(&mut self, run_key: String, shared: String) {
        if self.keys.insert(run_key.clone(), shared).is_none() {
            self.order.push_back(run_key);
        }
        while self.order.len() > RESUMED_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.keys.remove(&oldest);
            }
        }
    }
}

/// An [`ActionLedger`] that stores in `inner` under the shared key of a resumed call.
#[derive(Debug)]
struct HarnessLedger {
    inner: Arc<dyn ActionLedger>,
    resumed: Arc<Mutex<ResumedKeys>>,
}

impl HarnessLedger {
    fn key(&self, key: &str) -> String {
        let resumed = self.resumed.lock().unwrap_or_else(|e| e.into_inner());
        resumed.keys.get(key).cloned().unwrap_or_else(|| key.to_string())
    }
}

#[async_trait]
impl ActionLedger for HarnessLedger {
    async fn begin(&self, record: &ActionRecord) -> Result<()> {
        let key = self.key(&record.key);
        if key == record.key {
            return self.inner.begin(record).await;
        }
        let mut record = record.clone();
        record.key = key;
        self.inner.begin(&record).await
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()> {
        self.inner.complete(&self.key(key), outcome).await
    }

    async fn get(&self, key: &str) -> Result<Option<ActionRecord>> {
        self.inner.get(&self.key(key)).await
    }
}

/// Ledgers runs carried, each with the wrapper the harness puts on those runs.
type Wrapped = Vec<(Arc<dyn ActionLedger>, Arc<dyn ActionLedger>)>;

/// The harness's action ledger and the wrappers it puts on runs (spec 3.1 §7.12).
#[derive(Clone, Debug)]
pub(crate) struct HarnessLedgers {
    store: Arc<dyn ActionLedger>,
    own: Arc<dyn ActionLedger>,
    resumed: Arc<Mutex<ResumedKeys>>,
    wrapped: Arc<Mutex<Wrapped>>,
}

impl HarnessLedgers {
    pub(crate) fn new(store: Arc<dyn ActionLedger>) -> Self {
        let resumed = Arc::new(Mutex::new(ResumedKeys::default()));
        let own: Arc<dyn ActionLedger> =
            Arc::new(HarnessLedger { inner: store.clone(), resumed: resumed.clone() });
        Self { store, own, resumed, wrapped: Arc::new(Mutex::new(Vec::new())) }
    }

    /// The ledger records are kept in.
    pub(crate) fn store(&self) -> &Arc<dyn ActionLedger> {
        &self.store
    }

    /// The ledger for a run that carries `existing`: the harness's own when it carries
    /// none, otherwise the run's own, still storing in it, with resumed calls keyed alike.
    pub(crate) fn for_run(
        &self,
        existing: Option<&Arc<dyn ActionLedger>>,
    ) -> Arc<dyn ActionLedger> {
        let Some(existing) = existing else {
            return self.own.clone();
        };
        if Arc::ptr_eq(existing, &self.own) {
            return existing.clone();
        }
        let mut wrapped = self.wrapped.lock().unwrap_or_else(|e| e.into_inner());
        for (inner, wrapper) in wrapped.iter() {
            if Arc::ptr_eq(existing, wrapper) {
                return existing.clone();
            }
            if Arc::ptr_eq(existing, inner) {
                return wrapper.clone();
            }
        }
        let wrapper: Arc<dyn ActionLedger> =
            Arc::new(HarnessLedger { inner: existing.clone(), resumed: self.resumed.clone() });
        wrapped.push((existing.clone(), wrapper.clone()));
        if wrapped.len() > WRAPPED_CAP {
            wrapped.remove(0);
        }
        wrapper
    }

    /// Notes the calls a run of `invocation` may resume, so each is ledgered under its
    /// held call's shared key.
    pub(crate) fn note_resumable(
        &self,
        (app, user, session): (&str, &str, &str),
        invocation: &str,
        held: &[HeldToolCall],
    ) {
        if held.is_empty() {
            return;
        }
        let mut resumed = self.resumed.lock().unwrap_or_else(|e| e.into_inner());
        for call in held {
            let Some(call_id) = call.id.as_deref() else {
                continue;
            };
            resumed.insert(
                format!("{app}/{user}/{session}/{invocation}/{call_id}"),
                held_call_key(app, user, session, call_id, &call.request.fingerprint()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{InMemoryActionLedger, ToolConfirmationRequest, ToolEffect};
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

    #[tokio::test]
    async fn every_resumption_of_a_held_call_shares_one_record() {
        let store: Arc<dyn ActionLedger> = Arc::new(InMemoryActionLedger::new());
        let ledgers = HarnessLedgers::new(store.clone());
        let ledger = ledgers.for_run(None);
        let identity = ("app", "alice", "s1");
        let call = held("call-1");

        ledgers.note_resumable(identity, "inv-2", std::slice::from_ref(&call));
        let key = "app/alice/s1/inv-2/call-1";
        ledger
            .begin(&ActionRecord::new(key, "bash", &call.args, ToolEffect::NonIdempotent))
            .await
            .unwrap();
        ledger.complete(key, ActionOutcome::Succeeded { result_digest: "d".into() }).await.unwrap();

        ledgers.note_resumable(identity, "inv-3", std::slice::from_ref(&call));
        let again = ledger.get("app/alice/s1/inv-3/call-1").await.unwrap().unwrap();
        assert!(again.is_completed());
        let shared = held_call_key("app", "alice", "s1", "call-1", &call.request.fingerprint());
        assert_eq!(again.key, shared);
        assert!(store.get(key).await.unwrap().is_none(), "stored under the shared key only");
        assert!(ledger.get("app/alice/s1/inv-3/call-2").await.unwrap().is_none());
    }

    #[test]
    fn a_runs_own_ledger_is_wrapped_once_and_kept_as_the_store() {
        let ledgers = HarnessLedgers::new(Arc::new(InMemoryActionLedger::new()));
        let own = ledgers.for_run(None);
        assert!(Arc::ptr_eq(&ledgers.for_run(Some(&own)), &own));
        let runs: Arc<dyn ActionLedger> = Arc::new(InMemoryActionLedger::new());
        let wrapped = ledgers.for_run(Some(&runs));
        assert!(!Arc::ptr_eq(&wrapped, &runs));
        assert!(Arc::ptr_eq(&ledgers.for_run(Some(&wrapped)), &wrapped));
        assert!(Arc::ptr_eq(&ledgers.for_run(Some(&runs)), &wrapped));
    }
}
