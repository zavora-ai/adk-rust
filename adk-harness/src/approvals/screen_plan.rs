//! Screen-plan cards: the steps of a previewed screen plan approved with one answer
//! (spec 3.7 Q8), as change-set cards are for edits (spec 3.1 §7.26).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adk_core::{Result, ToolApproval, ToolConfirmationRequest};
use serde_json::{Value, json};

use super::prompt::{PromptingConfirmationHandler, refuse};
use crate::mode::HarnessMode;
use crate::packs::computer_use::{
    COMPUTER_USE_AUDIT_PLAN_DECIDED, COMPUTER_USE_EXECUTE, PlanBinding, PlanState, PreviewedPlan,
    ScreenPlans, plan_scope,
};
use crate::policy::{HarnessPolicy, PromptContext};

/// Default byte budget of [`RenderedScreenPlan::body`].
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::approvals::SCREEN_PLAN_MAX_BYTES, 16 * 1024);
/// ```
pub const SCREEN_PLAN_MAX_BYTES: usize = 16 * 1024;

/// A screen plan rendered for a person: the window, then each step's operation, target and
/// proposed action, escaped. A body cut to the budget cannot be approved until
/// [`expand`](Self::expand) has shown it whole.
#[derive(Clone, Debug)]
pub struct RenderedScreenPlan {
    /// One line naming the plan's size and window; never model-written text.
    pub title: String,
    /// The steps, cut to [`SCREEN_PLAN_MAX_BYTES`].
    pub body: String,
    /// Whether [`body`](Self::body) was cut.
    pub truncated: bool,
    full: String,
    expanded: Arc<AtomicBool>,
}

impl RenderedScreenPlan {
    /// Renders `plan` with a body of at most `max_bytes` bytes.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::approvals::RenderedScreenPlan;
    /// use adk_harness::packs::computer_use::{PlanStep, PlanWindow, PreviewedPlan};
    /// use serde_json::json;
    ///
    /// let window = PlanWindow { app_id: "forms".into(), window_id: None, window_title_digest: None, display_id: None };
    /// let step = |text: &str| PlanStep {
    ///     action_digest: text.into(),
    ///     policy_digest: "p".into(),
    ///     action: json!({ "operation": "type" }),
    ///     proposed: json!({ "operation": "type", "text": text }),
    ///     target: None,
    /// };
    /// let plan = PreviewedPlan { digest: "sha256:ab".into(), window, steps: vec![step("Ada"), step("Lovelace")] };
    /// let rendered = RenderedScreenPlan::render(&plan, 40);
    /// assert!(rendered.truncated && !rendered.can_approve());
    /// assert!(rendered.expand().contains("Lovelace"));
    /// assert!(rendered.can_approve());
    /// ```
    pub fn render(plan: &PreviewedPlan, max_bytes: usize) -> Self {
        let full = plan.render();
        let (body, truncated) = if full.len() <= max_bytes {
            (full.clone(), false)
        } else {
            let end = full.floor_char_boundary(max_bytes);
            (format!("{}… [truncated: {} of {} bytes shown]", &full[..end], end, full.len()), true)
        };
        Self {
            title: format!("Screen plan: {} desktop actions in {}", plan.steps.len(), plan.window),
            body,
            truncated,
            full,
            expanded: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The whole body. Showing it lets a truncated plan be approved.
    pub fn expand(&self) -> &str {
        self.expanded.store(true, Ordering::SeqCst);
        &self.full
    }

    /// Whether an approval of this rendering may stand: the body was shown in full.
    pub fn can_approve(&self) -> bool {
        !self.truncated || self.expanded.load(Ordering::SeqCst)
    }
}

/// What a screen-plan card shows (spec 3.7 Q8): every step of one previewed plan, decided
/// with one answer.
#[derive(Clone, Debug)]
pub struct ScreenPlanPrompt {
    /// The plan: its digest, window and steps.
    pub plan: PreviewedPlan,
    /// The session's mode.
    pub mode: HarnessMode,
    /// The acting agent.
    pub agent: String,
    /// The plan rendered for display.
    pub rendered: RenderedScreenPlan,
}

/// A person's answer to a [`ScreenPlanPrompt`].
///
/// # Example
///
/// ```rust
/// use adk_harness::approvals::ScreenPlanAnswer;
///
/// let answer = ScreenPlanAnswer::Deny { message: Some("not that form".into()) };
/// assert!(matches!(answer, ScreenPlanAnswer::Deny { .. }));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub enum ScreenPlanAnswer {
    /// Run the plan's steps, each once, in order, each checked against its preview before
    /// it runs.
    Approve,
    /// Run none of them; the message is reported to the model.
    Deny {
        /// Reported to the model.
        message: Option<String>,
    },
}

impl PromptingConfirmationHandler {
    /// Answers a held `computer_use_execute` from its screen plan (spec 3.7 Q8): the next
    /// step of an approved plan is answered from the plan's decision; the first step of a
    /// plan no one decided yet shows the plan's card. `None` leaves the call to its own
    /// prompt: no plan, arguments that differ from the previewed step, a step out of order,
    /// a plan whose approval ended, or a prompter that shows calls one at a time.
    ///
    /// # Errors
    ///
    /// Returns the prompter's error, a freeze, or the audit trail's error.
    pub(super) async fn screen_plan(
        &self,
        context: &PromptContext,
        request: &ToolConfirmationRequest,
        policy: &HarnessPolicy,
        mode: HarnessMode,
        turn: (&mut usize, &mut bool),
    ) -> Result<Option<ToolApproval>> {
        let (prompts, deny_all) = turn;
        let Some(plans) = &self.screen_plans else { return Ok(None) };
        if request.tool_name != COMPUTER_USE_EXECUTE {
            return Ok(None);
        }
        let Some(action_digest) = request.args.get("actionDigest").and_then(Value::as_str) else {
            return Ok(None);
        };
        let scope = plan_scope(&context.app_name, &context.user_id, &context.session_id);
        let Some(member) = plans.member(&scope, action_digest) else { return Ok(None) };
        let plan = &member.plan;
        // Only the exact arguments of the plan's next step are answered from the plan.
        let is_step = |index: usize| {
            plan.steps.get(index).is_some_and(|step| step.execute_args() == request.args)
        };
        let answered = match &member.state {
            PlanState::Approved { answered, .. } => *answered,
            PlanState::Denied { message } => {
                return Ok(Some(refuse(message.clone().unwrap_or_else(|| denied_message(plan)))));
            }
            PlanState::Ended { .. } => return Ok(None),
            PlanState::Previewed if !is_step(0) => {
                // A later step asked about first: the plan no longer runs as previewed.
                plans.end(&scope, &plan.digest, "a step was asked about before the plan");
                return Ok(None);
            }
            PlanState::Previewed if *prompts >= self.per_turn_cap => return Ok(None),
            PlanState::Previewed => {
                let agent =
                    super::RenderedCall::render(request, policy.isolation_status(), 0).agent;
                let rendered = RenderedScreenPlan::render(plan, SCREEN_PLAN_MAX_BYTES);
                let prompt = ScreenPlanPrompt {
                    plan: plan.clone(),
                    mode,
                    agent,
                    rendered: rendered.clone(),
                };
                let answer = tokio::select! {
                    answer = self.prompter.prompt_screen_plan(prompt) => answer?,
                    error = super::batch::frozen(&context.governance) => {
                        tracing::warn!("execution was frozen while a person was deciding a screen plan; its steps do not run");
                        return Err(error);
                    }
                };
                let Some(answer) = answer else {
                    plans.end(&scope, &plan.digest, "the prompter shows calls one at a time");
                    return Ok(None);
                };
                *prompts += 1;
                let answer = match answer {
                    ScreenPlanAnswer::Approve if !rendered.can_approve() => {
                        ScreenPlanAnswer::Deny {
                            message: Some(
                                "the screen plan was approved without showing all of it".into(),
                            ),
                        }
                    }
                    answer => answer,
                };
                let (state, label) = match &answer {
                    ScreenPlanAnswer::Approve => {
                        (PlanState::Approved { answered: 0, ran: 0 }, "approve")
                    }
                    ScreenPlanAnswer::Deny { message } => {
                        (PlanState::Denied { message: message.clone() }, "deny")
                    }
                };
                if !plans.settle(&scope, &plan.digest, member.instance, state) {
                    return Ok(Some(refuse(
                        "the screen plan was previewed again while it was decided; the new \
                         preview needs its own approval",
                    )));
                }
                if let Some(audit) = context.audit() {
                    let steps: Vec<&str> =
                        plan.steps.iter().map(|step| step.action_digest.as_str()).collect();
                    audit
                        .record(
                            COMPUTER_USE_AUDIT_PLAN_DECIDED,
                            json!({
                                "planDigest": plan.digest,
                                "window": plan.window,
                                "steps": steps,
                                "functionCallId": request.function_call_id,
                                "answer": label,
                            }),
                        )
                        .await?;
                }
                if let ScreenPlanAnswer::Deny { message } = answer {
                    // After a person denies a call, the rest of the turn's calls are denied
                    // (I19).
                    *deny_all = true;
                    return Ok(Some(refuse(message.unwrap_or_else(|| denied_message(plan)))));
                }
                0
            }
        };
        if !is_step(answered) || !plans.answer(&scope, &plan.digest, answered) {
            return Ok(None);
        }
        // The step's own arguments with the plan's digest and the step number, so the
        // call's fingerprint and its execution bind the plan that was shown.
        let args = PlanBinding::bind(&request.args, &plan.digest, answered + 1);
        Ok(Some(ToolApproval::approve().with_edited_args(args)))
    }

    /// Answers a held `computer_use_execute` of one of `plans` from the plan's decision.
    pub(crate) fn with_screen_plans(mut self, plans: Arc<ScreenPlans>) -> Self {
        self.screen_plans = Some(plans);
        self
    }
}

pub(super) fn denied_message(plan: &PreviewedPlan) -> String {
    format!("the person denied screen plan {}", plan.digest)
}
