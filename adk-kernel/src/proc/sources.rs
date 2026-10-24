//! What feeds the ledger (spec 3.2 §7.10): the kernel's events, and the audit records of
//! the runtimes it hosts. Each mapping copies labels, counts and durations only; every
//! free-text field of its input — a reason, an argument, a key, a prompt — is either
//! dropped or reduced to a subject the ledger stores as a hash (K-I19).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::audit_log::{AuditDecision, AuditRecordDraft, AuditRecordKind, AuditRecorder};
use adk_core::{DurableRunStore, RunState, ToolConfirmationDecision};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{Activity, KERNEL_AGENT, Ledger, LedgerKind, LedgerSink, label};
use crate::bus::{EventSink, KernelEvent};
use crate::error::Result;
use crate::partition::PartitionId;
use crate::stores::ProcessStore;
use crate::wake::WakeOutcome;

/// The longest label the mappings copy from an identifier.
const LABEL: usize = 128;

/// A run state's wire name, such as `running`.
fn state_label(state: RunState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// The row a [`KernelEvent`] becomes (spec 3.2 §7.10), or `None` for a `Health` report.
///
/// The row names the kernel as its agent and the default partition; the kernel's own sink
/// replaces both with the process's when the event names one, and adds how long a process
/// ran when it leaves `running`. Reasons are dropped, and pids, wake sources, fingerprints,
/// rule ids and observation ids become the row's subject, stored as a hash.
///
/// | Event | Kind | Effect | Subject | Meta |
/// |---|---|---|---|---|
/// | `Process` | `process` | the state entered | the pid | |
/// | `Wake` | `wake` | `woke`, `spawned`, `deduplicated`, `expired` or `unmatched` | the source | |
/// | `Held` | `held` | | the call's fingerprint | |
/// | `Decided` | `decided` | `approve` or `deny` | the fingerprint | |
/// | `RuleHit` | `rule_hit` | | the rule id | |
/// | `Spend` | `spend` | | | `micro_usd` |
/// | `Agenda` | `proposal` | the state the item entered | the item id | |
/// | `Review` | `audit` | `review` | | `rules`, `sleepers`, `frozen` |
/// | `Driver` | `audit` | `driver.` and the state entered, such as `driver.crashed` | the driver id | |
/// | `Observation` | `observation` | | the observation id | |
/// | `Governance` | `governance` | `frozen` or `unfrozen` | | |
///
/// # Example
///
/// ```rust
/// use adk_core::RunState;
/// use adk_kernel::KernelEvent;
/// use adk_kernel::proc::{LedgerKind, activity_for_event};
///
/// let event = KernelEvent::Process {
///     pid: "run-1".into(),
///     from: RunState::Running,
///     to: RunState::Failed,
///     reason: Some("the tool printed the API key sk-live-1".into()),
/// };
/// let activity = activity_for_event(&event).unwrap();
/// assert_eq!(activity.kind, LedgerKind::Process);
/// assert_eq!(activity.effect.as_deref(), Some("failed"));
/// assert_eq!(activity.subject.as_deref(), Some("run-1"));
/// ```
pub fn activity_for_event(event: &KernelEvent) -> Option<Activity> {
    let kernel = |kind| Activity::new(kind, KERNEL_AGENT);
    let activity = match event {
        KernelEvent::Process { pid, to, .. } => {
            kernel(LedgerKind::Process).with_effect(state_label(*to)).with_subject(pid.clone())
        }
        KernelEvent::Wake { source, outcome, .. } => {
            let effect = match outcome {
                Some(WakeOutcome::Woke { .. }) => "woke",
                Some(WakeOutcome::Spawned { .. }) => "spawned",
                Some(WakeOutcome::Deduplicated { .. }) => "deduplicated",
                Some(WakeOutcome::Expired { .. }) => "expired",
                None => "unmatched",
            };
            kernel(LedgerKind::Wake).with_effect(effect).with_subject(source.clone())
        }
        KernelEvent::Held { request, .. } => {
            kernel(LedgerKind::Held).with_subject(request.fingerprint())
        }
        KernelEvent::Decided { fingerprint, decision, .. } => {
            let effect = match decision {
                ToolConfirmationDecision::Approve => "approve",
                ToolConfirmationDecision::Deny => "deny",
            };
            kernel(LedgerKind::Decided).with_effect(effect).with_subject(fingerprint.clone())
        }
        KernelEvent::RuleHit { rule_id, .. } => {
            kernel(LedgerKind::RuleHit).with_subject(rule_id.clone())
        }
        KernelEvent::Spend { partition, key, delta } => {
            let agent =
                key.agent.as_deref().map_or_else(|| KERNEL_AGENT.into(), |a| label(a, LABEL));
            Activity::new(LedgerKind::Spend, agent)
                .with_partition(partition.clone())
                .with_count("micro_usd", delta.micro_usd)
        }
        KernelEvent::Agenda { item, to, .. } => kernel(LedgerKind::Proposal)
            .with_effect(label(&to.kind().to_string(), 64))
            .with_subject(item.clone()),
        KernelEvent::Review(report) => kernel(LedgerKind::Audit)
            .with_partition(report.partition.clone())
            .with_effect("review")
            .with_count("rules", report.rules.len() as u64)
            .with_count("sleepers", report.sleepers.len() as u64)
            .with_count("frozen", report.frozen.len() as u64),
        KernelEvent::Driver { id, state, .. } => kernel(LedgerKind::Audit)
            .with_effect(format!("driver.{}", state.as_str()))
            .with_subject(id.clone()),
        KernelEvent::Observation { id, partition } => kernel(LedgerKind::Observation)
            .with_partition(partition.clone())
            .with_subject(id.clone()),
        KernelEvent::Governance { frozen, .. } => {
            kernel(LedgerKind::Governance).with_effect(if *frozen { "frozen" } else { "unfrozen" })
        }
        KernelEvent::Health(_) => return None,
    };
    Some(activity)
}

/// The process an event concerns, whose partition and agent its row takes.
fn event_pid(event: &KernelEvent) -> Option<&str> {
    match event {
        KernelEvent::Process { pid, .. }
        | KernelEvent::Held { pid, .. }
        | KernelEvent::Decided { pid, .. }
        | KernelEvent::RuleHit { pid, .. } => Some(pid),
        KernelEvent::Wake { pid, .. } => pid.as_deref(),
        KernelEvent::Spend { .. }
        | KernelEvent::Agenda { .. }
        | KernelEvent::Review(_)
        | KernelEvent::Driver { .. }
        | KernelEvent::Observation { .. }
        | KernelEvent::Governance { .. }
        | KernelEvent::Health(_) => None,
    }
}

/// A number among an audit record's extension fields.
fn ext_count(draft: &AuditRecordDraft, field: &str) -> Option<u64> {
    draft.ext.get(field).and_then(Value::as_u64)
}

/// The row an audit record becomes (spec 3.2 §7.10), or `None` when another source writes
/// it: an allowed tool call (the harness writes its `tool_call` row with the duration) and
/// the kernel's own `kernel.*` and `governance.*` records (its events write those).
///
/// Every row keeps the actor's agent and invocation and, as its subject, the action and
/// the arguments' digest; reasons and every other extension field but the counts below are
/// dropped.
///
/// | Record | Kind | Effect | Meta |
/// |---|---|---|---|
/// | `tool_call`, decision `denied` | `denial` | the tool's effect class | |
/// | `tool_call`, decision `approval_required` | `held` | the tool's effect class | |
/// | `approval`, decision `approved` / `rejected` | `approval` / `rejection` | | |
/// | `harness.spend` | `turn` | | `model_calls`, `tool_calls`, `tokens`, `prompt_tokens`, `cached_tokens`, `cost_micro_usd` |
/// | `harness.loop.detected` | `loop_guard` | | `identical_calls`, `redundant_calls` |
/// | any other record | `audit` | its action, such as `harness.mode.changed` | |
///
/// # Example
///
/// ```rust
/// use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind};
/// use adk_kernel::proc::{LedgerKind, activity_for_audit};
///
/// let draft = AuditRecordDraft::new(AuditRecordKind::Other("harness".into()), "harness.spend")
///     .ext("modelCalls", serde_json::json!(3))
///     .ext("cost", serde_json::json!(1250));
/// let activity = activity_for_audit(&draft).unwrap();
/// assert_eq!(activity.kind, LedgerKind::Turn);
/// assert_eq!(activity.meta["cost_micro_usd"], 1250);
/// ```
pub fn activity_for_audit(draft: &AuditRecordDraft) -> Option<Activity> {
    let effect = || draft.ext.get("effect").and_then(Value::as_str).map(|effect| label(effect, 64));
    let (kind, effect) = match &draft.kind {
        AuditRecordKind::ToolCall => match draft.decision {
            Some(AuditDecision::Denied) => (LedgerKind::Denial, effect()),
            Some(AuditDecision::ApprovalRequired) => (LedgerKind::Held, effect()),
            _ => return None,
        },
        AuditRecordKind::Approval => match draft.decision {
            Some(AuditDecision::Approved) => (LedgerKind::Approval, None),
            Some(AuditDecision::Rejected) => (LedgerKind::Rejection, None),
            _ => return None,
        },
        AuditRecordKind::Governance => return None,
        AuditRecordKind::Other(kind) if kind == crate::audit::KERNEL_AUDIT_KIND => return None,
        AuditRecordKind::Other(_) => match draft.action.as_str() {
            "harness.spend" => (LedgerKind::Turn, None),
            "harness.loop.detected" => (LedgerKind::LoopGuard, None),
            action => (LedgerKind::Audit, Some(label(action, 64))),
        },
        other => (LedgerKind::Audit, Some(label(other.as_str(), 64))),
    };
    let agent = draft
        .actor
        .agent
        .as_ref()
        .map_or_else(|| "unknown".to_string(), |agent| label(agent.as_str(), LABEL));
    let mut activity = Activity::new(kind.clone(), agent).with_subject(format!(
        "{}\u{1f}{}",
        draft.action,
        draft.args_digest.as_deref().unwrap_or_default()
    ));
    if let Some(effect) = effect {
        activity = activity.with_effect(effect);
    }
    if let Some(invocation) = &draft.actor.invocation_id
        && (1..=64).contains(&invocation.len())
        && invocation.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        activity = activity.with_trace_id(invocation.clone());
    }
    let counts: &[(&str, &str)] = match kind {
        LedgerKind::Turn => &[
            ("modelCalls", "model_calls"),
            ("toolCalls", "tool_calls"),
            ("tokens", "tokens"),
            ("promptTokens", "prompt_tokens"),
            ("cachedTokens", "cached_tokens"),
            ("cost", "cost_micro_usd"),
        ],
        LedgerKind::LoopGuard => {
            &[("identicalCalls", "identical_calls"), ("redundantCalls", "redundant_calls")]
        }
        _ => &[],
    };
    for (field, key) in counts {
        if let Some(count) = ext_count(draft, field) {
            activity = activity.with_count(*key, count);
        }
    }
    Some(activity)
}

/// An `AuditRecorder` that also writes each record's ledger row (spec 3.2 §7.10): the
/// harness wraps its audit log in one, so its spend, loop-guard, approval and other audit
/// records feed the ledger. The record is written first; a ledger failure is logged and
/// never fails the record.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, AuditRecorder, InMemoryAuditRecorder};
/// use adk_kernel::proc::{InMemoryLedgerStore, Ledger, LedgerAuditTap, LedgerKey, LedgerQuery, LedgerStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let store = Arc::new(InMemoryLedgerStore::new());
/// let ledger = Arc::new(Ledger::new(store.clone(), LedgerKey::from_bytes([1; 32])));
/// let records = Arc::new(InMemoryAuditRecorder::new());
/// let tap = LedgerAuditTap::new(records.clone(), ledger);
/// tap.record(AuditRecordDraft::new(AuditRecordKind::Other("harness".into()), "harness.mode.changed")).await?;
/// assert_eq!(records.records().len(), 1);
/// assert_eq!(store.list(&LedgerQuery::default()).await?[0].kind, "audit");
/// # Ok(())
/// # }
/// ```
pub struct LedgerAuditTap {
    inner: Arc<dyn AuditRecorder>,
    sink: Arc<dyn LedgerSink>,
}

impl std::fmt::Debug for LedgerAuditTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LedgerAuditTap").finish_non_exhaustive()
    }
}

impl LedgerAuditTap {
    /// Records to `inner` and writes each record's row to `sink`.
    pub fn new(inner: Arc<dyn AuditRecorder>, sink: Arc<dyn LedgerSink>) -> Self {
        Self { inner, sink }
    }

    /// The recorder records go to.
    pub fn inner(&self) -> &Arc<dyn AuditRecorder> {
        &self.inner
    }
}

#[async_trait]
impl AuditRecorder for LedgerAuditTap {
    fn digest(&self, value: &Value) -> String {
        self.inner.digest(value)
    }

    async fn record(&self, draft: AuditRecordDraft) -> adk_core::Result<()> {
        let activity = activity_for_audit(&draft);
        self.inner.record(draft).await?;
        if let Some(activity) = activity
            && let Err(error) = self.sink.record(activity).await
        {
            tracing::warn!(error = %error, "could not write an audit record's ledger row");
        }
        Ok(())
    }
}

/// Most processes the kernel's sink remembers the partition, agent and start of.
const REMEMBERED: usize = 4096;

#[derive(Default)]
struct Remembered {
    /// The partition and agent of each process.
    owners: HashMap<String, (PartitionId, String)>,
    /// When each process entered `running`.
    running: HashMap<String, DateTime<Utc>>,
}

/// The kernel's event sink that writes the ledger (spec 3.2 §7.10): each event's row from
/// [`activity_for_event`], with the process's partition and agent, and the time a process
/// ran when it leaves `running`.
pub(crate) struct LedgerEventSink {
    ledger: Ledger,
    runs: Arc<dyn DurableRunStore>,
    processes: Arc<dyn ProcessStore>,
    remembered: Mutex<Remembered>,
}

impl LedgerEventSink {
    pub(crate) fn new(
        ledger: Ledger,
        runs: Arc<dyn DurableRunStore>,
        processes: Arc<dyn ProcessStore>,
    ) -> Self {
        Self { ledger, runs, processes, remembered: Mutex::default() }
    }

    /// The partition and agent of `pid`: its side-table partition, and the kernel for its
    /// own targets, else the acting agent's id or the run's target.
    async fn owner(&self, pid: &str) -> (PartitionId, String) {
        if let Some(owner) =
            self.remembered.lock().unwrap_or_else(|e| e.into_inner()).owners.get(pid)
        {
            return owner.clone();
        }
        let run = self.runs.get(pid).await.ok().flatten();
        let partition = match self.processes.get(pid).await {
            Ok(Some(meta)) => meta.partition,
            _ => PartitionId::default(),
        };
        let agent = match &run {
            Some(run) if run.target.starts_with("kernel.") => KERNEL_AGENT.to_string(),
            Some(run) => match &run.principal {
                Some(principal) => label(principal.agent.id.as_str(), LABEL),
                None => label(&run.target, LABEL),
            },
            None => KERNEL_AGENT.to_string(),
        };
        let owner = (partition, agent);
        if run.is_some() {
            let mut remembered = self.remembered.lock().unwrap_or_else(|e| e.into_inner());
            if remembered.owners.len() >= REMEMBERED {
                remembered.owners.clear();
            }
            remembered.owners.insert(pid.to_string(), owner.clone());
        }
        owner
    }
}

#[async_trait]
impl EventSink for LedgerEventSink {
    async fn emit(&self, event: &KernelEvent) -> Result<()> {
        let Some(mut activity) = activity_for_event(event) else {
            return Ok(());
        };
        if let Some(pid) = event_pid(event) {
            let (partition, agent) = self.owner(pid).await;
            activity.partition = partition;
            if !matches!(event, KernelEvent::Wake { .. }) {
                activity.agent = agent;
            }
        }
        if let KernelEvent::Process { pid, from, to, .. } = event {
            let now = self.ledger.clock().now();
            let mut remembered = self.remembered.lock().unwrap_or_else(|e| e.into_inner());
            if *to == RunState::Running {
                if remembered.running.len() >= REMEMBERED {
                    remembered.running.clear();
                }
                remembered.running.insert(pid.clone(), now);
            } else if *from == RunState::Running
                && let Some(started) = remembered.running.remove(pid)
            {
                activity = activity.with_duration((now - started).to_std().unwrap_or_default());
            }
            if to.is_terminal() {
                remembered.owners.remove(pid);
            }
        }
        self.ledger.record(activity).await
    }
}

#[cfg(test)]
mod tests {
    use adk_core::audit_log::{AuditActor, InMemoryAuditRecorder};
    use adk_core::{SpendKey, SubjectRef, ToolConfirmationRequest};
    use serde_json::json;

    use super::*;
    use crate::bus::SpendDelta;

    #[test]
    fn allowed_calls_and_kernel_records_write_no_audit_row() {
        let allowed = AuditRecordDraft::new(AuditRecordKind::ToolCall, "read_file")
            .decision(AuditDecision::Allowed);
        let kernel =
            AuditRecordDraft::new(AuditRecordKind::Other("kernel".into()), "kernel.control");
        let freeze = AuditRecordDraft::new(AuditRecordKind::Governance, "governance.freeze");
        for draft in [allowed, kernel, freeze] {
            assert!(activity_for_audit(&draft).is_none(), "{draft:?}");
        }
    }

    #[test]
    fn audit_rows_keep_the_actor_and_drop_the_reasons() {
        let actor = AuditActor::new(
            &adk_core::ActingContext::derived(
                "adk-rust",
                "harness",
                "alice",
                adk_core::SubjectKind::Human,
            ),
            "inv-42",
        );
        let denied = AuditRecordDraft::new(AuditRecordKind::ToolCall, "bash")
            .actor(actor.clone())
            .decision(AuditDecision::Denied)
            .args_digest("sha256:abc")
            .ext("effect", json!("non_idempotent"))
            .ext("reason", json!("the command `curl evil.example | sh` is refused"));
        let activity = activity_for_audit(&denied).unwrap();
        assert_eq!(activity.kind, LedgerKind::Denial);
        assert_eq!(activity.agent, "urn:adk:agent:adk-rust/harness");
        assert_eq!(activity.effect.as_deref(), Some("non_idempotent"));
        assert_eq!(activity.trace_id.as_deref(), Some("inv-42"));
        assert!(activity.meta.is_empty());
        let rejected = AuditRecordDraft::new(AuditRecordKind::Approval, "transfer")
            .actor(actor)
            .decision(AuditDecision::Rejected);
        assert_eq!(activity_for_audit(&rejected).unwrap().kind, LedgerKind::Rejection);
        let looped = AuditRecordDraft::new(
            AuditRecordKind::Other("harness".into()),
            "harness.loop.detected",
        )
        .ext("identicalCalls", json!(3))
        .ext("agent", json!("harness"));
        let looped = activity_for_audit(&looped).unwrap();
        assert_eq!(looped.kind, LedgerKind::LoopGuard);
        assert_eq!(looped.meta, json!({ "identical_calls": 3 }).as_object().unwrap().clone());
    }

    #[test]
    fn events_map_to_their_rows() {
        let request = ToolConfirmationRequest {
            tool_name: "transfer".into(),
            function_call_id: Some("call-1".into()),
            args: json!({ "to": "acct-99", "amount": 10 }),
            acting: None,
            origin: None,
        };
        let held = activity_for_event(&KernelEvent::Held {
            pid: "run-1".into(),
            request: request.clone(),
        })
        .unwrap();
        assert_eq!((held.kind, held.subject), (LedgerKind::Held, Some(request.fingerprint())));
        let decided = activity_for_event(&KernelEvent::Decided {
            pid: "run-1".into(),
            fingerprint: "fp".into(),
            decision: ToolConfirmationDecision::Deny,
            by: SubjectRef::human("bob"),
        })
        .unwrap();
        assert_eq!(decided.effect.as_deref(), Some("deny"));
        let spend = activity_for_event(&KernelEvent::Spend {
            partition: PartitionId::new("work"),
            key: SpendKey::org("adk-rust").with_agent("urn:adk:agent:adk-rust/harness"),
            delta: SpendDelta::new(1500),
        })
        .unwrap();
        assert_eq!(spend.partition, PartitionId::new("work"));
        assert_eq!(spend.meta["micro_usd"], 1500);
        let frozen = activity_for_event(&KernelEvent::Governance {
            frozen: true,
            reason: Some("incident 12: leaked token".into()),
        })
        .unwrap();
        assert_eq!(frozen.effect.as_deref(), Some("frozen"));
        let stale = activity_for_event(&KernelEvent::Driver {
            id: "notes".into(),
            state: crate::drivers::DriverState::NeedsReapproval,
            reason: Some("read_note now forwards the note".into()),
        })
        .unwrap();
        assert_eq!(
            (stale.kind, stale.agent.as_str(), stale.effect.as_deref()),
            (LedgerKind::Audit, KERNEL_AGENT, Some("driver.needsReapproval"))
        );
        assert_eq!(stale.subject.as_deref(), Some("notes"));
        assert!(activity_for_event(&KernelEvent::Health(Default::default())).is_none());
    }

    #[tokio::test]
    async fn the_tap_records_first_and_never_fails_on_the_ledger() {
        struct Failing;
        #[async_trait]
        impl LedgerSink for Failing {
            async fn record(&self, _activity: Activity) -> Result<()> {
                Err(crate::KernelError::Config { reason: "down".into() })
            }
        }
        let records = Arc::new(InMemoryAuditRecorder::new());
        let tap = LedgerAuditTap::new(records.clone(), Arc::new(Failing));
        tap.record(AuditRecordDraft::new(
            AuditRecordKind::Other("harness".into()),
            "harness.spend",
        ))
        .await
        .unwrap();
        assert_eq!(records.records().len(), 1);
    }
}
