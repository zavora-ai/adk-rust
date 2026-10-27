//! An agent's screen-action tools, `verb_<name>`, published as `verbs:<app>` (spec 3.3 §2
//! item 7, §7.1).

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind};
use adk_core::{
    AdkError, ApprovalRequest, ApprovalRequestQuery, ApprovalScope, ApprovalState, ReadonlyContext,
    Tool, ToolConfirmationRequest, ToolContext, ToolEffect, Toolset,
};
use adk_harness::{OriginClasses, ToolKind, driver_tool_kind};
use adk_kernel::PartitionId;
use adk_kernel::drivers::EffectClass;
use async_trait::async_trait;
use serde_json::{Value, json};

use super::registry::VerbRegistry;
use super::{Callers, SURFACE_AUDIT_VERB, VerbDone, VerbError, VerbSpec};

/// The tools of the screen actions agents may ask for, for one agent's area (spec 3.3 §2 item
/// 7): `verb_coming_up` reads, `verb_snooze` asks the person every time ([`VerbPolicy`](super::VerbPolicy)).
///
/// An agent sees only its own area's items. Attach the toolset, [`VerbClasses`] and
/// [`VerbPolicy`](super::VerbPolicy) together, as `kernel_drivers` attaches a driver's:
///
/// ```rust,ignore
/// let agent = HarnessAgent::builder("finance")
///     .toolset(Arc::new(registry.toolset("work")))
///     .tool_classes(Arc::new(registry.classes()))
///     .add_operator_policy(Arc::new(registry.policy("work")))
///     .build()?;
/// ```
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::Toolset;
///
/// # fn example(registry: adk_surface::verbs::VerbRegistry) {
/// let toolset = registry.toolset("work");
/// assert_eq!(toolset.name(), "verbs");
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct VerbToolset {
    registry: VerbRegistry,
    area: PartitionId,
}

impl VerbToolset {
    pub(crate) fn new(registry: VerbRegistry, area: PartitionId) -> Self {
        Self { registry, area }
    }

    /// The tools, one per action an agent may ask for.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.registry
            .entries()
            .iter()
            .filter(|entry| entry.spec.callers == Callers::PersonAndAgents)
            .map(|entry| {
                Arc::new(VerbTool::new(&self.registry, &entry.spec, Some(self.area.clone())))
                    as Arc<dyn Tool>
            })
            .collect()
    }
}

#[async_trait]
impl Toolset for VerbToolset {
    fn name(&self) -> &str {
        "verbs"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> adk_core::Result<Vec<Arc<dyn Tool>>> {
        Ok(VerbToolset::tools(self))
    }
}

/// One action as a tool. The person's own calls are governed through the same declaration,
/// so a decision's origin digest is the same whoever calls.
pub(crate) struct VerbTool {
    registry: VerbRegistry,
    verb: String,
    tool_name: String,
    description: String,
    parameters: Value,
    origin: String,
    effect: EffectClass,
    area: Option<PartitionId>,
}

impl VerbTool {
    pub(crate) fn new(registry: &VerbRegistry, spec: &VerbSpec, area: Option<PartitionId>) -> Self {
        Self {
            registry: registry.clone(),
            verb: spec.name.clone(),
            tool_name: spec.tool_name(),
            description: spec.description.clone(),
            parameters: spec.args.clone(),
            origin: registry.origin(),
            effect: spec.effect,
            area,
        }
    }

    /// The record of an agent's call that changed, or tried to change, something (spec 3.3
    /// §11): the agent and invocation as its actor, how the person said yes when they did.
    async fn record(
        &self,
        ctx: &dyn ToolContext,
        args: &Value,
        outcome: Result<&VerbDone, &VerbError>,
    ) {
        let registry = self.registry.inner();
        let Some(trail) = &registry.audit else {
            return;
        };
        let acting = ctx.acting_context();
        let mut draft =
            AuditRecordDraft::new(AuditRecordKind::Other("surface".into()), SURFACE_AUDIT_VERB)
                .actor(AuditActor::new(&acting, ctx.invocation_id()))
                .ext("origin", json!({ "kind": "agent", "agent": ctx.agent_name() }))
                .ext("verb", json!(self.verb))
                .ext("argsDigest", json!(trail.digest(args)))
                .ext("effect", json!(self.effect.to_string()))
                .ext("functionCallId", json!(ctx.function_call_id()))
                .ext("sessionId", json!(ctx.session_id()));
        draft = match outcome {
            Ok(done) => {
                let mut draft = draft.ext("outcome", json!("done"));
                if let Some(target) = &done.target {
                    draft = draft.ext("target", json!(target));
                }
                if let Some(title) = &done.title {
                    draft = draft.ext("title", json!(title));
                }
                draft
            }
            Err(error) => {
                let item = args.get("item").and_then(Value::as_str);
                let mut draft = draft
                    .ext("outcome", json!("refused"))
                    .ext("code", json!(error.code()))
                    .ext("target", json!(item.map(|id| format!("agenda:{id}"))));
                // The item's title, so the record names it as the person knows it (§8).
                let scope = self.registry.scope(ctx.user_id(), self.area.clone());
                if let Some(id) = item
                    && let Ok(found) = scope.item(id).await
                {
                    draft = draft.ext("title", json!(found.title));
                }
                draft
            }
        };
        if let Some((approval, answered)) = self.answered(ctx, args).await {
            draft = draft.ext("approvalId", json!(approval));
            if let Some(answered) = answered {
                draft = draft.ext("answered", answered);
            }
        }
        if let Err(error) = trail.record(draft).await {
            tracing::warn!(error = %error, verb = %self.verb, "the screen action's record failed");
        }
    }

    /// The decision that let this call with `args` run, and where the person gave it, when
    /// the registry reads the inbox: the approved request of this session for this very call —
    /// its fingerprint, or the arguments the person's Edit gave it — the newest if several.
    async fn answered(
        &self,
        ctx: &dyn ToolContext,
        args: &Value,
    ) -> Option<(String, Option<Value>)> {
        let registry = self.registry.inner();
        let store = registry.approvals.as_ref()?;
        let scope = ApprovalScope::new(&registry.app, ctx.user_id(), ctx.session_id());
        let acting = ctx.acting_context();
        let fingerprint = ToolConfirmationRequest::for_tool(self, args.clone())
            .with_acting_context(acting.clone())
            .fingerprint();
        let this_call = |request: &ApprovalRequest| {
            request.tool_name == self.tool_name
                && request.origin.as_ref().is_some_and(|origin| origin.source == self.origin)
                && request.acting.as_ref().is_some_and(|by| by.agent.id == acting.agent.id)
                && (request.fingerprint == fingerprint
                    || request
                        .decisions
                        .last()
                        .and_then(|decision| decision.edited_args.as_ref())
                        .is_some_and(|edited| edited == args))
        };
        let mut query = ApprovalRequestQuery::default()
            .with_session(&scope)
            .with_states([ApprovalState::Approved]);
        let mut found: Option<ApprovalRequest> = None;
        loop {
            let page = store.list_requests(&query).await.ok()?;
            let full = page.len() >= query.limit;
            let next = page.last().map(ApprovalRequest::cursor);
            for request in page.into_iter().filter(|request| this_call(request)) {
                if found.as_ref().is_none_or(|newest| request.resolved_at > newest.resolved_at) {
                    found = Some(request);
                }
            }
            match next {
                Some(cursor) if full => query = query.with_after(Some(cursor)),
                _ => break,
            }
        }
        let request = found?;
        let answered = match &registry.answers {
            Some(answers) => answers.find(&request.id).await.ok().flatten().map(|record| {
                json!({
                    "surface": record.via.surface,
                    "origin": record.via.origin,
                    "answer": record.answer,
                })
            }),
            None => None,
        };
        Some((request.id, answered))
    }
}

#[async_trait]
impl Tool for VerbTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(self.parameters.clone())
    }

    fn origin(&self) -> &str {
        &self.origin
    }

    fn effect(&self) -> ToolEffect {
        self.effect.tool_effect()
    }

    fn is_read_only(&self) -> bool {
        self.effect == EffectClass::Read
    }

    fn is_concurrency_safe(&self) -> bool {
        self.effect == EffectClass::Read
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let Some(entry) = self.registry.entry(&self.verb) else {
            return Err(VerbError::Unknown { verb: self.verb.clone() }.into());
        };
        let scope = self.registry.scope(ctx.user_id(), self.area.clone());
        let reads = entry.spec.reads();
        match entry.handler.run(&scope, &args).await {
            Ok(done) => {
                if !reads {
                    self.record(ctx.as_ref(), &args, Ok(&done)).await;
                }
                Ok(json!({ "status": "done", "text": done.text, "result": done.result }))
            }
            Err(error) => {
                if !reads {
                    self.record(ctx.as_ref(), &args, Err(&error)).await;
                }
                Err(AdkError::from(error))
            }
        }
    }
}

/// The classes of the action tools, from their declared effects (spec 3.2 §7.7): a read is a
/// trusted read-only tool, every other action a trusted tool with effects, which the modes ask
/// about — and which [`VerbPolicy`](super::VerbPolicy) holds whatever the mode.
///
/// # Example
///
/// ```rust,no_run
/// use adk_harness::OriginClasses;
///
/// # fn example(registry: adk_surface::verbs::VerbRegistry) {
/// let classes = registry.classes();
/// assert!(classes.class_of("verbs:adk-rust", "verb_snooze").is_some());
/// assert!(classes.class_of("mcp:calendar", "verb_snooze").is_none());
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct VerbClasses {
    registry: VerbRegistry,
}

impl VerbClasses {
    pub(crate) fn new(registry: VerbRegistry) -> Self {
        Self { registry }
    }
}

impl OriginClasses for VerbClasses {
    fn class_of(&self, source: &str, tool: &str) -> Option<ToolKind> {
        if source != self.registry.origin() {
            return None;
        }
        self.registry.entry_for_tool(tool).map(|entry| driver_tool_kind(entry.spec.effect))
    }
}
