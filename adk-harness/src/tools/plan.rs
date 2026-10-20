//! `exit_plan_mode` (spec 3.1 §7.4).

use std::collections::HashSet;
use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::HarnessAudit;
use crate::mode::{HarnessMode, MODE_STATE_KEY, ModeSource, ModeStore, write_mode_change};
use crate::policy::has_harness_layer;

/// Session-state key of the approved plan's display mirror (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::PLAN_STATE_KEY, "harness:plan");
/// ```
pub const PLAN_STATE_KEY: &str = "harness:plan";

/// The approval gate of a plan step (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::PlanGate::default(), adk_harness::PlanGate::None);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanGate {
    /// No gate.
    #[default]
    None,
    /// A person approves before the step runs.
    Approve,
    /// A reviewer member checks the step.
    Review,
}

/// One step of an approved plan (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// use adk_harness::PlanStep;
/// use serde_json::json;
///
/// let step: PlanStep = serde_json::from_value(json!({
///     "id": "1", "title": "Add the test", "instruction": "Write a failing test", "depends_on": []
/// })).unwrap();
/// assert_eq!(step.title, "Add the test");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    /// Unique id within the plan.
    pub id: String,
    /// Short title.
    pub title: String,
    /// What the step does.
    pub instruction: String,
    /// Ids of the steps this one waits for.
    #[serde(default, alias = "depends_on")]
    pub depends_on: Vec<String>,
    /// The step's gate.
    #[serde(default)]
    pub gate: PlanGate,
}

/// The approved plan as mirrored under [`PLAN_STATE_KEY`].
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanRecord<'a> {
    plan: &'a str,
    steps: &'a [PlanStep],
    next_mode: HarnessMode,
    plan_hash: String,
    approved_at: chrono::DateTime<Utc>,
}

fn invalid(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "harness.plan.invalid",
        message,
    )
}

/// `exit_plan_mode`: asks to leave Plan mode with a plan (spec 3.1 §7.4).
///
/// Always declared, so the tool surface and the prompt cache stay stable; the policy
/// denies it outside Plan and holds it for a non-waivable approval in Plan. On approval it
/// writes the mode store with [`ModeSource::PlanApproval`], effective from the next
/// invocation, mirrors `harness:plan` and `harness:mode` into session state, and ends the
/// turn (`skip_summarization`), since the invocation that approved the plan stays in Plan. The
/// approver may edit `next_mode`; edited arguments are checked by the policy again.
/// Auto is never the default target.
///
/// # Example
///
/// ```rust
/// use adk_core::Tool;
/// use std::sync::Arc;
///
/// let tool = adk_harness::ExitPlanModeTool::new(
///     Arc::new(adk_harness::InMemoryModeStore::new()),
///     adk_harness::HarnessMode::Plan,
/// );
/// assert_eq!(tool.name(), "exit_plan_mode");
/// ```
pub struct ExitPlanModeTool {
    mode_store: Arc<dyn ModeStore>,
    default_mode: HarnessMode,
}

impl ExitPlanModeTool {
    /// A tool over `mode_store`, whose sessions start in `default_mode`.
    pub fn new(mode_store: Arc<dyn ModeStore>, default_mode: HarnessMode) -> Self {
        Self { mode_store, default_mode }
    }
}

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        "exit_plan_mode"
    }

    fn description(&self) -> &str {
        "Present your plan and ask the user to approve leaving plan mode. Call it only in \
         plan mode, once your investigation is complete; the user sees the plan and may \
         approve or reject it. On approval your turn ends, and the new mode applies from the \
         user's next message."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "plan": { "type": "string", "description": "The plan, in Markdown." },
                "next_mode": { "type": "string", "enum": ["ask", "auto"], "description": "Mode to switch to; defaults to ask." },
                "steps": {
                    "type": "array",
                    "description": "Optional structured steps.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "title": { "type": "string" },
                            "instruction": { "type": "string" },
                            "depends_on": { "type": "array", "items": { "type": "string" } },
                            "gate": { "type": "string", "enum": ["none", "approve", "review"] }
                        },
                        "required": ["id", "title", "instruction"]
                    }
                }
            },
            "required": ["plan"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let config = ctx
            .run_config()
            .ok_or_else(|| AdkError::tool("exit_plan_mode runs only inside a harness run"))?;
        if !config.tool_policy.as_deref().is_some_and(has_harness_layer) {
            return Err(AdkError::tool(
                "exit_plan_mode refuses to run without the harness policy in the run config",
            ));
        }
        let plan = args
            .get("plan")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|plan| !plan.is_empty())
            .ok_or_else(|| invalid("plan is required"))?;
        let next_mode = match args.get("next_mode").and_then(Value::as_str) {
            None | Some("ask") => HarnessMode::Ask,
            Some("auto") => HarnessMode::Auto,
            Some(other) => {
                return Err(invalid(format!("next_mode must be ask or auto, not '{other}'")));
            }
        };
        let steps: Vec<PlanStep> = match args.get("steps") {
            None | Some(Value::Null) => Vec::new(),
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|error| invalid(format!("steps are malformed: {error}")))?,
        };
        let ids: HashSet<&str> = steps.iter().map(|step| step.id.as_str()).collect();
        if ids.len() != steps.len() {
            return Err(invalid("step ids must be unique"));
        }
        if let Some(step) =
            steps.iter().find(|step| step.depends_on.iter().any(|dep| !ids.contains(dep.as_str())))
        {
            return Err(invalid(format!("step '{}' depends on an unknown step", step.id)));
        }

        let (app, user, session) = (ctx.app_name(), ctx.user_id(), ctx.session_id());
        let current = self.mode_store.get(app, user, session).await?;
        let mode = current
            .as_ref()
            .map_or(self.default_mode, |record| record.mode_for_invocation(ctx.invocation_id()));
        if mode != HarnessMode::Plan {
            return Err(AdkError::tool(format!(
                "exit_plan_mode is only valid in plan mode; the session is in {mode} mode"
            )));
        }
        let audit = HarnessAudit::for_tool(ctx.as_ref());
        let record = write_mode_change(
            self.mode_store.as_ref(),
            self.default_mode,
            (app, user, session),
            next_mode,
            ModeSource::PlanApproval,
            Some(ctx.invocation_id().to_string()),
            audit.as_ref(),
            &[],
        )
        .await?;

        let plan_hash = crate::fsutil::hex_digest(
            json!({ "plan": plan, "steps": steps }).to_string().as_bytes(),
        );
        let mut actions = ctx.actions();
        actions.state_delta.insert(
            MODE_STATE_KEY.to_string(),
            json!({ "mode": record.mode, "revision": record.revision, "setBy": record.set_by }),
        );
        actions.state_delta.insert(
            PLAN_STATE_KEY.to_string(),
            serde_json::to_value(PlanRecord {
                plan,
                steps: &steps,
                next_mode,
                plan_hash: plan_hash.clone(),
                approved_at: Utc::now(),
            })
            .unwrap_or(Value::Null),
        );
        // The approved mode applies from the next invocation, so this one ends here rather
        // than continuing under Plan.
        actions.skip_summarization = true;
        ctx.set_actions(actions);
        tracing::info!(harness.mode = %next_mode, plan.steps = steps.len(), "plan approved");
        Ok(json!({
            "status": "approved",
            "mode": next_mode,
            "effective": "from the user's next turn",
            "steps": steps.len(),
            "planHash": plan_hash,
            "instruction": "The plan is approved. End your turn now; the user's next message starts the work.",
        }))
    }
}
