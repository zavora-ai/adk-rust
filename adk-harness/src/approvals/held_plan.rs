//! Screen-plan cards on surfaces that park held calls (spec 3.7 Q8): the plan a held
//! `computer_use_execute` is a step of, decided from its first step's card with the same
//! binding and audit record as a live prompter's answer.

use std::sync::Arc;

use adk_core::audit_log::AuditActor;
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalDecision, ApprovalRequest, ApprovalScope, ApprovalState, ErrorCategory,
    ErrorComponent, InboxQuery, Result, ToolApproval, ToolConfirmationRequest,
};
use serde_json::{Value, json};

use super::ScreenPlanAnswer;
use super::screen_plan::denied_message;
use crate::audit::HarnessAudit;
use crate::harness::Harness;
use crate::packs::computer_use::{
    COMPUTER_USE_AUDIT_PLAN_DECIDED, COMPUTER_USE_EXECUTE, Filed, PlanBinding, PlanState, PlanStep,
    PreviewedPlan, ScreenPlans, plan_scope,
};

/// The note the inbox requests of a plan's later steps are withdrawn with when a person decides
/// the plan on its card: the plan's decision answers them (spec 3.7 Q8).
const DECIDED_WITH_THE_PLAN: &str = "decided with its screen plan";

/// Where a screen plan stands, as a surface shows its card (spec 3.7 Q8).
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::HeldPlanState;
///
/// assert_ne!(HeldPlanState::Open, HeldPlanState::Approved);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeldPlanState {
    /// Previewed; no one decided it yet: its card takes one answer.
    Open,
    /// A person approved it: its steps run in order, each checked against its preview.
    Approved,
    /// A person denied it: none of its steps runs.
    Denied,
    /// Its approval covers no further step: a step deviated, failed or ran out of order, it was
    /// previewed again, or everything was stopped while its card waited.
    Ended,
}

/// A held `computer_use_execute` that is a step of a previewed screen plan, for a surface
/// whose decision cards park held calls rather than ask a live prompter (spec 3.7 Q8): the
/// plan's first step carries the plan's one card, and the person's answer there decides every
/// step, with the same binding and audit record as a live prompter's.
#[derive(Clone, Debug)]
pub struct HeldScreenPlan {
    /// The plan: its digest, window and steps, targets included.
    pub plan: PreviewedPlan,
    /// Which step the held call is, from 0.
    pub step: usize,
    /// Where the plan stands.
    pub state: HeldPlanState,
    /// The fingerprint each step's `computer_use_execute` is held under, in order: a surface
    /// finds the held calls of one plan by them.
    pub fingerprints: Vec<String>,
    instance: u64,
}

impl HeldScreenPlan {
    /// The plan as the lines of its card: the window, then each step's operation, target and
    /// proposed action, escaped ([`PreviewedPlan::render`]).
    pub fn render(&self) -> String {
        self.plan.render()
    }
}

/// The fingerprint `step`'s `computer_use_execute` is held under when the agent and tool of
/// `request` hold it.
fn step_fingerprint(step: &PlanStep, request: &ApprovalRequest) -> String {
    ToolConfirmationRequest {
        tool_name: COMPUTER_USE_EXECUTE.to_string(),
        function_call_id: None,
        args: step.execute_args(),
        acting: request.acting.clone().map(Box::new),
        origin: request.origin.clone(),
        evidence: Vec::new(),
    }
    .fingerprint()
}

fn plan_conflict(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Conflict,
        "harness.screen_plan.changed",
        message,
    )
}

impl Harness {
    fn screen_plans(&self) -> Option<&Arc<ScreenPlans>> {
        self.agent().runtime().screen_plans.as_ref()
    }

    /// The screen plan a held `computer_use_execute` is a step of (spec 3.7 Q8), for a surface
    /// that parks held calls: `Some` when `request` holds exactly the previewed arguments of a
    /// step of a plan this harness keeps for the request's session, held by the same agent and
    /// tool as the plan's other steps would be.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if let Some(held) = harness.held_screen_plan(&request)
    ///     && held.step == 0
    ///     && held.state == HeldPlanState::Open
    /// {
    ///     // One card for the whole plan.
    /// }
    /// ```
    pub fn held_screen_plan(&self, request: &ApprovalRequest) -> Option<HeldScreenPlan> {
        if request.tool_name != COMPUTER_USE_EXECUTE || request.scope.app_name != self.app_name() {
            return None;
        }
        let action_digest = request.args_redacted.get("actionDigest").and_then(Value::as_str)?;
        let scope =
            plan_scope(&request.scope.app_name, &request.scope.user_id, &request.scope.session_id);
        let member = self.screen_plans()?.member(&scope, action_digest)?;
        let fingerprints: Vec<String> =
            member.plan.steps.iter().map(|step| step_fingerprint(step, request)).collect();
        // The fingerprint binds the call's arguments, agent and tool: only the exact step is
        // answered from the plan.
        let step =
            fingerprints.iter().position(|fingerprint| *fingerprint == request.fingerprint)?;
        let state = match member.state {
            PlanState::Previewed => HeldPlanState::Open,
            PlanState::Approved { .. } => HeldPlanState::Approved,
            PlanState::Denied { .. } => HeldPlanState::Denied,
            PlanState::Ended { .. } => HeldPlanState::Ended,
        };
        Some(HeldScreenPlan {
            plan: member.plan,
            step,
            state,
            fingerprints,
            instance: member.instance,
        })
    }

    /// Decides the screen plan whose first step `request` holds, as `by` answered its card
    /// (spec 3.7 Q8), and returns the decision to record on `request`: an approval runs the
    /// step with the plan's binding, `plan: { digest, step: 1 }`, as a live prompter's does.
    ///
    /// The plan's later steps are answered here: their decisions are filed in the approval
    /// store under each step's fingerprint — an approval with the step's own binding, or the
    /// denial — and the open inbox requests of steps already held are withdrawn. Each approved
    /// step still runs only in order and only after a fresh preview finds the target the card
    /// showed; a deviation ends the plan, its filed decisions are taken back, and the step is
    /// held again for a fresh card. The decision is recorded as
    /// [`COMPUTER_USE_AUDIT_PLAN_DECIDED`] with `by` as the actor. It is never a standing rule.
    ///
    /// Record the returned decision on `request` next; when that fails, call
    /// [`revert_held_screen_plan`](Self::revert_held_screen_plan).
    ///
    /// # Errors
    ///
    /// `harness.screen_plan.changed` when `request` opens no plan this harness keeps, the plan
    /// is no longer `digest` or was decided already; `harness.decision.invalid` when the
    /// answer approves a plan whose card was not shown in full; the store's or the audit
    /// trail's error.
    pub async fn decide_held_screen_plan(
        &self,
        request: &ApprovalRequest,
        digest: &str,
        answer: ScreenPlanAnswer,
        by: &SubjectRef,
    ) -> Result<ApprovalDecision> {
        let held =
            self.held_screen_plan(request).filter(|held| held.step == 0).ok_or_else(|| {
                plan_conflict("the call no longer opens a screen plan this harness keeps")
            })?;
        if held.plan.digest != digest {
            return Err(plan_conflict(
                "the screen plan was previewed again since its card was shown; the new preview \
                 needs its own answer",
            ));
        }
        if held.state != HeldPlanState::Open {
            return Err(plan_conflict("the screen plan was decided already"));
        }
        let plans = self.screen_plans().ok_or_else(|| plan_conflict("no screen plans are kept"))?;
        let runtime = self.agent().runtime();
        let store = runtime.approval_store().clone();
        let scope = request.scope.clone();
        let plan = &held.plan;
        let (state, label, later) = match &answer {
            ScreenPlanAnswer::Approve => {
                let n = plan.steps.len();
                (PlanState::Approved { answered: n, ran: 0 }, "approve", None)
            }
            ScreenPlanAnswer::Deny { message } => {
                let message = message.clone().unwrap_or_else(|| denied_message(plan));
                (PlanState::Denied { message: Some(message.clone()) }, "deny", Some(message))
            }
        };
        let filed = Filed {
            store: store.clone(),
            scope: scope.clone(),
            fingerprints: held.fingerprints[1..].to_vec(),
        };
        let book = plan_scope(&scope.app_name, &scope.user_id, &scope.session_id);
        if !plans.decide_held(&book, &plan.digest, held.instance, state, filed) {
            return Err(plan_conflict("the screen plan changed while it was decided"));
        }
        let decided = self.file_held_steps(&held, &scope, later.as_deref()).await;
        if let Err(error) = decided {
            self.revert_held_screen_plan(request, digest).await;
            return Err(error);
        }
        if let Some(trail) = self.runner().audit_trail().or(runtime.audit_trail()) {
            let call_id = store
                .pending(&scope)
                .await
                .ok()
                .and_then(|pending| {
                    pending.into_iter().find(|entry| entry.fingerprint == request.fingerprint)
                })
                .and_then(|entry| entry.request.function_call_id);
            let steps: Vec<&str> =
                plan.steps.iter().map(|step| step.action_digest.as_str()).collect();
            let audit = HarnessAudit::new(
                trail.clone(),
                AuditActor::subject(by.clone()),
                &scope.session_id,
            );
            let recorded = audit
                .record(
                    COMPUTER_USE_AUDIT_PLAN_DECIDED,
                    json!({
                        "planDigest": plan.digest,
                        "window": plan.window,
                        "steps": steps,
                        "functionCallId": call_id,
                        "approvalId": request.id,
                        "answer": label,
                    }),
                )
                .await;
            if let Err(error) = recorded {
                self.revert_held_screen_plan(request, digest).await;
                return Err(error);
            }
        }
        Ok(match later {
            None => ApprovalDecision::edit(
                by.clone(),
                PlanBinding::bind(&plan.steps[0].execute_args(), &plan.digest, 1),
            ),
            Some(message) => ApprovalDecision::reject(by.clone(), message),
        })
    }

    /// Files the decisions of `held`'s later steps — the denial `denied`, or each step's
    /// approval with its binding — and withdraws the open inbox requests of those steps.
    async fn file_held_steps(
        &self,
        held: &HeldScreenPlan,
        scope: &ApprovalScope,
        denied: Option<&str>,
    ) -> Result<()> {
        let runtime = self.agent().runtime();
        let plan = &held.plan;
        for (index, (step, fingerprint)) in
            plan.steps.iter().zip(&held.fingerprints).enumerate().skip(1)
        {
            let approval = match denied {
                Some(message) => ToolApproval::deny().with_message(message),
                None => ToolApproval::approve().with_edited_args(PlanBinding::bind(
                    &step.execute_args(),
                    &plan.digest,
                    index + 1,
                )),
            };
            runtime.approval_store().decide(scope, fingerprint, approval).await?;
        }
        let Some(service) = runtime.approval_service() else { return Ok(()) };
        let later = &held.fingerprints[1..];
        let mut query =
            InboxQuery::default().with_session(scope).with_states([ApprovalState::Pending]);
        loop {
            let page = service.inbox(query.clone()).await?;
            for item in page.items.iter().filter(|item| later.contains(&item.fingerprint)) {
                if let Err(error) =
                    service.withdraw(&item.id, Some(DECIDED_WITH_THE_PLAN.into())).await
                {
                    tracing::warn!(error = %error, decision.id = %item.id, "a screen plan step's inbox request could not be withdrawn");
                }
            }
            match page.next_cursor {
                Some(cursor) => query = query.with_cursor(Some(cursor)),
                None => break,
            }
        }
        Ok(())
    }

    /// Takes back a decision [`decide_held_screen_plan`](Self::decide_held_screen_plan) made
    /// that could not be recorded on the held call, such as when another answer was recorded
    /// first: the plan waits for a decision again and its filed decisions are taken back.
    pub async fn revert_held_screen_plan(&self, request: &ApprovalRequest, digest: &str) {
        let Some(plans) = self.screen_plans() else { return };
        let Some(held) = self.held_screen_plan(request) else { return };
        if held.plan.digest != digest {
            return;
        }
        let scope =
            plan_scope(&request.scope.app_name, &request.scope.user_id, &request.scope.session_id);
        if let Some(filed) = plans.revert(&scope, digest, held.instance) {
            filed.withdraw().await;
        }
    }

    /// Ends the screen plan `request` is a step of, whatever its state, for `reason`: its
    /// card closes, it answers no further step, and what was filed for its steps is taken
    /// back. A surface ends a plan whose card is open when everything is stopped (spec 3.7
    /// V-I17): previewing the actions again makes a fresh card.
    pub async fn end_held_screen_plan(
        &self,
        request: &ApprovalRequest,
        digest: &str,
        reason: &str,
    ) {
        let Some(plans) = self.screen_plans() else { return };
        let scope =
            plan_scope(&request.scope.app_name, &request.scope.user_id, &request.scope.session_id);
        if let Some(filed) = plans.close(&scope, digest, reason) {
            filed.withdraw().await;
        }
    }
}
