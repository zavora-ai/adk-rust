//! The computer-use pack (spec 3.1 §7.24, feature `computer-use`).
//!
//! adk-computer-use's reference graph does the work: parallel observation, a preview, a
//! digest-bound approval interrupt, one executor, verification. The pack is classes and
//! wiring: `computer_use_observe` and `computer_use_preview` are class `Read`, and
//! `computer_use_execute` is class [`ToolKind::ComputerUse`](crate::ToolKind) — denied in
//! ReadOnly and Plan, held for a non-waivable one-shot approval in Ask and Auto, never
//! covered by a standing rule, a `tool_approvals` entry or a hook (spec 3.1 §9.2 I59).
//!
//! The approval the person answers is the harness's, for `computer_use_execute` with the
//! runtime's action and policy digests as its arguments. Execution runs the reference graph;
//! when the graph interrupts for approval, the interrupt is resumed with those digests as
//! the runtime-held authority only when they equal the approved arguments, and the graph
//! checks them again before its single `execute_action`. Any other digest, an `action`
//! that differs from the kept preview, or a preview this process does not hold runs nothing.
//!
//! `computer_use_preview { actions }` previews several actions as screen plans of up to N
//! actions in one window, each approved once (spec 3.7 Q8): see [`PreviewedPlan`].

mod plan;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_computer_use::{
    ActionPreview, ComputerUseRuntime, ScopeAuthorizer, build_reference_graph_with_checkpointer,
};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext, ToolEffect};
use adk_graph::{Checkpointer, ExecutionConfig, GraphError, Interrupt, MemoryCheckpointer, State};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::audit::HarnessAudit;
pub use plan::{DEFAULT_PLAN_MAX_ACTIONS, PlanStep, PlanWindow, PreviewedPlan};
pub(crate) use plan::{PlanState, ScreenPlans, scope as plan_scope};

/// `computer_use_observe`: the three observation frames (spec 3.1 §7.24).
pub const COMPUTER_USE_OBSERVE: &str = "computer_use_observe";
/// `computer_use_preview`: observation, then the runtime's preview of one action.
pub const COMPUTER_USE_PREVIEW: &str = "computer_use_preview";
/// `computer_use_execute`: the previewed action through the reference graph, once.
pub const COMPUTER_USE_EXECUTE: &str = "computer_use_execute";

/// The pack's tool names (spec 3.1 §7.24).
///
/// # Example
///
/// ```rust
/// assert!(adk_harness::packs::computer_use::COMPUTER_USE_TOOL_NAMES.contains(&"computer_use_execute"));
/// ```
pub const COMPUTER_USE_TOOL_NAMES: &[&str] =
    &[COMPUTER_USE_OBSERVE, COMPUTER_USE_PREVIEW, COMPUTER_USE_EXECUTE];

/// Action of the record one preview writes (spec 3.1 §9.3).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::computer_use::COMPUTER_USE_AUDIT_PREVIEWED, "harness.computer_use.previewed");
/// ```
pub const COMPUTER_USE_AUDIT_PREVIEWED: &str = "harness.computer_use.previewed";

/// Action of the record one execution, or its refusal, writes (spec 3.1 §9.3).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::computer_use::COMPUTER_USE_AUDIT_EXECUTED, "harness.computer_use.executed");
/// ```
pub const COMPUTER_USE_AUDIT_EXECUTED: &str = "harness.computer_use.executed";

/// Error code of an execution refused because its digests or its description differ from
/// the kept preview, or no preview is kept (spec 3.1 §7.24, §9.2 I59).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::computer_use::COMPUTER_USE_PREVIEW_MISMATCH_CODE, "harness.computer_use.preview_mismatch");
/// ```
pub const COMPUTER_USE_PREVIEW_MISMATCH_CODE: &str = "harness.computer_use.preview_mismatch";

/// Action of the record a person's decision on a screen plan writes (spec 3.7 Q8).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::computer_use::COMPUTER_USE_AUDIT_PLAN_DECIDED, "harness.computer_use.plan_decided");
/// ```
pub const COMPUTER_USE_AUDIT_PLAN_DECIDED: &str = "harness.computer_use.plan_decided";

/// Error code of a plan step that no longer matches its preview, or that its plan's
/// approval no longer covers: it did not run, and it needs a fresh approval (spec 3.7 Q8).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::computer_use::COMPUTER_USE_PLAN_DEVIATION_CODE, "harness.computer_use.plan_deviation");
/// ```
pub const COMPUTER_USE_PLAN_DEVIATION_CODE: &str = "harness.computer_use.plan_deviation";

/// Whether `name` is one of the pack's tools.
///
/// # Example
///
/// ```rust
/// use adk_harness::packs::computer_use::is_computer_use_tool;
///
/// assert!(is_computer_use_tool("computer_use_preview"));
/// assert!(!is_computer_use_tool("bash"));
/// ```
pub fn is_computer_use_tool(name: &str) -> bool {
    COMPUTER_USE_TOOL_NAMES.contains(&name)
}

/// Upper bound of kept previews per process; the map is cleared beyond it.
const KEPT_PREVIEWS_CAP: usize = 1024;

/// The computer-use pack (spec 3.1 §7.24): a `ComputerUseRuntime` — a `computer-use-mcp`
/// server through `ComputerUseMcpRuntime`, or an in-process implementation — under the
/// harness policy.
///
/// `HarnessAgentBuilder::pack_computer_use` installs it. The checkpointer holds the
/// reference graph's interrupt between the preview and the approved execution; without one
/// the pack keeps an in-memory checkpointer.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_computer_use::ScopeAuthorizer;
/// use adk_harness::{HarnessAgent, packs::computer_use::ComputerUsePack};
///
/// let authorizer = ScopeAuthorizer::new(["computer:plan", "computer:execute:background"]);
/// let harness = HarnessAgent::builder("harness")
///     .model(model)
///     .workspace(workspace)
///     .pack_computer_use(ComputerUsePack::new(runtime, authorizer))
///     .build()?;
/// assert_eq!(harness.info().packs, vec!["computer-use".to_string()]);
/// ```
pub struct ComputerUsePack {
    runtime: Arc<dyn ComputerUseRuntime>,
    authorizer: Arc<ScopeAuthorizer>,
    checkpointer: Option<Arc<dyn Checkpointer>>,
    plan_max_actions: usize,
}

impl std::fmt::Debug for ComputerUsePack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComputerUsePack")
            .field("authorizer", &self.authorizer)
            .field("checkpointer", &self.checkpointer.is_some())
            .finish_non_exhaustive()
    }
}

impl ComputerUsePack {
    /// A pack driving `runtime` under `authorizer`'s `computer:*` scopes.
    pub fn new(runtime: Arc<dyn ComputerUseRuntime>, authorizer: ScopeAuthorizer) -> Self {
        Self {
            runtime,
            authorizer: Arc::new(authorizer),
            checkpointer: None,
            plan_max_actions: DEFAULT_PLAN_MAX_ACTIONS,
        }
    }

    /// The most actions one screen plan holds (default [`DEFAULT_PLAN_MAX_ACTIONS`], at
    /// least 2): `computer_use_preview { actions }` splits a longer run into plans of this
    /// size, each approved on its own (spec 3.7 Q8).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let pack = ComputerUsePack::new(runtime, authorizer).with_plan_max_actions(12);
    /// ```
    #[must_use]
    pub fn with_plan_max_actions(mut self, max: usize) -> Self {
        self.plan_max_actions = max.max(2);
        self
    }

    /// The checkpointer the reference graph's interrupts are kept in (default: in memory).
    #[must_use]
    pub fn with_checkpointer(mut self, checkpointer: Arc<dyn Checkpointer>) -> Self {
        self.checkpointer = Some(checkpointer);
        self
    }

    /// Builds the three tools.
    pub(crate) fn into_runtime(self) -> ComputerUseTools {
        let plans = Arc::new(ScreenPlans::default());
        let shared = Arc::new(Shared {
            runtime: self.runtime,
            authorizer: self.authorizer,
            checkpointer: self
                .checkpointer
                .unwrap_or_else(|| Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>),
            previews: Mutex::new(HashMap::new()),
            plans: plans.clone(),
            plan_max_actions: self.plan_max_actions,
        });
        ComputerUseTools {
            tools: vec![
                Arc::new(ObserveTool { shared: shared.clone() }),
                Arc::new(PreviewTool { shared: shared.clone() }),
                Arc::new(ExecuteTool { shared }),
            ],
            plans,
        }
    }
}

/// What the builder installs for a configured pack.
pub(crate) struct ComputerUseTools {
    pub(crate) tools: Vec<Arc<dyn Tool>>,
    /// The screen plans the prompting handler shows and answers (spec 3.7 Q8).
    pub(crate) plans: Arc<ScreenPlans>,
}

/// A preview kept between `computer_use_preview` and the approved `computer_use_execute`.
#[derive(Clone)]
struct Kept {
    action: Value,
    summary: Value,
    policy_digest: String,
}

struct Shared {
    runtime: Arc<dyn ComputerUseRuntime>,
    authorizer: Arc<ScopeAuthorizer>,
    checkpointer: Arc<dyn Checkpointer>,
    /// Kept previews by (session, action digest).
    previews: Mutex<HashMap<String, Kept>>,
    /// Previewed screen plans (spec 3.7 Q8).
    plans: Arc<ScreenPlans>,
    plan_max_actions: usize,
}

impl Shared {
    fn key(session_id: &str, action_digest: &str) -> String {
        format!("{session_id}\u{1f}{action_digest}")
    }

    fn keep(&self, session_id: &str, action_digest: &str, kept: Kept) {
        let mut previews = self.previews.lock().unwrap_or_else(|e| e.into_inner());
        if previews.len() >= KEPT_PREVIEWS_CAP {
            previews.clear();
        }
        previews.insert(Self::key(session_id, action_digest), kept);
    }

    fn kept(&self, session_id: &str, action_digest: &str) -> Option<Kept> {
        self.previews
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&Self::key(session_id, action_digest))
            .cloned()
    }

    fn forget(&self, session_id: &str, action_digest: &str) {
        self.previews
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&Self::key(session_id, action_digest));
    }

    /// The three observation frames, concurrently (the graph's first step).
    async fn observe(&self) -> Result<Value> {
        let (capabilities, visual, semantic) = tokio::try_join!(
            self.runtime.discover_capabilities(),
            self.runtime.observe_visual(),
            self.runtime.observe_semantic(),
        )
        .map_err(|error| runtime_error("observation", error))?;
        Ok(json!({ "capabilities": capabilities, "visual": visual, "semantic": semantic }))
    }
}

fn runtime_error(what: &str, error: impl std::fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unavailable,
        "harness.computer_use.runtime",
        format!("the computer-use runtime failed during {what}: {error}"),
    )
}

fn invalid(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "harness.computer_use.args",
        message,
    )
}

fn mismatch(reason: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Forbidden,
        COMPUTER_USE_PREVIEW_MISMATCH_CODE,
        format!(
            "{reason}; call computer_use_preview and echo its actionDigest, policyDigest and action exactly"
        ),
    )
}

/// What the model, the prompt and the audit record see of a previewed action.
///
/// # Example
///
/// ```rust,ignore
/// let summary = adk_harness::packs::computer_use::summarize(&preview);
/// assert_eq!(summary["operation"], preview.envelope.operation);
/// ```
pub fn summarize(preview: &ActionPreview) -> Value {
    let envelope = &preview.envelope;
    json!({
        "tool": envelope.tool,
        "operation": envelope.operation,
        "actionClass": envelope.action_class,
        "requestedMode": envelope.requested_mode,
        "targetApp": envelope.target.as_ref().map(|target| target.app_id.clone()),
        "reversible": envelope.reversible,
        "externalSideEffect": envelope.external_side_effect,
    })
}

fn route_of(preview: &ActionPreview) -> &'static str {
    if preview.executable {
        "allowed"
    } else if preview.blocker.as_deref() == Some("approval_required") {
        "approval"
    } else {
        "blocked"
    }
}

/// `computer_use_observe` (spec 3.1 §7.24): class `Read`.
struct ObserveTool {
    shared: Arc<Shared>,
}

#[async_trait]
impl Tool for ObserveTool {
    fn name(&self) -> &str {
        COMPUTER_USE_OBSERVE
    }

    fn description(&self) -> &str {
        "Observe the desktop: the runtime's execution capabilities, a visual frame and a \
         semantic (accessibility) frame, captured together. Changes nothing."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({ "type": "object", "properties": {} }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.shared.observe().await
    }
}

/// `computer_use_preview` (spec 3.1 §7.24): class `Read`; keeps the preview. With `actions`,
/// previews several actions and groups them into screen plans (spec 3.7 Q8).
struct PreviewTool {
    shared: Arc<Shared>,
}

impl PreviewTool {
    /// Previews `actions` on one observation, groups the held ones into screen plans and
    /// keeps every preview and plan for the session.
    async fn preview_plans(&self, ctx: &dyn ToolContext, actions: &Value) -> Result<Value> {
        let actions = actions
            .as_array()
            .filter(|actions| {
                !actions.is_empty()
                    && actions.len() <= plan::PREVIEW_MAX_ACTIONS
                    && actions.iter().all(Value::is_object)
            })
            .ok_or_else(|| {
                invalid(format!(
                    "computer_use_preview needs `actions` as 1 to {} action objects",
                    plan::PREVIEW_MAX_ACTIONS
                ))
            })?;
        let observation = self.shared.observe().await?;
        let mut previewed = Vec::with_capacity(actions.len());
        let mut digests = std::collections::HashSet::new();
        for (index, action) in actions.iter().enumerate() {
            let preview = self
                .shared
                .runtime
                .preview_action(action.clone())
                .await
                .map_err(|error| runtime_error("preview", error))?;
            // One kept preview per action digest, so each step needs its own.
            if !digests.insert(preview.envelope.args_digest.clone()) {
                return Err(invalid(format!(
                    "action {} previews to the same action digest as an earlier one; preview it \
                     again after the earlier one runs",
                    index + 1
                )));
            }
            previewed.push((action.clone(), preview));
        }
        let steps = previewed
            .iter()
            .map(|(action, preview)| {
                let step = PlanStep {
                    action_digest: preview.envelope.args_digest.clone(),
                    policy_digest: preview.policy.policy_digest.clone(),
                    action: summarize(preview),
                    proposed: action.clone(),
                    target: preview.envelope.target.clone(),
                };
                // Only a step the graph stops at its approval interrupt for can be checked
                // against its preview before it runs.
                (step, route_of(preview) == "approval")
            })
            .collect();
        let (plans, positions) = plan::group(steps, self.shared.plan_max_actions);
        let scope = plan_scope(ctx.app_name(), ctx.user_id(), ctx.session_id());
        for plan in &plans {
            self.shared.plans.keep(&scope, plan.clone());
        }
        let audit = HarnessAudit::for_tool(ctx);
        let mut answers = Vec::with_capacity(previewed.len());
        for ((action, preview), position) in previewed.into_iter().zip(positions) {
            let summary = summarize(&preview);
            let route = route_of(&preview);
            let action_digest = preview.envelope.args_digest.clone();
            let policy_digest = preview.policy.policy_digest.clone();
            self.shared.keep(
                ctx.session_id(),
                &action_digest,
                Kept { action, summary: summary.clone(), policy_digest: policy_digest.clone() },
            );
            let plan_digest = position.map(|(plan, _)| plans[plan].digest.clone());
            let step = position.map(|(_, step)| step + 1);
            if let Some(audit) = &audit {
                audit
                    .record(
                        COMPUTER_USE_AUDIT_PREVIEWED,
                        json!({
                            "functionCallId": ctx.function_call_id(),
                            "actionDigest": action_digest,
                            "policyDigest": policy_digest,
                            "route": route,
                            "actionClass": preview.envelope.action_class,
                            "requestedMode": preview.envelope.requested_mode,
                            "planDigest": plan_digest,
                            "step": step,
                        }),
                    )
                    .await?;
            }
            answers.push(json!({
                "actionDigest": action_digest,
                "policyDigest": policy_digest,
                "route": route,
                "blocker": preview.blocker,
                "reasons": preview.policy.reasons,
                "action": summary,
                "plan": plan_digest,
                "step": step,
            }));
        }
        let plans: Vec<Value> = plans
            .iter()
            .map(|plan| {
                json!({ "planDigest": plan.digest, "window": plan.window, "steps": plan.steps.len() })
            })
            .collect();
        Ok(json!({ "previews": answers, "plans": plans, "observation": observation }))
    }
}

#[async_trait]
impl Tool for PreviewTool {
    fn name(&self) -> &str {
        COMPUTER_USE_PREVIEW
    }

    fn description(&self) -> &str {
        "Preview desktop actions without performing them: observe, then ask the runtime what \
         each action would do and whether its policy allows it. Pass `action` for one action: \
         returns actionDigest, policyDigest, route and a summary of the action. Pass `actions` \
         for several in the order they run: returns `previews` in that order, and consecutive \
         actions in one window that need approval form a screen plan the user approves once; \
         each preview names its `plan` and `step`. Pass each preview's actionDigest, \
         policyDigest and action to computer_use_execute, unchanged and in order, to perform \
         it after the user's approval."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "object",
                    "description": "One proposed action as the runtime expects it: tool, operation and its arguments."
                },
                "actions": {
                    "type": "array",
                    "items": { "type": "object" },
                    "description": "Several proposed actions, in the order they run, previewed together as screen plans."
                }
            }
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if let Some(actions) = args.get("actions") {
            return self.preview_plans(ctx.as_ref(), actions).await;
        }
        let action = args
            .get("action")
            .filter(|action| action.is_object())
            .cloned()
            .ok_or_else(|| invalid("computer_use_preview needs an action object or `actions`"))?;
        let observation = self.shared.observe().await?;
        let preview = self
            .shared
            .runtime
            .preview_action(action.clone())
            .await
            .map_err(|error| runtime_error("preview", error))?;
        let summary = summarize(&preview);
        let route = route_of(&preview);
        let action_digest = preview.envelope.args_digest.clone();
        let policy_digest = preview.policy.policy_digest.clone();
        self.shared.keep(
            ctx.session_id(),
            &action_digest,
            Kept { action, summary: summary.clone(), policy_digest: policy_digest.clone() },
        );
        if let Some(audit) = HarnessAudit::for_tool(ctx.as_ref()) {
            audit
                .record(
                    COMPUTER_USE_AUDIT_PREVIEWED,
                    json!({
                        "functionCallId": ctx.function_call_id(),
                        "actionDigest": action_digest,
                        "policyDigest": policy_digest,
                        "route": route,
                        "actionClass": preview.envelope.action_class,
                        "requestedMode": preview.envelope.requested_mode,
                    }),
                )
                .await?;
        }
        Ok(json!({
            "actionDigest": action_digest,
            "policyDigest": policy_digest,
            "route": route,
            "blocker": preview.blocker,
            "reasons": preview.policy.reasons,
            "action": summary,
            "observation": observation,
        }))
    }
}

/// `computer_use_execute` (spec 3.1 §7.24): class `ComputerUse`, never waivable.
struct ExecuteTool {
    shared: Arc<Shared>,
}

/// The plan step an approval answered from a screen plan names in the call's arguments
/// (spec 3.7 Q8): `plan: { digest, step }`, the step counted from 1.
#[derive(Clone, Debug)]
pub(crate) struct PlanBinding {
    pub(crate) digest: String,
    pub(crate) step: usize,
}

impl PlanBinding {
    /// The binding `args` carry, if any; `Err` when `plan` is not a digest and a step.
    fn of(args: &Value) -> std::result::Result<Option<Self>, ()> {
        let Some(plan) = args.get("plan") else { return Ok(None) };
        let digest = plan.get("digest").and_then(Value::as_str).ok_or(())?;
        let step = plan.get("step").and_then(Value::as_u64).filter(|step| *step >= 1).ok_or(())?;
        Ok(Some(Self { digest: digest.to_string(), step: usize::try_from(step).map_err(|_| ())? }))
    }

    /// The step's arguments with the binding added.
    pub(crate) fn bind(args: &Value, digest: &str, step: usize) -> Value {
        let mut bound = args.clone();
        if let Value::Object(map) = &mut bound {
            map.insert("plan".into(), json!({ "digest": digest, "step": step }));
        }
        bound
    }
}

impl ExecuteTool {
    async fn refuse(
        &self,
        audit: Option<&HarnessAudit>,
        ctx: &dyn ToolContext,
        digests: (&str, &str),
        reason: &str,
    ) -> Result<Value> {
        if let Some(audit) = audit {
            audit
                .record(
                    COMPUTER_USE_AUDIT_EXECUTED,
                    json!({
                        "functionCallId": ctx.function_call_id(),
                        "actionDigest": digests.0,
                        "policyDigest": digests.1,
                        "refusal": "previewMismatch",
                        "reason": reason,
                    }),
                )
                .await?;
        }
        Err(mismatch(reason))
    }

    /// Refuses a step its plan's approval does not cover, or that no longer matches its
    /// preview: it did not run and needs a fresh approval (spec 3.7 Q8).
    async fn refuse_step(
        &self,
        audit: Option<&HarnessAudit>,
        ctx: &dyn ToolContext,
        digests: (&str, &str),
        binding: &PlanBinding,
        deviation: &str,
        reason: &str,
    ) -> Result<Value> {
        if let Some(audit) = audit {
            audit
                .record(
                    COMPUTER_USE_AUDIT_EXECUTED,
                    json!({
                        "functionCallId": ctx.function_call_id(),
                        "actionDigest": digests.0,
                        "policyDigest": digests.1,
                        "planDigest": binding.digest,
                        "step": binding.step,
                        "refusal": "planDeviation",
                        "deviation": deviation,
                        "reason": reason,
                    }),
                )
                .await?;
        }
        Err(AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Conflict,
            COMPUTER_USE_PLAN_DEVIATION_CODE,
            format!(
                "step {} of screen plan {} did not run: {reason}. The plan's approval no longer \
                 covers it or any later step; preview the action again, and it is held for a \
                 fresh approval",
                binding.step, binding.digest
            ),
        ))
    }
}

/// The preview an interrupt carries: its data, else the interrupted state's `preview`.
fn interrupted_preview(interrupted: &adk_graph::InterruptedExecution) -> Option<ActionPreview> {
    let from_data = match &interrupted.interrupt {
        Interrupt::Dynamic { data: Some(data), .. } => serde_json::from_value(data.clone()).ok(),
        Interrupt::Dynamic { .. } | Interrupt::Before(_) | Interrupt::After(_) => None,
    };
    from_data.or_else(|| {
        interrupted
            .state
            .get("preview")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    })
}

#[async_trait]
impl Tool for ExecuteTool {
    fn name(&self) -> &str {
        COMPUTER_USE_EXECUTE
    }

    fn description(&self) -> &str {
        "Perform a desktop action previewed by computer_use_preview, exactly once, through \
         the governed reference graph. Pass the preview's actionDigest, policyDigest and \
         action unchanged. The harness asks the user to approve the call before it runs, or \
         once for a whole screen plan; an action whose digests or description differ from \
         the preview is refused, and a plan step whose target moved is held again."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "actionDigest": { "type": "string", "description": "actionDigest from computer_use_preview." },
                "policyDigest": { "type": "string", "description": "policyDigest from computer_use_preview." },
                "action": { "type": "object", "description": "The action summary from computer_use_preview, unchanged." }
            },
            "required": ["actionDigest", "policyDigest", "action"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let audit = HarnessAudit::for_tool(ctx.as_ref());
        let action_digest = args
            .get("actionDigest")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("computer_use_execute needs actionDigest"))?
            .to_string();
        let policy_digest = args
            .get("policyDigest")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("computer_use_execute needs policyDigest"))?
            .to_string();
        let echo = args.get("action").cloned().unwrap_or(Value::Null);
        let binding = PlanBinding::of(&args).map_err(|()| {
            invalid("`plan` names a screen plan's digest and a step number from 1")
        })?;
        let digests = (action_digest.as_str(), policy_digest.as_str());
        let session_id = ctx.session_id().to_string();
        let Some(kept) = self.shared.kept(&session_id, &action_digest) else {
            return self
                .refuse(
                    audit.as_ref(),
                    ctx.as_ref(),
                    digests,
                    "no preview is kept for this action digest in this process",
                )
                .await;
        };
        if kept.policy_digest != policy_digest {
            return self
                .refuse(
                    audit.as_ref(),
                    ctx.as_ref(),
                    digests,
                    "the policy digest differs from the preview's",
                )
                .await;
        }
        if kept.summary != echo {
            return self
                .refuse(
                    audit.as_ref(),
                    ctx.as_ref(),
                    digests,
                    "the action differs from the preview",
                )
                .await;
        }
        // A step answered from a screen plan's approval runs only as that step, in order
        // (spec 3.7 Q8).
        let scope = plan_scope(ctx.app_name(), ctx.user_id(), &session_id);
        let planned = match &binding {
            Some(binding) => {
                match self.shared.plans.begin(&scope, &binding.digest, binding.step - 1, &args) {
                    Ok(step) => Some(step),
                    Err(reason) => {
                        return self
                            .refuse_step(
                                audit.as_ref(),
                                ctx.as_ref(),
                                digests,
                                binding,
                                "notCovered",
                                &reason,
                            )
                            .await;
                    }
                }
            }
            None => None,
        };
        let end_plan = |reason: &str| {
            if let Some(binding) = &binding {
                self.shared.plans.end(&scope, &binding.digest, reason);
            }
        };

        let graph = build_reference_graph_with_checkpointer(
            self.shared.runtime.clone(),
            self.shared.authorizer.clone(),
            Some(self.shared.checkpointer.clone()),
        )
        .map_err(|error| runtime_error("graph build", error))?;
        let thread = format!("cu:{session_id}:{action_digest}:{}", ctx.function_call_id());
        let mut input = State::new();
        input.insert("proposed_action".into(), kept.action.clone());
        let first = graph.invoke(input, ExecutionConfig::new(&thread)).await;
        let mut checked = false;
        let output = match first {
            Ok(output) => output,
            Err(GraphError::Interrupted(interrupted)) => {
                // The graph's digest-bound approval interrupt, answered by the harness
                // approval the person gave for exactly these digests (spec 3.1 §7.24, I59).
                let Some(preview) = interrupted_preview(&interrupted) else {
                    self.shared.forget(&session_id, &action_digest);
                    end_plan("the runtime's approval interrupt carried no preview");
                    return self
                        .refuse(
                            audit.as_ref(),
                            ctx.as_ref(),
                            digests,
                            "the runtime's approval interrupt carried no preview",
                        )
                        .await;
                };
                if preview.envelope.args_digest != action_digest
                    || preview.policy.policy_digest != policy_digest
                {
                    self.shared.forget(&session_id, &action_digest);
                    end_plan("the runtime's preview changed since the approval");
                    return self
                        .refuse(
                            audit.as_ref(),
                            ctx.as_ref(),
                            digests,
                            "the runtime's preview changed since the approval",
                        )
                        .await;
                }
                // The graph previewed the step again on a fresh observation: its target
                // must be the one the plan showed (spec 3.7 Q8).
                if let (Some(binding), Some(planned)) = (&binding, &planned)
                    && let Some(deviation) =
                        plan::deviation(planned.target.as_ref(), preview.envelope.target.as_ref())
                {
                    self.shared.forget(&session_id, &action_digest);
                    end_plan(&format!("step {} deviated: {}", binding.step, deviation.reason));
                    return self
                        .refuse_step(
                            audit.as_ref(),
                            ctx.as_ref(),
                            digests,
                            binding,
                            deviation.kind,
                            &deviation.reason,
                        )
                        .await;
                }
                checked = true;
                let mut resume = State::new();
                resume.insert(
                    "approval".into(),
                    json!({
                        "actionDigest": action_digest,
                        "policyDigest": policy_digest,
                        "runtimeApproved": true,
                    }),
                );
                let resumed = graph
                    .invoke(
                        resume,
                        ExecutionConfig::new(&thread).with_resume_from(&interrupted.checkpoint_id),
                    )
                    .await;
                match resumed {
                    Ok(output) => output,
                    Err(error) => {
                        end_plan("the step's execution failed");
                        return Err(runtime_error("execution", error));
                    }
                }
            }
            Err(error) => {
                self.shared.forget(&session_id, &action_digest);
                end_plan("the step's execution failed");
                return Err(runtime_error("execution", error));
            }
        };
        // One shot: a second execution needs a new preview and a new approval.
        self.shared.forget(&session_id, &action_digest);
        let result = output.get("result").cloned().unwrap_or(Value::Null);
        let receipt_id = result.get("receiptId").cloned().unwrap_or(Value::Null);
        let status = result.get("status").cloned().unwrap_or(Value::Null);
        let verified = output.get("verified").cloned().unwrap_or(json!(false));
        let committed = output.get("committed").cloned().unwrap_or(json!(false));
        // The next step runs under the plan only after this one ran and verified, checked
        // at the approval interrupt; anything else ends the plan.
        if let Some(binding) = &binding {
            let ran = checked && verified == json!(true) && committed == json!(true);
            self.shared.plans.finish(&scope, &binding.digest, binding.step - 1, ran);
        }
        let plan_digest = binding.as_ref().map(|binding| binding.digest.clone());
        let step = binding.as_ref().map(|binding| binding.step);
        if let Some(audit) = &audit {
            audit
                .record(
                    COMPUTER_USE_AUDIT_EXECUTED,
                    json!({
                        "functionCallId": ctx.function_call_id(),
                        "actionDigest": action_digest,
                        "policyDigest": policy_digest,
                        "receiptId": receipt_id,
                        "status": status,
                        "verified": verified,
                        "committed": committed,
                        "planDigest": plan_digest,
                        "step": step,
                    }),
                )
                .await?;
        }
        let mut answer = json!({
            "status": status,
            "receiptId": receipt_id,
            "verified": verified,
            "committed": committed,
            "verificationDetail": result.get("verificationDetail").cloned().unwrap_or(Value::Null),
            "reason": result.get("reason").cloned().unwrap_or(Value::Null),
            "actionDigest": action_digest,
            "policyDigest": policy_digest,
        });
        if let Some(binding) = &binding {
            answer["plan"] = json!({ "digest": binding.digest, "step": binding.step });
        }
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_are_the_three_tools() {
        assert_eq!(COMPUTER_USE_TOOL_NAMES.len(), 3);
        assert!(is_computer_use_tool(COMPUTER_USE_EXECUTE));
        assert!(!is_computer_use_tool("computer_use"));
    }

    #[test]
    fn kept_previews_are_per_session_and_one_shot() {
        let kept =
            Kept { action: json!({}), summary: json!({ "tool": "x" }), policy_digest: "p".into() };
        let shared = Shared {
            runtime: Arc::new(Never),
            authorizer: Arc::new(ScopeAuthorizer::new(["computer:plan"])),
            checkpointer: Arc::new(MemoryCheckpointer::new()),
            previews: Mutex::new(HashMap::new()),
            plans: Arc::new(ScreenPlans::default()),
            plan_max_actions: DEFAULT_PLAN_MAX_ACTIONS,
        };
        shared.keep("s1", "d1", kept);
        assert!(shared.kept("s1", "d1").is_some());
        assert!(shared.kept("s2", "d1").is_none());
        shared.forget("s1", "d1");
        assert!(shared.kept("s1", "d1").is_none());
    }

    struct Never;

    #[async_trait]
    impl ComputerUseRuntime for Never {
        async fn discover_capabilities(
            &self,
        ) -> std::result::Result<Value, adk_computer_use::ComputerUseError> {
            unreachable!()
        }
        async fn observe_visual(
            &self,
        ) -> std::result::Result<Value, adk_computer_use::ComputerUseError> {
            unreachable!()
        }
        async fn observe_semantic(
            &self,
        ) -> std::result::Result<Value, adk_computer_use::ComputerUseError> {
            unreachable!()
        }
        async fn preview_action(
            &self,
            _: Value,
        ) -> std::result::Result<ActionPreview, adk_computer_use::ComputerUseError> {
            unreachable!()
        }
        async fn acquire_lease(
            &self,
            _: &adk_computer_use::ActionEnvelope,
        ) -> std::result::Result<adk_computer_use::ControlLease, adk_computer_use::ComputerUseError>
        {
            unreachable!()
        }
        async fn execute_action(
            &self,
            _: &adk_computer_use::ActionEnvelope,
            _: &adk_computer_use::ControlLease,
            _: Option<&str>,
        ) -> std::result::Result<
            adk_computer_use::ExecutionReceipt,
            adk_computer_use::ComputerUseError,
        > {
            unreachable!()
        }
        async fn verify(
            &self,
            _: &adk_computer_use::ExecutionReceipt,
            _: Option<&adk_computer_use::ActionPostcondition>,
        ) -> std::result::Result<
            adk_computer_use::VerificationOutcome,
            adk_computer_use::ComputerUseError,
        > {
            unreachable!()
        }
    }
}
