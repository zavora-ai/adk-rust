//! AG-UI decisions (spec 3.3 §10 S2): a call held for approval ends the run with an AG-UI
//! interrupt, and an AG-UI resume becomes [`ApprovalService::decide`] on the inbox entry.
//!
//! The shapes follow the AG-UI interrupt-aware run lifecycle (AG-UI 1.1 draft,
//! <https://docs.ag-ui.com/concepts/interrupts>): `RUN_FINISHED` carries
//! `outcome: {"type": "interrupt", "interrupts": [...]}`, each interrupt bound to its tool
//! call with `reason: "tool_call"` and `toolCallId`, and the next run's input answers them in
//! `resume: [{"interruptId", "status", "payload"}]`.

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalAction, ApprovalDecision, ApprovalRequest, ApprovalScope, ApprovalService,
    ApprovalState, ErrorCategory, ToolConfirmationRequest,
};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::runtime::{AgUiResumeEntry, AgUiResumeStatus};

/// The `CUSTOM` event name of a decision update.
pub(crate) const DECISION_EVENT: &str = "adk.decision";

/// The AG-UI interrupt reason for an interrupt bound to one tool call.
pub(crate) const TOOL_CALL_REASON: &str = "tool_call";

/// A call the run held for approval, as the stream saw it.
#[derive(Debug, Clone)]
pub(crate) struct HeldCall {
    pub(crate) request: ToolConfirmationRequest,
    /// The agent that held the call.
    pub(crate) agent: String,
}

/// What a protocol-native AG-UI stream needs to turn held calls into interrupts.
#[derive(Clone, Default)]
pub(crate) struct AgUiDecisions {
    /// The inbox held calls are routed to; without one an interrupt names the call ID.
    pub(crate) service: Option<Arc<dyn ApprovalService>>,
    /// The session the run belongs to.
    pub(crate) scope: Option<ApprovalScope>,
    /// Whether the caller authenticated as the session's owner, so an interrupt may carry
    /// the call's arguments.
    pub(crate) show_arguments: bool,
    /// `adk.decision` values for the resume entries this request answered, sent after
    /// `RUN_STARTED`.
    pub(crate) updates: Vec<Value>,
}

impl AgUiDecisions {
    /// The interrupts for `held`, in order.
    pub(crate) async fn interrupts(&self, held: &[HeldCall]) -> Vec<Value> {
        let mut interrupts = Vec::with_capacity(held.len());
        for call in held {
            let entry = self.inbox_entry(&call.request).await;
            interrupts.push(interrupt(call, entry.as_ref(), self.show_arguments));
        }
        interrupts
    }

    /// The inbox entry the governed path submitted for `request`, found as
    /// `StoreApprovalService::submit_call` names it.
    async fn inbox_entry(&self, request: &ToolConfirmationRequest) -> Option<ApprovalRequest> {
        let (service, scope) = (self.service.as_ref()?, self.scope.as_ref()?);
        let store = service.approval_store()?;
        let fingerprint = request.fingerprint();
        let pending = match store.pending(scope).await {
            Ok(pending) => pending,
            Err(error) => {
                warn!(error = %error, "could not read the held calls for the AG-UI interrupt");
                return None;
            }
        };
        let held = pending.into_iter().find(|entry| entry.fingerprint == fingerprint)?;
        let id = ApprovalRequest::id_for_call(scope, &fingerprint, held.requested_at);
        match service.get(&id).await {
            Ok(entry) => entry,
            Err(error) => {
                warn!(error = %error, approval.id = %id, "could not read the inbox entry for the AG-UI interrupt");
                None
            }
        }
    }
}

/// One AG-UI `Interrupt` for a held call: the inbox entry's ID when the call has one, else
/// the call ID; the arguments only for the session's owner.
///
/// A call with no inbox entry — no approval service, or one that leaves the call held in its
/// store only — cannot be decided here, so its interrupt carries no `responseSchema` and
/// `metadata.answerable: false` (spec 3.3 S-I1).
fn interrupt(call: &HeldCall, entry: Option<&ApprovalRequest>, show_arguments: bool) -> Value {
    let request = &call.request;
    let call_id =
        request.function_call_id.clone().unwrap_or_else(|| format!("{}-call", request.tool_name));
    let summary = entry.map_or_else(
        || format!("Approve a call to '{}'", request.tool_name),
        |entry| entry.summary.clone(),
    );
    // The agent the governed path resolved, as the inbox records it; the hold event's own
    // author only when there is no entry.
    let agent = entry
        .and_then(|entry| entry.acting.as_ref())
        .map_or_else(|| call.agent.clone(), |acting| acting.agent.id.to_string());
    let mut metadata = json!({
        "summary": summary,
        "agent": agent,
        "tool": request.tool_name,
        "answerable": entry.is_some(),
    });
    if let Some(entry) = entry {
        metadata["risk"] = json!(entry.risk);
        metadata["version"] = json!(entry.version);
    }
    if show_arguments {
        metadata["arguments"] =
            entry.map_or_else(|| request.args.clone(), |entry| entry.args_redacted.clone());
    }
    let mut interrupt = json!({
        "id": entry.map_or_else(|| call_id.clone(), |entry| entry.id.clone()),
        "reason": TOOL_CALL_REASON,
        "message": summary,
        "toolCallId": call_id,
        "metadata": metadata,
    });
    if let Some(entry) = entry {
        let allows = |action: ApprovalAction| entry.allowed_actions.contains(&action);
        interrupt["responseSchema"] =
            response_schema(allows(ApprovalAction::Edit), allows(ApprovalAction::Reject));
        if let Some(at) = entry.expires_at {
            interrupt["expiresAt"] = json!(at.to_rfc3339());
        }
    }
    interrupt
}

/// The answer an interrupt accepts: AG-UI's recommended approval shape.
fn response_schema(edit: bool, reject: bool) -> Value {
    let mut properties = json!({ "approved": { "type": "boolean" } });
    if edit {
        properties["editedArgs"] = json!({ "type": "object" });
    }
    if reject {
        properties["reason"] = json!({ "type": "string" });
    }
    json!({ "type": "object", "properties": properties, "required": ["approved"] })
}

/// An `adk.decision` value: where the inbox entry `interrupt_id` stands, with the version to
/// answer it at, or `unknown` when the session has no such entry.
pub(crate) fn decision_update(interrupt_id: &str, entry: Option<&ApprovalRequest>) -> Value {
    let Some(entry) = entry else {
        return json!({ "interruptId": interrupt_id, "state": "unknown" });
    };
    let mut value = json!({
        "interruptId": interrupt_id,
        "state": entry.state.as_str(),
        "tool": entry.tool_name,
        "version": entry.version,
    });
    if let Some(decision) = entry.decisions.last() {
        value["decidedBy"] = json!(decision.authority().id);
        value["decidedAt"] = json!(decision.decided_at.to_rfc3339());
    }
    if let Some(at) = entry.resolved_at {
        value["resolvedAt"] = json!(at.to_rfc3339());
    }
    value
}

/// A decision update that also says why this answer was not recorded.
fn refused_update(interrupt_id: &str, entry: &ApprovalRequest, code: &str, message: &str) -> Value {
    let mut value = decision_update(interrupt_id, Some(entry));
    value["code"] = json!(code);
    value["message"] = json!(message);
    value
}

/// A `CUSTOM` event carrying a decision update.
pub(crate) fn decision_event(value: Value) -> Value {
    json!({
        "type": "CUSTOM",
        "name": DECISION_EVENT,
        "value": value,
        "timestamp": chrono::Utc::now().timestamp_millis().max(0),
    })
}

/// What reading the resume entries found, before anything is decided.
#[derive(Debug)]
pub(crate) enum ResumeOutcome {
    /// Every answer may be recorded: start the run, recording them first with
    /// [`commit_resume`].
    Run(ResumePlan),
    /// Do not run and decide nothing: an answer would be refused. Send these updates, then a
    /// `RUN_ERROR`.
    Refused {
        /// Decision updates, the refused entry's last.
        updates: Vec<Value>,
        /// The refusal's code, such as `approval.version_conflict`.
        code: &'static str,
        /// What happened, in plain words.
        message: String,
    },
}

/// The answers of a resume that may all be recorded, in the order the client sent them.
#[derive(Debug, Default)]
pub(crate) struct ResumePlan {
    steps: Vec<Step>,
}

#[derive(Debug)]
enum Step {
    /// Record `decision` on the entry at `version`.
    Decide {
        interrupt_id: String,
        request: Box<ApprovalRequest>,
        version: u64,
        decision: Box<ApprovalDecision>,
    },
    /// Nothing to record; report this update.
    Report(Value),
}

/// The decision `entry` answers with, from its payload.
fn decision_for(
    entry: &AgUiResumeEntry,
    approver: &SubjectRef,
    groups: &[String],
) -> Result<ApprovalDecision, String> {
    let invalid = || {
        format!(
            "resume entry '{}' needs a payload such as {{\"approved\": true}}, {{\"approved\": \
             true, \"editedArgs\": {{...}}}} or {{\"approved\": false, \"reason\": \"...\"}}",
            entry.interrupt_id
        )
    };
    let payload = entry.payload.as_ref().and_then(Value::as_object).ok_or_else(invalid)?;
    let approved = payload.get("approved").and_then(Value::as_bool).ok_or_else(invalid)?;
    let reason = payload.get("reason").and_then(Value::as_str).filter(|reason| !reason.is_empty());
    let decision = match (approved, payload.get("editedArgs")) {
        (true, None | Some(Value::Null)) => ApprovalDecision::approve(approver.clone()),
        (true, Some(args @ Value::Object(_))) => {
            ApprovalDecision::edit(approver.clone(), args.clone())
        }
        (true, Some(_)) => {
            return Err(format!(
                "resume entry '{}': `editedArgs` must be an object, the call's full replacement \
                 arguments",
                entry.interrupt_id
            ));
        }
        (false, _) => {
            let mut decision = ApprovalDecision::reject(approver.clone(), "");
            decision.reason = reason.map(str::to_string);
            decision
        }
    };
    Ok(decision.with_groups(groups.iter().cloned()))
}

/// What a refused decision says, in plain words, from where the entry stands now.
fn refusal_message(entry: Option<&ApprovalRequest>, fallback: &str) -> String {
    let Some(entry) = entry else {
        return fallback.to_string();
    };
    let by = entry.decisions.last().map(|decision| decision.authority().id.clone());
    match (entry.state, by) {
        (
            ApprovalState::Approved | ApprovalState::Rejected | ApprovalState::Responded,
            Some(by),
        ) => {
            format!("Already decided by {by}")
        }
        (ApprovalState::Approved | ApprovalState::Rejected | ApprovalState::Responded, None) => {
            "Already decided".to_string()
        }
        (ApprovalState::Expired, _) => "This decision expired".to_string(),
        (ApprovalState::Cancelled, _) => "This decision was withdrawn".to_string(),
        (ApprovalState::Pending, _) => fallback.to_string(),
    }
}

/// The message of a version conflict on a pending entry.
const CHANGED: &str = "This decision changed since it was shown; look at it again";

/// Whether `decision` is the answer `entry` already resolved with.
fn same_answer(entry: &ApprovalRequest, decision: &ApprovalDecision) -> bool {
    match decision.kind {
        ApprovalAction::Approve | ApprovalAction::Edit => entry.state == ApprovalState::Approved,
        ApprovalAction::Reject => entry.state == ApprovalState::Rejected,
        ApprovalAction::Respond => entry.state == ApprovalState::Responded,
    }
}

/// Why `service` would refuse `decision` on the pending `request` at `version` now, mirroring
/// [`ApprovalService::decide`]'s checks so a resume is refused before anything is recorded.
async fn refusal(
    service: &dyn ApprovalService,
    request: &ApprovalRequest,
    version: u64,
    decision: &ApprovalDecision,
) -> Option<(&'static str, String)> {
    let approver = &decision.approver;
    if request.version != version {
        return Some(("approval.version_conflict", CHANGED.to_string()));
    }
    if request.expires_at.is_some_and(|at| at <= chrono::Utc::now()) {
        return Some(("approval.expired", "This decision expired".to_string()));
    }
    if !request.allowed_actions.contains(&decision.kind) {
        return Some((
            "approval.action_not_allowed",
            format!("This decision does not allow {:?}", decision.kind).to_lowercase(),
        ));
    }
    if request.policy.forbid_self_approval && request.is_self(approver) {
        return Some((
            "approval.self_approval",
            format!("'{}' may not approve their own call", approver.id),
        ));
    }
    if !request.is_eligible(approver, &decision.approver_groups) {
        let now = chrono::Utc::now();
        let delegated = match service.delegations_to(approver).await {
            Ok(delegations) => {
                delegations.iter().any(|delegation| delegation.applies_to(request, now))
            }
            Err(error) => {
                warn!(error = %error, "could not read the approver's delegations");
                false
            }
        };
        if !delegated {
            return Some((
                "approval.not_an_approver",
                format!("'{}' may not decide this call", approver.id),
            ));
        }
    }
    if decision.kind == ApprovalAction::Approve && request.quorum_progress().0.contains(approver) {
        return Some((
            "approval.already_decided",
            format!("'{}' already approved this call; another approver must decide", approver.id),
        ));
    }
    None
}

/// Reads `entries`, answered as `approver`, a member of `groups`, and decides whether every
/// answer may be recorded — without recording any (spec 3.3 §10 S2, S-I2).
///
/// A resume is all or nothing. Every payload is checked, an interrupt answered twice is
/// refused, and every `resolved` entry is checked against its inbox entry as
/// [`ApprovalService::decide`] would check it, at the version the client saw (its
/// `metadata.version`) or, without one, the version read here. The first refusal refuses
/// the whole resume, and nothing is decided. An entry already decided whose call has not run
/// yet — decided on another surface, or by this resume sent before — is reported and lets the
/// run start, so the run dispatches the call; an entry whose call already ran is a refusal. A
/// `cancelled` entry decides nothing: the call keeps waiting in the inbox (spec 3.3 §7.3). An
/// entry naming no inbox entry of `scope` is reported as `unknown` and skipped, as AG-UI asks
/// of an unknown interrupt.
///
/// # Errors
///
/// Returns the reason when a payload is not an answer or an interrupt is answered twice.
pub(crate) async fn plan_resume(
    service: &dyn ApprovalService,
    scope: &ApprovalScope,
    approver: &SubjectRef,
    groups: &[String],
    entries: &[AgUiResumeEntry],
) -> Result<ResumeOutcome, String> {
    let mut decisions = Vec::with_capacity(entries.len());
    let mut answered = std::collections::HashSet::new();
    for entry in entries {
        if !answered.insert(entry.interrupt_id.as_str()) {
            return Err(format!(
                "resume answers interrupt '{}' more than once; send one answer per interrupt",
                entry.interrupt_id
            ));
        }
        decisions.push(match entry.status {
            AgUiResumeStatus::Resolved => Some(decision_for(entry, approver, groups)?),
            AgUiResumeStatus::Cancelled => None,
        });
    }
    let store = service.approval_store();
    let mut plan = ResumePlan::default();
    let mut updates = Vec::new();
    for (entry, decision) in entries.iter().zip(decisions) {
        let Some(decision) = decision else {
            info!(approval.id = %entry.interrupt_id, "AG-UI client abandoned an interrupt; the decision keeps waiting");
            continue;
        };
        let request = match service.get(&entry.interrupt_id).await {
            Ok(Some(request)) if request.scope == *scope => request,
            Ok(_) => {
                warn!(approval.id = %entry.interrupt_id, "AG-UI resume names no decision of this session; skipped");
                let update = decision_update(&entry.interrupt_id, None);
                updates.push(update.clone());
                plan.steps.push(Step::Report(update));
                continue;
            }
            Err(error) => {
                return Ok(ResumeOutcome::Refused {
                    updates,
                    code: error.code,
                    message: refusal_text(&error),
                });
            }
        };
        if request.state.is_terminal() {
            // Decided already: a call still waiting to run is dispatched by the run; one that
            // ran cannot be answered again.
            let waiting = match &store {
                Some(store) => store
                    .has_decision(&request.scope, &request.fingerprint)
                    .await
                    .unwrap_or_else(|error| {
                        warn!(error = %error, "could not read whether the decision waits to run");
                        false
                    }),
                None => false,
            };
            let message = refusal_message(Some(&request), CHANGED);
            if !waiting {
                updates.push(decision_update(&entry.interrupt_id, Some(&request)));
                return Ok(ResumeOutcome::Refused {
                    updates,
                    code: "approval.already_resolved",
                    message,
                });
            }
            let update = if same_answer(&request, &decision) {
                decision_update(&entry.interrupt_id, Some(&request))
            } else {
                refused_update(&entry.interrupt_id, &request, "approval.already_resolved", &message)
            };
            updates.push(update.clone());
            plan.steps.push(Step::Report(update));
            continue;
        }
        let version = entry
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("version"))
            .and_then(Value::as_u64)
            .unwrap_or(request.version);
        if let Some((code, message)) = refusal(service, &request, version, &decision).await {
            info!(approval.id = %request.id, error.code = code, "AG-UI resume refused before deciding");
            updates.push(decision_update(&entry.interrupt_id, Some(&request)));
            return Ok(ResumeOutcome::Refused { updates, code, message });
        }
        updates.push(decision_update(&entry.interrupt_id, Some(&request)));
        plan.steps.push(Step::Decide {
            interrupt_id: entry.interrupt_id.clone(),
            request: Box::new(request),
            version,
            decision: Box::new(decision),
        });
    }
    Ok(ResumeOutcome::Run(plan))
}

/// Records a planned resume through [`ApprovalService::decide`] and returns the decision
/// updates to send, one per answered entry, in order.
///
/// Call it once the run is about to start. An answer refused here — another approver decided
/// between the plan and now — is reported in its update with the refusal's `code` and
/// `message`; the others stand and the run starts, so the calls they decided run.
pub(crate) async fn commit_resume(service: &dyn ApprovalService, plan: ResumePlan) -> Vec<Value> {
    let mut updates = Vec::with_capacity(plan.steps.len());
    for step in plan.steps {
        match step {
            Step::Report(update) => updates.push(update),
            Step::Decide { interrupt_id, request, version, decision } => {
                match service.decide(&request.id, version, *decision).await {
                    Ok(decided) => updates.push(decision_update(&interrupt_id, Some(&decided))),
                    Err(error) => {
                        warn!(approval.id = %request.id, error.code = error.code, "AG-UI answer refused while recording it");
                        let now = service.get(&request.id).await.ok().flatten();
                        let current = now.as_ref().unwrap_or(request.as_ref());
                        let message = if error.category == ErrorCategory::Conflict {
                            refusal_message(Some(current), CHANGED)
                        } else {
                            refusal_text(&error)
                        };
                        updates.push(refused_update(&interrupt_id, current, error.code, &message));
                    }
                }
            }
        }
    }
    updates
}

/// An error's message when it describes the call, a generic one otherwise.
fn refusal_text(error: &adk_core::AdkError) -> String {
    if error.category.is_rejection() {
        error.message.clone()
    } else {
        "The decision could not be recorded; try again".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{
        ApprovalCall, ApprovalPolicy, ApprovalStore, ApproverSelector, InMemoryApprovalStore,
        StoreApprovalService, ToolApproval,
    };
    use proptest::prelude::*;

    fn service() -> StoreApprovalService {
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
            ApprovalPolicy::new([ApproverSelector::human("alice")]).allow_self_approval(),
        )
    }

    fn scope() -> ApprovalScope {
        ApprovalScope::new("finance", "alice", "s1")
    }

    fn call() -> ToolConfirmationRequest {
        ToolConfirmationRequest {
            tool_name: "send_invoice".into(),
            function_call_id: Some("call-1".into()),
            args: json!({ "invoice": "INV-10", "to": "client@example.com" }),
            origin: None,
            acting: None,
        }
    }

    async fn hold(service: &StoreApprovalService) -> ApprovalRequest {
        service
            .submit_call(ApprovalCall {
                scope: scope(),
                request: call(),
                reason: "Send the October invoice".into(),
                agent_name: "finance".into(),
                invocation_id: "inv-1".into(),
                run_id: None,
                acting: None,
            })
            .await
            .unwrap()
            .expect("routed to the inbox")
    }

    fn answer(id: &str, payload: Value) -> AgUiResumeEntry {
        AgUiResumeEntry {
            interrupt_id: id.into(),
            status: AgUiResumeStatus::Resolved,
            payload: Some(payload),
            metadata: None,
        }
    }

    fn held() -> HeldCall {
        HeldCall { request: call(), agent: "finance".into() }
    }

    fn alice() -> SubjectRef {
        SubjectRef::human("alice")
    }

    /// A resume as the handler runs it: planned, then recorded when the run may start.
    #[derive(Debug)]
    enum Applied {
        Run(Vec<Value>),
        Refused { updates: Vec<Value>, code: &'static str, message: String },
    }

    async fn apply_resume(
        service: &StoreApprovalService,
        scope: &ApprovalScope,
        approver: &SubjectRef,
        groups: &[String],
        entries: &[AgUiResumeEntry],
    ) -> Result<Applied, String> {
        Ok(match plan_resume(service, scope, approver, groups, entries).await? {
            ResumeOutcome::Run(plan) => Applied::Run(commit_resume(service, plan).await),
            ResumeOutcome::Refused { updates, code, message } => {
                Applied::Refused { updates, code, message }
            }
        })
    }

    #[tokio::test]
    async fn an_interrupt_is_drawn_from_the_inbox_entry_and_shows_arguments_only_to_the_owner() {
        let service = Arc::new(service());
        let entry = hold(&service).await;
        let decisions = |show_arguments| AgUiDecisions {
            service: Some(service.clone() as Arc<dyn ApprovalService>),
            scope: Some(scope()),
            show_arguments,
            updates: Vec::new(),
        };

        let stranger = decisions(false).interrupts(&[held()]).await;
        assert_eq!(
            stranger,
            vec![json!({
                "id": entry.id,
                "reason": "tool_call",
                "message": "Send the October invoice",
                "toolCallId": "call-1",
                "responseSchema": response_schema(true, true),
                "metadata": {
                    "summary": "Send the October invoice",
                    "agent": "finance",
                    "tool": "send_invoice",
                    "answerable": true,
                    "risk": "medium",
                    "version": 1,
                },
            })]
        );
        let owner = decisions(true).interrupts(&[held()]).await;
        assert_eq!(
            owner[0]["metadata"]["arguments"],
            json!({ "invoice": "INV-10", "to": "client@example.com" })
        );
    }

    #[tokio::test]
    async fn an_interrupt_carries_the_inbox_entry_expiry() {
        let service = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
                ApprovalPolicy::new([ApproverSelector::human("alice")])
                    .allow_self_approval()
                    .expires_after(std::time::Duration::from_secs(600)),
            ),
        );
        let entry = hold(&service).await;
        let expires_at = entry.expires_at.expect("the policy sets an expiry");
        let decisions = AgUiDecisions {
            service: Some(service as Arc<dyn ApprovalService>),
            scope: Some(scope()),
            ..AgUiDecisions::default()
        };
        let interrupts = decisions.interrupts(&[held()]).await;
        assert_eq!(interrupts[0]["expiresAt"], json!(expires_at.to_rfc3339()));
    }

    #[tokio::test]
    async fn a_call_without_an_inbox_entry_is_named_by_its_call_id() {
        let interrupts = AgUiDecisions::default().interrupts(&[held()]).await;
        assert_eq!(interrupts[0]["id"], "call-1");
        assert_eq!(interrupts[0]["toolCallId"], "call-1");
        assert_eq!(interrupts[0]["message"], "Approve a call to 'send_invoice'");
        assert!(interrupts[0]["metadata"].get("arguments").is_none());
        // Nothing can decide it here, so no answer is offered.
        assert!(interrupts[0].get("responseSchema").is_none());
        assert_eq!(interrupts[0]["metadata"]["answerable"], false);
    }

    #[test]
    fn payloads_map_to_the_decisions_they_name() {
        let decide = |payload: Value| decision_for(&answer("apr_1", payload), &alice(), &[]);
        assert_eq!(decide(json!({ "approved": true })).unwrap().kind, ApprovalAction::Approve);
        let edit =
            decide(json!({ "approved": true, "editedArgs": { "invoice": "INV-11" } })).unwrap();
        assert_eq!(
            (edit.kind, edit.edited_args),
            (ApprovalAction::Edit, Some(json!({ "invoice": "INV-11" })))
        );
        let deny = decide(json!({ "approved": false, "reason": "wrong client" })).unwrap();
        assert_eq!(
            (deny.kind, deny.reason.as_deref()),
            (ApprovalAction::Reject, Some("wrong client"))
        );
        assert_eq!(decide(json!({ "approved": false })).unwrap().reason, None);
        for invalid in [
            json!({}),
            json!({ "approved": "yes" }),
            json!(true),
            json!({ "approved": true, "editedArgs": [1] }),
        ] {
            assert!(decide(invalid.clone()).is_err(), "{invalid} is not an answer");
        }
        let missing = AgUiResumeEntry { payload: None, ..answer("apr_1", Value::Null) };
        assert!(decision_for(&missing, &alice(), &[]).is_err());
    }

    #[tokio::test]
    async fn an_invalid_answer_decides_nothing() {
        let service = service();
        let entry = hold(&service).await;
        let entries =
            [answer(&entry.id, json!({ "approved": true })), answer(&entry.id, json!({ "ok": 1 }))];
        assert!(apply_resume(&service, &scope(), &alice(), &[], &entries).await.is_err());
        assert_eq!(service.get(&entry.id).await.unwrap().unwrap().state, ApprovalState::Pending);
    }

    #[tokio::test]
    async fn a_stale_version_is_reported_not_retried() {
        let service = service();
        let entry = hold(&service).await;
        let stale = AgUiResumeEntry {
            metadata: Some(json!({ "version": entry.version + 1 })),
            ..answer(&entry.id, json!({ "approved": true }))
        };
        let outcome = apply_resume(&service, &scope(), &alice(), &[], &[stale]).await.unwrap();
        let Applied::Refused { updates, code, message } = outcome else {
            panic!("a stale version must be refused");
        };
        assert_eq!(code, "approval.version_conflict");
        assert_eq!(message, "This decision changed since it was shown; look at it again");
        assert_eq!(updates, vec![decision_update(&entry.id, Some(&entry))]);
        assert_eq!(service.get(&entry.id).await.unwrap().unwrap(), entry, "nothing was decided");
    }

    #[tokio::test]
    async fn an_entry_of_another_session_is_unknown_and_left_alone() {
        let service = service();
        let entry = hold(&service).await;
        let elsewhere = ApprovalScope::new("finance", "alice", "s2");
        let outcome = apply_resume(
            &service,
            &elsewhere,
            &alice(),
            &[],
            &[answer(&entry.id, json!({ "approved": true }))],
        )
        .await
        .unwrap();
        let Applied::Run(updates) = outcome else { panic!("an unknown entry is skipped") };
        assert_eq!(updates, vec![json!({ "interruptId": entry.id, "state": "unknown" })]);
        assert_eq!(service.get(&entry.id).await.unwrap().unwrap().state, ApprovalState::Pending);
    }

    #[tokio::test]
    async fn an_answer_overtaken_while_recording_is_reported_and_the_rest_stand() {
        let service = service();
        let entry = hold(&service).await;
        let ResumeOutcome::Run(plan) = plan_resume(
            &service,
            &scope(),
            &alice(),
            &[],
            &[answer(&entry.id, json!({ "approved": true }))],
        )
        .await
        .unwrap() else {
            panic!("the answer may be recorded");
        };
        // Another surface decides between the plan and the run.
        service
            .decide(&entry.id, entry.version, ApprovalDecision::reject(alice(), "not now"))
            .await
            .unwrap();
        let updates = commit_resume(&service, plan).await;
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0]["state"], "rejected");
        assert_eq!(updates[0]["code"], "approval.version_conflict");
        assert_eq!(updates[0]["message"], "Already decided by alice");
        assert_eq!(service.get(&entry.id).await.unwrap().unwrap().decisions.len(), 1);
    }

    /// One answer an AG-UI client may send.
    fn any_answer() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(json!({ "approved": true })),
            "[a-z]{0,8}".prop_map(|reason| json!({ "approved": false, "reason": reason })),
            "[A-Z0-9-]{1,8}".prop_map(
                |invoice| json!({ "approved": true, "editedArgs": { "invoice": invoice } })
            ),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// However many answers arrive for one interrupt, and in whatever form, exactly the
        /// first decides it. Later answers while the call waits to run decide nothing and let
        /// the run start, reporting a different answer as refused; once the call ran, an
        /// answer is refused. The store holds one decision, which authorizes one execution
        /// (spec 3.3 S-I2).
        #[test]
        fn the_first_answer_decides_and_later_ones_never_decide_again(
            answers in proptest::collection::vec(any_answer(), 1..6),
            echo_version in any::<bool>(),
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
            runtime.block_on(async {
                let store = Arc::new(InMemoryApprovalStore::new());
                let service = StoreApprovalService::new(store.clone()).with_router(
                    ApprovalPolicy::new([ApproverSelector::human("alice")]).allow_self_approval(),
                );
                let entry = hold(&service).await;
                let state_for = |payload: &Value| match payload["approved"].as_bool() {
                    Some(true) => ApprovalState::Approved,
                    _ => ApprovalState::Rejected,
                };
                let resume = |payload: &Value| {
                    let mut resume = answer(&entry.id, payload.clone());
                    if echo_version {
                        resume.metadata = Some(json!({ "version": entry.version }));
                    }
                    resume
                };
                for (index, payload) in answers.iter().enumerate() {
                    let outcome = apply_resume(&service, &scope(), &alice(), &[], &[resume(payload)])
                        .await
                        .unwrap();
                    let Applied::Run(updates) = outcome else {
                        prop_assert!(false, "answer {index} was refused while the call waits");
                        unreachable!();
                    };
                    prop_assert_eq!(updates.len(), 1);
                    prop_assert_eq!(&updates[0]["state"], state_for(&answers[0]).as_str());
                    let differs = index > 0 && state_for(payload) != state_for(&answers[0]);
                    prop_assert_eq!(
                        updates[0].get("code").and_then(Value::as_str),
                        differs.then_some("approval.already_resolved")
                    );
                }
                let decided = service.get(&entry.id).await.unwrap().unwrap();
                prop_assert_eq!(decided.decisions.len(), 1);
                let first = &answers[0];
                let expected = match (first["approved"].as_bool(), first.get("editedArgs")) {
                    (Some(true), None) => ApprovalAction::Approve,
                    (Some(true), Some(_)) => ApprovalAction::Edit,
                    _ => ApprovalAction::Reject,
                };
                prop_assert_eq!(decided.decisions[0].kind, expected);
                // The call runs: its decision is taken once.
                let fingerprint = call().fingerprint();
                let taken: Option<ToolApproval> =
                    store.take_approval(&scope(), &fingerprint).await.unwrap();
                prop_assert!(taken.is_some(), "the decision reached the store");
                prop_assert!(store.take_approval(&scope(), &fingerprint).await.unwrap().is_none());
                let after = apply_resume(&service, &scope(), &alice(), &[], &[resume(&answers[0])])
                    .await
                    .unwrap();
                prop_assert!(
                    matches!(after, Applied::Refused { code: "approval.already_resolved", .. }),
                    "an answer after the call ran is refused: {after:?}"
                );
                Ok(())
            })?;
        }
    }
}
