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
        Self { runtime, authorizer: Arc::new(authorizer), checkpointer: None }
    }

    /// The checkpointer the reference graph's interrupts are kept in (default: in memory).
    #[must_use]
    pub fn with_checkpointer(mut self, checkpointer: Arc<dyn Checkpointer>) -> Self {
        self.checkpointer = Some(checkpointer);
        self
    }

    /// Builds the three tools.
    pub(crate) fn into_runtime(self) -> ComputerUseTools {
        let shared = Arc::new(Shared {
            runtime: self.runtime,
            authorizer: self.authorizer,
            checkpointer: self
                .checkpointer
                .unwrap_or_else(|| Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>),
            previews: Mutex::new(HashMap::new()),
        });
        ComputerUseTools {
            tools: vec![
                Arc::new(ObserveTool { shared: shared.clone() }),
                Arc::new(PreviewTool { shared: shared.clone() }),
                Arc::new(ExecuteTool { shared }),
            ],
        }
    }
}

/// What the builder installs for a configured pack.
pub(crate) struct ComputerUseTools {
    pub(crate) tools: Vec<Arc<dyn Tool>>,
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

/// `computer_use_preview` (spec 3.1 §7.24): class `Read`; keeps the preview.
struct PreviewTool {
    shared: Arc<Shared>,
}

#[async_trait]
impl Tool for PreviewTool {
    fn name(&self) -> &str {
        COMPUTER_USE_PREVIEW
    }

    fn description(&self) -> &str {
        "Preview one desktop action without performing it: observe, then ask the runtime \
         what the action would do and whether its policy allows it. Returns actionDigest, \
         policyDigest, route and a summary of the action; pass all three to \
         computer_use_execute, unchanged, to perform it after the user's approval."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "object",
                    "description": "The proposed action as the runtime expects it: tool, operation and its arguments."
                }
            },
            "required": ["action"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let action = args
            .get("action")
            .filter(|action| action.is_object())
            .cloned()
            .ok_or_else(|| invalid("computer_use_preview needs an action object"))?;
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
         action unchanged. The harness asks the user to approve the call before it runs; \
         an action whose digests or description differ from the preview is refused."
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
        let output = match first {
            Ok(output) => output,
            Err(GraphError::Interrupted(interrupted)) => {
                // The graph's digest-bound approval interrupt, answered by the harness
                // approval the person gave for exactly these digests (spec 3.1 §7.24, I59).
                let Some(preview) = interrupted_preview(&interrupted) else {
                    self.shared.forget(&session_id, &action_digest);
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
                    return self
                        .refuse(
                            audit.as_ref(),
                            ctx.as_ref(),
                            digests,
                            "the runtime's preview changed since the approval",
                        )
                        .await;
                }
                let mut resume = State::new();
                resume.insert(
                    "approval".into(),
                    json!({
                        "actionDigest": action_digest,
                        "policyDigest": policy_digest,
                        "runtimeApproved": true,
                    }),
                );
                graph
                    .invoke(
                        resume,
                        ExecutionConfig::new(&thread).with_resume_from(&interrupted.checkpoint_id),
                    )
                    .await
                    .map_err(|error| runtime_error("execution", error))?
            }
            Err(error) => {
                self.shared.forget(&session_id, &action_digest);
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
                    }),
                )
                .await?;
        }
        Ok(json!({
            "status": status,
            "receiptId": receipt_id,
            "verified": verified,
            "committed": committed,
            "verificationDetail": result.get("verificationDetail").cloned().unwrap_or(Value::Null),
            "reason": result.get("reason").cloned().unwrap_or(Value::Null),
            "actionDigest": action_digest,
            "policyDigest": policy_digest,
        }))
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
