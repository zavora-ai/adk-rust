//! AG-UI decisions (spec 3.3 §10 S2): a call held for approval ends the run with an AG-UI
//! interrupt, and an AG-UI resume becomes [`ApprovalService::decide`] on the inbox entry.
//!
//! The shapes follow the AG-UI interrupt-aware run lifecycle (AG-UI 1.1 draft,
//! <https://docs.ag-ui.com/concepts/interrupts>): `RUN_FINISHED` carries
//! `outcome: {"type": "interrupt", "interrupts": [...]}`, each interrupt bound to its tool
//! call with `reason: "tool_call"` and `toolCallId`, and the next run's input answers them in
//! `resume: [{"interruptId", "status", "payload"}]`.
//!
//! AG-UI front ends cannot confirm it is the person, so they answer as the surfaces of spec
//! 3.3 §7.2 that cannot: a high-risk call, or one that spends money, may be denied here but
//! is approved only where the person can confirm it is them. Every answer is written to the
//! server's audit trail with the surface and how it was given (spec 3.3 §11).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditTrail,
};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalAction, ApprovalDecision, ApprovalRequest, ApprovalRisk, ApprovalScope,
    ApprovalService, ApprovalState, ErrorCategory, ToolConfirmationRequest,
};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::runtime::{AgUiResumeEntry, AgUiResumeStatus};

/// The `CUSTOM` event name of a decision update.
pub(crate) const DECISION_EVENT: &str = "adk.decision";

/// The AG-UI interrupt reason for an interrupt bound to one tool call.
pub(crate) const TOOL_CALL_REASON: &str = "tool_call";

/// The refusal code of a resume sent while another resume of the session is in progress.
pub(crate) const RESUME_IN_PROGRESS: &str = "approval.resume_in_progress";

/// The audit action of an answer to a decision given through a surface, as adk-surface
/// records one (spec 3.3 §11).
pub(crate) const AUDIT_DECIDE: &str = "surface.decide";

/// The surface an AG-UI answer's audit record names: the UI protocol profile.
pub(crate) const AUDIT_SURFACE: &str = "ag_ui";

/// A call the run held for approval, as the stream saw it.
#[derive(Debug, Clone)]
pub(crate) struct HeldCall {
    pub(crate) request: ToolConfirmationRequest,
    /// The agent that held the call.
    pub(crate) agent: String,
}

/// The authenticated caller of an AG-UI request, who answers as the session's owner.
#[derive(Debug, Clone)]
pub(crate) struct AgUiCaller {
    /// The session's owner, as a person.
    pub(crate) approver: SubjectRef,
    /// The groups the caller's authentication established.
    pub(crate) groups: Vec<String>,
}

/// What a protocol-native AG-UI stream needs to turn held calls into interrupts.
#[derive(Clone, Default)]
pub(crate) struct AgUiDecisions {
    /// The inbox held calls are routed to; without one an interrupt names the call ID.
    pub(crate) service: Option<Arc<dyn ApprovalService>>,
    /// The session the run belongs to.
    pub(crate) scope: Option<ApprovalScope>,
    /// The caller the server's request-context extractor authenticated as the session's
    /// owner: an interrupt shows them the call's arguments and offers the answers they may
    /// give. Without one no resume is accepted, so nothing is offered.
    pub(crate) caller: Option<AgUiCaller>,
    /// `adk.decision` values for the resume entries this request answered, sent after
    /// `RUN_STARTED`.
    pub(crate) updates: Vec<Value>,
    /// Where each answer is recorded with the surface and how it was given: the audit trail
    /// attached to the server's kill switch, when one is (spec 3.3 §11).
    pub(crate) audit: Option<AuditTrail>,
}

impl AgUiDecisions {
    /// The interrupts for `held`, in order.
    ///
    /// # Errors
    ///
    /// Returns the error of a failed inbox read: an interrupt built without its entry would
    /// tell the client the call cannot be decided here.
    pub(crate) async fn interrupts(&self, held: &[HeldCall]) -> Result<Vec<Value>, AdkError> {
        let mut interrupts = Vec::with_capacity(held.len());
        for call in held {
            let entry = self.inbox_entry(&call.request).await?;
            let answers = match (entry.as_ref(), self.service.as_deref(), self.caller.as_ref()) {
                (Some(entry), Some(service), Some(caller)) => {
                    answers_for(service, entry, caller).await
                }
                _ => Vec::new(),
            };
            interrupts.push(interrupt(call, entry.as_ref(), self.caller.is_some(), &answers));
        }
        Ok(interrupts)
    }

    /// The inbox entry the governed path submitted for `request`, found as
    /// `StoreApprovalService::submit_call` names it.
    async fn inbox_entry(
        &self,
        request: &ToolConfirmationRequest,
    ) -> Result<Option<ApprovalRequest>, AdkError> {
        let (Some(service), Some(scope)) = (self.service.as_ref(), self.scope.as_ref()) else {
            return Ok(None);
        };
        let Some(store) = service.approval_store() else {
            return Ok(None);
        };
        let fingerprint = request.fingerprint();
        let pending = store.pending(scope).await.inspect_err(|error| {
            warn!(error = %error, "could not read the held calls for the AG-UI interrupt");
        })?;
        let Some(held) = pending.into_iter().find(|entry| entry.fingerprint == fingerprint) else {
            return Ok(None);
        };
        let id = ApprovalRequest::id_for_call(scope, &fingerprint, held.requested_at);
        service.get(&id).await.inspect_err(|error| {
            warn!(error = %error, approval.id = %id, "could not read the inbox entry for the AG-UI interrupt");
        })
    }
}

/// The answers `caller` may give `entry` here, in the order Approve, Edit, Reject: the ones
/// [`refusal`] lets through.
async fn answers_for(
    service: &dyn ApprovalService,
    entry: &ApprovalRequest,
    caller: &AgUiCaller,
) -> Vec<ApprovalAction> {
    let approver = caller.approver.clone();
    let mut answers = Vec::new();
    for decision in [
        ApprovalDecision::approve(approver.clone()),
        ApprovalDecision::edit(approver.clone(), Value::Null),
        ApprovalDecision::reject(approver, ""),
    ] {
        let decision = decision.with_groups(caller.groups.iter().cloned());
        if refusal(service, entry, entry.version, &decision).await.is_none() {
            answers.push(decision.kind);
        }
    }
    answers
}

/// One AG-UI `Interrupt` for a held call: the inbox entry's ID when the call has one, else
/// the call ID; the arguments only for the session's owner.
///
/// An interrupt offers `answers` — what the caller may answer here — in its
/// `responseSchema`; one with none, such as a call with no inbox entry, a server that
/// authenticates no caller, or an entry the caller may not decide, carries no
/// `responseSchema` and `metadata.answerable: false` (spec 3.3 S-I1).
fn interrupt(
    call: &HeldCall,
    entry: Option<&ApprovalRequest>,
    show_arguments: bool,
    answers: &[ApprovalAction],
) -> Value {
    let request = &call.request;
    let call_id =
        request.function_call_id.clone().unwrap_or_else(|| format!("{}-call", request.tool_name));
    let summary = entry.map_or_else(
        || format!("Approve a call to '{}'", request.tool_name),
        |entry| entry.summary.clone(),
    );
    // The agent's name, as adk-surface names a decision's agent: the last segment of the
    // acting identity the inbox records (`urn:adk:agent:finance/finance` reads `finance`,
    // spec 3.3 §7.1, §8); the hold event's own author only when there is no entry.
    let agent = match entry.and_then(|entry| entry.acting.as_ref()) {
        Some(acting) => {
            let id = acting.agent.id.as_str();
            id.rsplit(['/', ':']).next().filter(|last| !last.is_empty()).unwrap_or(id).to_string()
        }
        None => call.agent.clone(),
    };
    let mut metadata = json!({
        "summary": summary,
        "agent": agent,
        "tool": request.tool_name,
        "answerable": !answers.is_empty(),
    });
    if let Some(entry) = entry {
        metadata["risk"] = json!(entry.risk);
        metadata["version"] = json!(entry.version);
    }
    if show_arguments {
        // The arguments the call runs with: another approver's edit replaces the ones held.
        metadata["arguments"] = entry.map_or_else(
            || request.args.clone(),
            |entry| entry.quorum_progress().1.unwrap_or_else(|| entry.args_redacted.clone()),
        );
    }
    let mut interrupt = json!({
        "id": entry.map_or_else(|| call_id.clone(), |entry| entry.id.clone()),
        "reason": TOOL_CALL_REASON,
        "message": summary,
        "toolCallId": call_id,
        "metadata": metadata,
    });
    if !answers.is_empty() {
        interrupt["responseSchema"] = response_schema(answers);
    }
    if let Some(at) = entry.and_then(|entry| entry.expires_at) {
        interrupt["expiresAt"] = json!(at.to_rfc3339());
    }
    interrupt
}

/// The answers an interrupt accepts, in AG-UI's recommended approval shape: `approved` takes
/// only the values `answers` allow, `editedArgs` only with Edit, and `reason` only with
/// Reject.
fn response_schema(answers: &[ApprovalAction]) -> Value {
    let can = |action: ApprovalAction| answers.contains(&action);
    let yes = can(ApprovalAction::Approve) || can(ApprovalAction::Edit);
    let no = can(ApprovalAction::Reject);
    let mut approved = json!({ "type": "boolean" });
    if yes != no {
        approved["const"] = json!(yes);
    }
    let mut properties = json!({ "approved": approved });
    if can(ApprovalAction::Edit) {
        properties["editedArgs"] = json!({ "type": "object" });
    }
    if no {
        properties["reason"] = json!({ "type": "string" });
    }
    let mut schema =
        json!({ "type": "object", "properties": properties, "required": ["approved"] });
    if can(ApprovalAction::Edit) && !can(ApprovalAction::Approve) {
        // Yes is only an edit, so it carries the replacement arguments.
        schema["if"] = json!({ "properties": { "approved": { "const": true } } });
        schema["then"] = json!({ "required": ["editedArgs"] });
    }
    schema
}

/// An `adk.decision` value: where the inbox entry `interrupt_id` stands, with the version to
/// answer it at, or `unknown` when the session has no such entry.
///
/// Only a resume, whose caller is authenticated as the session's owner, sends one, so it also
/// carries the arguments another approver's edit put in place of the ones the interrupt showed.
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
    if matches!(entry.state, ApprovalState::Pending | ApprovalState::Approved)
        && let Some(edited) = entry.quorum_progress().1
    {
        value["editedArgs"] = edited;
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

/// The sessions with an AG-UI resume in progress, from reading its answers until its run's
/// stream ends, so two resumes never run the same held call at once (spec 3.3 §10 S2, S-I2).
#[derive(Clone, Default)]
pub(crate) struct ResumesInProgress(Arc<Mutex<HashSet<ApprovalScope>>>);

impl ResumesInProgress {
    /// Starts a resume of `scope`, or `None` while another one is in progress.
    pub(crate) fn begin(&self, scope: &ApprovalScope) -> Option<ResumeInProgress> {
        let inserted =
            self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(scope.clone());
        inserted.then(|| ResumeInProgress { sessions: self.0.clone(), scope: scope.clone() })
    }
}

/// One resume in progress; dropping it ends the resume.
pub(crate) struct ResumeInProgress {
    sessions: Arc<Mutex<HashSet<ApprovalScope>>>,
    scope: ApprovalScope,
}

impl Drop for ResumeInProgress {
    fn drop(&mut self) {
        self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.scope);
    }
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
    /// Nothing to record; report this update, and audit the answer to an entry another
    /// decision already resolved.
    Report { update: Value, answered: Option<Box<(ApprovalRequest, ApprovalDecision)>> },
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

/// Whether `decision` is the answer `entry` already resolved with: the same verdict and, for
/// a yes, the same replacement arguments or none.
fn same_answer(entry: &ApprovalRequest, decision: &ApprovalDecision) -> bool {
    match decision.kind {
        ApprovalAction::Approve | ApprovalAction::Edit => {
            entry.state == ApprovalState::Approved
                && entry.quorum_progress().1 == decision.edited_args
        }
        ApprovalAction::Reject => entry.state == ApprovalState::Rejected,
        ApprovalAction::Respond => entry.state == ApprovalState::Responded,
    }
}

/// Whether a call is approved only where the person can confirm it is them: a high-risk
/// call, or one that spends money — the payment tools `adk-payments` publishes, all named
/// `payments_*` — as adk-surface's rules tell them (spec 3.3 §7.2, §8.2).
fn needs_presence(request: &ApprovalRequest) -> bool {
    request.risk >= ApprovalRisk::High || request.tool_name.starts_with("payments_")
}

/// Why `decision` on the pending `request` at `version` would be refused now: the checks of
/// [`ApprovalService::decide`], so a resume is refused before anything is recorded, and the
/// rule of spec 3.3 §7.2 that a surface which cannot confirm it is the person does not
/// approve a high-risk call or one that spends money.
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
        // The answer in the words of spec 3.3 §8.2.
        let answer = match decision.kind {
            ApprovalAction::Approve => "Allow once",
            ApprovalAction::Edit => "Edit",
            ApprovalAction::Reject => "Deny",
            ApprovalAction::Respond => "a reply",
        };
        return Some((
            "approval.action_not_allowed",
            format!("This decision does not offer {answer}"),
        ));
    }
    if matches!(decision.kind, ApprovalAction::Approve | ApprovalAction::Edit)
        && needs_presence(request)
    {
        return Some((
            "surface.answer_not_allowed",
            "This call is high risk or spends money; approve it where you can confirm it's you"
                .to_string(),
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
/// answer may be recorded — without recording any (spec 3.3 §10 S2, S-I2). A refused answer
/// is written to `audit` (spec 3.3 §11).
///
/// A resume is all or nothing. Every payload is checked, an interrupt answered twice is
/// refused, and every `resolved` entry is checked against its inbox entry as
/// [`ApprovalService::decide`] would check it, at the version the client saw: its
/// `metadata.version`, or, without one, the version read here — unless another approver's
/// edit has replaced the arguments the interrupt showed, which is a version conflict. The
/// first refusal refuses the whole resume, and nothing is decided. An entry already decided
/// whose call has not run yet — decided on another surface, expired, withdrawn, or decided by
/// this resume sent before — is reported and lets the run start, so the run dispatches the
/// call with that outcome; a different answer is reported with a `code`. An entry whose call
/// already ran is a refusal. A `cancelled` entry decides nothing: the call keeps waiting in
/// the inbox (spec 3.3 §7.3). An entry naming no inbox entry of `scope` is reported as
/// `unknown` and skipped, as AG-UI asks of an unknown interrupt.
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
    audit: Option<&AuditTrail>,
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
                plan.steps.push(Step::Report { update, answered: None });
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
                Some(store) => {
                    match store.has_decision(&request.scope, &request.fingerprint).await {
                        Ok(waiting) => waiting,
                        Err(error) => {
                            warn!(error = %error, approval.id = %request.id, "could not read whether the decision waits to run");
                            audit_answer(audit, &request, &decision, "refused", Some(error.code))
                                .await;
                            updates.push(decision_update(&entry.interrupt_id, Some(&request)));
                            return Ok(ResumeOutcome::Refused {
                                updates,
                                code: error.code,
                                message: refusal_text(&error),
                            });
                        }
                    }
                }
                None => false,
            };
            let message = refusal_message(Some(&request), CHANGED);
            if !waiting {
                audit_answer(
                    audit,
                    &request,
                    &decision,
                    "refused",
                    Some("approval.already_resolved"),
                )
                .await;
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
            plan.steps.push(Step::Report { update, answered: Some(Box::new((request, decision))) });
            continue;
        }
        let echoed = entry
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("version"))
            .and_then(Value::as_u64);
        let version = echoed.unwrap_or(request.version);
        // Without an echoed version the answer is to the call as the interrupt showed it, so
        // another approver's edit since then is a conflict, never approved unseen.
        let refused = if echoed.is_none() && request.quorum_progress().1.is_some() {
            Some(("approval.version_conflict", CHANGED.to_string()))
        } else {
            refusal(service, &request, version, &decision).await
        };
        if let Some((code, message)) = refused {
            info!(approval.id = %request.id, error.code = code, "AG-UI resume refused before deciding");
            audit_answer(audit, &request, &decision, "refused", Some(code)).await;
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
/// updates to send, one per answered entry, in order. Every answer is written to `audit`
/// with the surface and how it was given (spec 3.3 §11).
///
/// Call it once the run is about to start. An answer refused here — another approver decided
/// between the plan and now — is reported in its update with the refusal's `code` and
/// `message`; the others stand and the run starts, so the calls they decided run.
pub(crate) async fn commit_resume(
    service: &dyn ApprovalService,
    plan: ResumePlan,
    audit: Option<&AuditTrail>,
) -> Vec<Value> {
    let mut updates = Vec::with_capacity(plan.steps.len());
    for step in plan.steps {
        match step {
            Step::Report { update, answered } => {
                if let Some(answered) = answered {
                    let (request, decision) = *answered;
                    let code = update.get("code").and_then(Value::as_str);
                    audit_answer(audit, &request, &decision, "already_decided", code).await;
                }
                updates.push(update);
            }
            Step::Decide { interrupt_id, request, version, decision } => {
                match service.decide(&request.id, version, (*decision).clone()).await {
                    Ok(decided) => {
                        info!(approval.id = %request.id, approval.action = ?decision.kind, "AG-UI answer recorded");
                        audit_answer(audit, &decided, &decision, "decided", None).await;
                        updates.push(decision_update(&interrupt_id, Some(&decided)));
                    }
                    Err(error) => {
                        warn!(approval.id = %request.id, error.code = error.code, "AG-UI answer refused while recording it");
                        let now = service.get(&request.id).await.ok().flatten();
                        let current = now.as_ref().unwrap_or(request.as_ref());
                        let message = if error.category == ErrorCategory::Conflict {
                            refusal_message(Some(current), CHANGED)
                        } else {
                            refusal_text(&error)
                        };
                        audit_answer(audit, current, &decision, "changed", Some(error.code)).await;
                        updates.push(refused_update(&interrupt_id, current, error.code, &message));
                    }
                }
            }
        }
    }
    updates
}

/// Writes one AG-UI answer to `audit` as adk-surface writes an answer (`surface.decide`):
/// who, the entry, the answer and its `outcome`, with the surface (`ag_ui`) and how it was
/// given — in another app — so "What your agents did" can say where it came from (spec 3.3
/// §11). A record that cannot be written is logged; the answer stands.
async fn audit_answer(
    audit: Option<&AuditTrail>,
    request: &ApprovalRequest,
    decision: &ApprovalDecision,
    outcome: &str,
    code: Option<&str>,
) {
    let Some(trail) = audit else {
        return;
    };
    // The answer in adk-surface's wire words.
    let answer = match decision.kind {
        ApprovalAction::Approve => "allowOnce",
        ApprovalAction::Edit => "edit",
        ApprovalAction::Reject => "deny",
        ApprovalAction::Respond => "respond",
    };
    let mut draft = AuditRecordDraft::new(AuditRecordKind::Approval, AUDIT_DECIDE)
        .actor(AuditActor::subject(decision.approver.clone()))
        .ext("surface", json!(AUDIT_SURFACE))
        .ext("origin", json!({ "kind": "app", "channel": "another app" }))
        .ext("approvalId", json!(request.id))
        .ext("sessionId", json!(request.scope.session_id))
        .ext("tool", json!(request.tool_name))
        .ext("answer", json!(answer))
        .ext("outcome", json!(outcome));
    if outcome == "decided" {
        draft = draft.decision(match decision.kind {
            ApprovalAction::Reject | ApprovalAction::Respond => AuditDecision::Rejected,
            ApprovalAction::Approve | ApprovalAction::Edit => AuditDecision::Approved,
        });
    }
    if let Some(code) = code {
        draft = draft.ext("code", json!(code));
    }
    if let Err(error) = trail.record(draft).await {
        warn!(error = %error, approval.id = %request.id, "the AG-UI answer stands; its audit record failed");
    }
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
        ApprovalCall, ApprovalPolicy, ApprovalRoute, ApprovalStore, ApproverSelector,
        InMemoryApprovalStore, StoreApprovalService, ToolApproval,
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
        Ok(match plan_resume(service, scope, approver, groups, entries, None).await? {
            ResumeOutcome::Run(plan) => Applied::Run(commit_resume(service, plan, None).await),
            ResumeOutcome::Refused { updates, code, message } => {
                Applied::Refused { updates, code, message }
            }
        })
    }

    fn owner() -> AgUiCaller {
        AgUiCaller { approver: alice(), groups: Vec::new() }
    }

    fn decisions_for(
        service: Arc<dyn ApprovalService>,
        caller: Option<AgUiCaller>,
    ) -> AgUiDecisions {
        AgUiDecisions {
            service: Some(service),
            scope: Some(scope()),
            caller,
            updates: Vec::new(),
            audit: None,
        }
    }

    #[tokio::test]
    async fn an_interrupt_is_drawn_from_the_inbox_entry_and_shows_arguments_only_to_the_owner() {
        let service = Arc::new(service());
        let entry = hold(&service).await;
        let owner = decisions_for(service.clone(), Some(owner())).interrupts(&[held()]).await;
        assert_eq!(
            owner.unwrap(),
            vec![json!({
                "id": entry.id,
                "reason": "tool_call",
                "message": "Send the October invoice",
                "toolCallId": "call-1",
                "responseSchema": {
                    "type": "object",
                    "properties": {
                        "approved": { "type": "boolean" },
                        "editedArgs": { "type": "object" },
                        "reason": { "type": "string" },
                    },
                    "required": ["approved"],
                },
                "metadata": {
                    "summary": "Send the October invoice",
                    "agent": "finance",
                    "tool": "send_invoice",
                    "answerable": true,
                    "risk": "medium",
                    "version": 1,
                    "arguments": { "invoice": "INV-10", "to": "client@example.com" },
                },
            })]
        );
        // Without an authenticated caller no resume is accepted: no arguments, no answers.
        let stranger = decisions_for(service, None).interrupts(&[held()]).await.unwrap();
        assert!(stranger[0]["metadata"].get("arguments").is_none());
        assert!(stranger[0].get("responseSchema").is_none());
        assert_eq!(stranger[0]["metadata"]["answerable"], false);
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
        let interrupts = decisions.interrupts(&[held()]).await.unwrap();
        assert_eq!(interrupts[0]["expiresAt"], json!(expires_at.to_rfc3339()));
    }

    #[tokio::test]
    async fn a_call_without_an_inbox_entry_is_named_by_its_call_id() {
        let interrupts = AgUiDecisions::default().interrupts(&[held()]).await.unwrap();
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
            None,
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
        let updates = commit_resume(&service, plan, None).await;
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0]["state"], "rejected");
        assert_eq!(updates[0]["code"], "approval.version_conflict");
        assert_eq!(updates[0]["message"], "Already decided by alice");
        assert_eq!(service.get(&entry.id).await.unwrap().unwrap().decisions.len(), 1);
    }

    // ----- Wave 2 review (spec 3.3 §10 S2) -----

    /// Holds `call()` under `router`, the way the governed path submits it.
    async fn hold_routed(
        router: impl adk_core::ApprovalRouter + 'static,
    ) -> (Arc<StoreApprovalService>, ApprovalRequest) {
        let service = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(router),
        );
        let entry = hold(&service).await;
        (service, entry)
    }

    fn self_approved() -> ApprovalPolicy {
        ApprovalPolicy::new([ApproverSelector::human("alice")]).allow_self_approval()
    }

    /// Findings 8 and 15: the schema offers only the answers the entry allows.
    #[tokio::test]
    async fn an_interrupt_offers_only_the_answers_the_entry_allows() {
        let schema_for = |actions: Vec<ApprovalAction>| async move {
            let (service, _) = hold_routed(move |_: &ApprovalCall| {
                Some(ApprovalRoute::new(self_approved()).with_allowed_actions(actions.clone()))
            })
            .await;
            let interrupts =
                decisions_for(service, Some(owner())).interrupts(&[held()]).await.unwrap();
            interrupts[0].get("responseSchema").cloned()
        };
        assert_eq!(
            schema_for(vec![ApprovalAction::Approve, ApprovalAction::Edit]).await,
            Some(json!({
                "type": "object",
                "properties": {
                    "approved": { "type": "boolean", "const": true },
                    "editedArgs": { "type": "object" },
                },
                "required": ["approved"],
            }))
        );
        assert_eq!(
            schema_for(vec![ApprovalAction::Reject]).await,
            Some(json!({
                "type": "object",
                "properties": {
                    "approved": { "type": "boolean", "const": false },
                    "reason": { "type": "string" },
                },
                "required": ["approved"],
            }))
        );
        // A yes that is only an edit carries the replacement arguments.
        let edit_only =
            schema_for(vec![ApprovalAction::Edit, ApprovalAction::Reject]).await.unwrap();
        assert_eq!(edit_only["properties"]["approved"], json!({ "type": "boolean" }));
        assert_eq!(edit_only["then"], json!({ "required": ["editedArgs"] }));
    }

    /// Finding 3: a high-risk call, or one that spends money, may be denied from AG-UI but is
    /// approved only where the person can confirm it is them (spec 3.3 §7.2).
    #[tokio::test]
    async fn a_high_risk_or_money_call_is_only_denied_here() {
        for (risk, tool) in [
            (ApprovalRisk::High, "send_invoice"),
            (ApprovalRisk::Critical, "send_invoice"),
            (ApprovalRisk::Low, "payments_send"),
        ] {
            let service = Arc::new(
                StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
                    move |_: &ApprovalCall| {
                        Some(ApprovalRoute::new(self_approved()).with_risk(risk))
                    },
                ),
            );
            let request = ToolConfirmationRequest { tool_name: tool.into(), ..call() };
            let entry = service
                .submit_call(ApprovalCall {
                    scope: scope(),
                    request: request.clone(),
                    reason: "Pay the supplier".into(),
                    agent_name: "finance".into(),
                    invocation_id: "inv-1".into(),
                    run_id: None,
                    acting: None,
                })
                .await
                .unwrap()
                .expect("routed to the inbox");
            let interrupts = decisions_for(service.clone(), Some(owner()))
                .interrupts(&[HeldCall { request, agent: "finance".into() }])
                .await
                .unwrap();
            assert_eq!(
                interrupts[0]["responseSchema"]["properties"]["approved"],
                json!({ "type": "boolean", "const": false }),
                "{tool} at {risk:?} offers deny only"
            );
            for yes in [json!({ "approved": true }), json!({ "approved": true, "editedArgs": {} })]
            {
                let outcome =
                    apply_resume(&service, &scope(), &alice(), &[], &[answer(&entry.id, yes)])
                        .await
                        .unwrap();
                assert!(
                    matches!(outcome, Applied::Refused { code: "surface.answer_not_allowed", .. }),
                    "{tool} at {risk:?}: {outcome:?}"
                );
            }
            assert_eq!(
                service.get(&entry.id).await.unwrap().unwrap().state,
                ApprovalState::Pending
            );
            let no = answer(&entry.id, json!({ "approved": false }));
            let outcome = apply_resume(&service, &scope(), &alice(), &[], &[no]).await.unwrap();
            assert!(matches!(outcome, Applied::Run(_)), "a deny stands: {outcome:?}");
            assert_eq!(
                service.get(&entry.id).await.unwrap().unwrap().state,
                ApprovalState::Rejected
            );
        }
    }

    /// Finding 7: the interrupt names the agent, not its identity URN.
    #[test]
    fn the_interrupt_names_the_agent_from_its_acting_identity() {
        let mut entry = ApprovalRequest::new(
            "apr_1",
            scope(),
            &call(),
            ApprovalPolicy::new([ApproverSelector::human("alice")]),
        );
        entry.acting = Some(adk_core::ActingContext::derived(
            "finance",
            "invoicer",
            "alice",
            adk_core::SubjectKind::Human,
        ));
        let interrupt = interrupt(&held(), Some(&entry), false, &[]);
        assert_eq!(interrupt["metadata"]["agent"], "invoicer");
    }

    #[test]
    fn a_session_resumes_once_at_a_time() {
        let resumes = ResumesInProgress::default();
        let first = resumes.begin(&scope()).expect("nothing in progress");
        assert!(resumes.begin(&scope()).is_none(), "one at a time");
        assert!(resumes.begin(&ApprovalScope::new("finance", "alice", "s2")).is_some());
        drop(first);
        assert!(resumes.begin(&scope()).is_some(), "ended when dropped");
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
                    // A different verdict, or a yes with other replacement arguments.
                    let differs = index > 0
                        && (state_for(payload) != state_for(&answers[0])
                            || (state_for(payload) == ApprovalState::Approved
                                && payload.get("editedArgs") != answers[0].get("editedArgs")));
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
