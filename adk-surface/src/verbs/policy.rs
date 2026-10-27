//! How an agent's screen actions are governed and shown (spec 3.3 §7.1, §7.2, owner question
//! 22): held for a person every time, at most three waiting, on a card that offers Allow once ·
//! Edit · Deny and never **Always allow**.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_core::{
    ApprovalAction, ApprovalCall, ApprovalRequest, ApprovalRequestQuery, ApprovalRisk,
    ApprovalRoute, ApprovalRouter, Part, PolicyCall, PolicyDecision, ToolPolicy, ToolPolicyRequest,
};
use adk_kernel::PartitionId;
use adk_kernel::drivers::EffectClass;
use adk_session::GetRequest;
use async_trait::async_trait;

use super::registry::VerbRegistry;
use super::{Callers, MAX_WAITING, verbs_principal, words};
use crate::hints::{CardHint, CardHints};
use crate::model::{CardDetail, Reversibility};

/// The policy reference of [`VerbPolicy`] in audit records: `tool_policy:adk-surface/verbs`.
pub const VERB_POLICY_REF: &str = "adk-surface/verbs";

/// Decides calls to screen-action tools (spec 3.3 §2 item 7, owner questions 22 and 23).
///
/// Attach it with `HarnessAgentBuilder::add_operator_policy`: it composes with the harness's
/// policy through `StrictestPolicy`, so no mode, standing rule or earlier approval allows an
/// agent's change (the spec 3.2 K-I9 pattern).
///
/// | Call | Decision |
/// |---|---|
/// | A tool not published as `verbs:<app>` | Allow (not this layer's call) |
/// | By `urn:adk:agent:<app>/surface.verbs`, a person's own action | Allow |
/// | An action that only reads (`coming_up`) | Allow |
/// | An action only a person takes | Deny |
/// | From an agent with [`MAX_WAITING`] requests waiting already | Deny: "Finance already has 3 changes waiting for you" |
/// | An action that would be refused now: not found, in another area, started, moved | Deny, before any card |
/// | Any other action | RequireApproval, every call, with the kernel's words: "Move News digest to 19:00" |
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// # fn example(registry: adk_surface::verbs::VerbRegistry, builder: adk_harness::HarnessAgentBuilder) {
/// let builder = builder
///     .toolset(Arc::new(registry.toolset("work")))
///     .tool_classes(Arc::new(registry.classes()))
///     .add_operator_policy(Arc::new(registry.policy("work")));
/// # let _ = builder;
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct VerbPolicy {
    registry: VerbRegistry,
    area: Option<PartitionId>,
}

impl VerbPolicy {
    pub(crate) fn new(registry: VerbRegistry, area: Option<PartitionId>) -> Self {
        Self { registry, area }
    }

    /// How many of `agent`'s screen actions wait for a person.
    async fn waiting(&self, agent: &adk_core::AgentId) -> usize {
        let registry = self.registry.inner();
        let Some(store) = &registry.approvals else {
            return 0;
        };
        let query = ApprovalRequestQuery::default().open_only().with_app_name(&registry.app);
        let origin = self.registry.origin();
        match store.list_requests(&query).await {
            Ok(requests) => requests
                .iter()
                .filter(|request| {
                    request.origin.as_ref().is_some_and(|by| by.source == origin)
                        && request.acting.as_ref().is_some_and(|acting| acting.agent.id == *agent)
                })
                .count(),
            Err(error) => {
                tracing::warn!(error = %error, "the waiting screen actions could not be counted");
                0
            }
        }
    }
}

#[async_trait]
impl ToolPolicy for VerbPolicy {
    async fn evaluate(&self, _request: &ToolPolicyRequest) -> PolicyDecision {
        PolicyDecision::Allow
    }

    async fn evaluate_call(
        &self,
        request: &ToolPolicyRequest,
        call: &PolicyCall<'_>,
    ) -> PolicyDecision {
        let origin = self.registry.origin();
        if call.origin.is_none_or(|by| by.source != origin) {
            return PolicyDecision::Allow;
        }
        let person = verbs_principal(self.registry.app_name()).id;
        let agent = request.acting.as_ref().map(|acting| acting.agent.id.clone());
        if agent.as_ref() == Some(&person) {
            return PolicyDecision::Allow;
        }
        let Some(entry) = self.registry.entry_for_tool(&request.tool_name) else {
            return PolicyDecision::deny(format!(
                "there is no screen action `{}` on this home",
                request.tool_name
            ));
        };
        if entry.spec.reads() {
            return PolicyDecision::Allow;
        }
        if entry.spec.callers == Callers::PersonOnly {
            return PolicyDecision::deny(format!(
                "only a person takes `{}`, on a screen",
                entry.spec.name
            ));
        }
        if let Some(agent) = &agent
            && self.waiting(agent).await >= MAX_WAITING
        {
            let name = self.registry.display_name(&request.agent_name);
            return PolicyDecision::deny(words::already_waiting(&name, MAX_WAITING));
        }
        let scope = self.registry.scope(&request.user_id, self.area.clone());
        match entry.handler.describe(&scope, &request.args).await {
            Ok(text) => PolicyDecision::require_approval(text),
            Err(error) => PolicyDecision::deny(error.to_string()),
        }
    }

    fn policy_ref(&self) -> Option<String> {
        Some(VERB_POLICY_REF.to_string())
    }
}

/// Presents an agent's screen action to the person (spec 3.3 §7.1): the policy's words as the
/// summary ("Move News digest to 19:00"), the risk its effect carries (a change on the
/// person's own screens is low) and Allow once · Edit · Deny. Every other call goes to the
/// fallback router unchanged.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::{ApprovalPolicy, ApproverSelector, InMemoryApprovalStore, StoreApprovalService};
/// use std::sync::Arc;
///
/// # fn example(registry: adk_surface::verbs::VerbRegistry) {
/// let operator = ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval();
/// let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
///     .with_router(registry.router(operator));
/// # let _ = service;
/// # }
/// ```
#[derive(Clone)]
pub struct VerbRouter {
    origin: String,
    /// The effect of each action tool, by tool name.
    effects: BTreeMap<String, EffectClass>,
    fallback: Arc<dyn ApprovalRouter>,
}

impl std::fmt::Debug for VerbRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerbRouter")
            .field("origin", &self.origin)
            .field("tools", &self.effects.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl VerbRouter {
    pub(crate) fn new(registry: VerbRegistry, fallback: Arc<dyn ApprovalRouter>) -> Self {
        let effects = registry
            .entries()
            .iter()
            .map(|entry| (entry.spec.tool_name(), entry.spec.effect))
            .collect();
        Self { origin: registry.origin(), effects, fallback }
    }

    /// The router of the first catalogue's tools in `app`, with `fallback` for every other
    /// call, for a host that builds its approval service before the registry, which reads
    /// that service's store (spec 3.3 §2 item 7).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{ApprovalPolicy, ApproverSelector};
    /// use adk_surface::verbs::VerbRouter;
    ///
    /// let operator = ApprovalPolicy::new([ApproverSelector::group("operator")]);
    /// let router = VerbRouter::catalogue("adk-rust", operator);
    /// # let _ = router;
    /// ```
    pub fn catalogue(app: &str, fallback: impl ApprovalRouter + 'static) -> Self {
        let effects = super::snooze::catalogue()
            .into_iter()
            .map(|entry| (entry.spec.tool_name(), entry.spec.effect))
            .collect();
        Self { origin: super::verbs_origin(app), effects, fallback: Arc::new(fallback) }
    }
}

/// The risk a card of an action of `effect` shows.
fn risk_of(effect: EffectClass) -> ApprovalRisk {
    match effect {
        EffectClass::Read | EffectClass::WriteLocal => ApprovalRisk::Low,
        EffectClass::PublishPublic | EffectClass::Financial | EffectClass::Delete => {
            ApprovalRisk::High
        }
        _ => ApprovalRisk::Medium,
    }
}

/// The policy's own words in `reason`, the governed path's "Tool 'verb_snooze' requires
/// confirmation: Move News digest to 19:00".
fn policy_words(reason: &str, tool: &str) -> String {
    let prefix = format!("Tool '{tool}' requires confirmation: ");
    reason.strip_prefix(&prefix).unwrap_or(reason).to_string()
}

impl ApprovalRouter for VerbRouter {
    fn route(&self, call: &ApprovalCall) -> Option<ApprovalRoute> {
        let route = self.fallback.route(call)?;
        if call.request.origin.as_ref().is_none_or(|by| by.source != self.origin) {
            return Some(route);
        }
        let Some(effect) = self.effects.get(&call.request.tool_name) else {
            return Some(route);
        };
        Some(
            route
                .with_summary(policy_words(&call.reason, &call.request.tool_name))
                .with_risk(risk_of(*effect))
                .with_allowed_actions([
                    ApprovalAction::Approve,
                    ApprovalAction::Edit,
                    ApprovalAction::Reject,
                ]),
        )
    }
}

/// What an agent's screen action looks like on a card (spec 3.3 §7.1, owner question 30): the
/// kernel's words as the heading, "Snooze something in Coming up" in chat, "You can change it
/// back", the fixed line "Agents always ask before they change things on your screens.", what
/// the agent read in the asking turn first, and never **Always allow**.
///
/// It reads only the inbox entry and the asking turn's tool events in the session store,
/// never text the agent wrote.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::{LocalSource, SurfaceEngine};
///
/// # fn example(registry: adk_surface::verbs::VerbRegistry, source: LocalSource) {
/// let engine = SurfaceEngine::builder(Arc::new(source))
///     .card_hints(Arc::new(registry.hints()))
///     .grant(registry.grant())
///     .build();
/// # let _ = engine;
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct VerbHints {
    registry: VerbRegistry,
}

impl VerbHints {
    pub(crate) fn new(registry: VerbRegistry) -> Self {
        Self { registry }
    }

    /// "Finance read 1 web page (example.com)": the pages the agent fetched in the turn that
    /// asked, from that turn's tool calls in the session store.
    async fn first_step(&self, request: &ApprovalRequest) -> Option<String> {
        let sessions = self.registry.inner().sessions.as_ref()?;
        let session = sessions
            .get(GetRequest {
                app_name: request.scope.app_name.clone(),
                user_id: request.scope.user_id.clone(),
                session_id: request.scope.session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await
            .ok()?;
        let events = session.events().all();
        let calls = |event: &adk_core::Event| -> Vec<(String, serde_json::Value)> {
            event
                .content()
                .map(|content| {
                    content
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::FunctionCall { name, args, .. } => {
                                Some((name.clone(), args.clone()))
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        // The newest turn that made this call asked.
        let (at, asking) = events.iter().enumerate().rev().find(|(_, event)| {
            calls(event)
                .iter()
                .any(|(name, args)| *name == request.tool_name && *args == request.args_redacted)
        })?;
        let invocation = asking.invocation_id.clone();
        let mut pages = 0usize;
        let mut hosts = BTreeSet::new();
        for event in events[..at].iter().filter(|event| event.invocation_id == invocation) {
            for (name, args) in calls(event) {
                if name != "web_fetch" {
                    continue;
                }
                pages += 1;
                if let Some(host) = args
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|url| url.split("://").nth(1))
                    .and_then(|rest| rest.split(['/', '?', '#']).next())
                    .filter(|host| !host.is_empty())
                {
                    hosts.insert(host.to_lowercase());
                }
            }
        }
        if pages == 0 {
            return None;
        }
        // The agent that made the call, as the session names it.
        let agent = Some(asking.author.clone())
            .filter(|author| !author.is_empty() && author != "user")
            .or_else(|| {
                request.acting.as_ref().and_then(|acting| {
                    acting.agent.id.as_str().rsplit('/').next().map(str::to_string)
                })
            })
            .unwrap_or_else(|| "The agent".to_string());
        let agent = self.registry.display_name(&agent);
        let what = if pages == 1 { "1 web page".to_string() } else { format!("{pages} web pages") };
        let hosts: Vec<String> = hosts.into_iter().collect();
        Some(match hosts.is_empty() {
            true => format!("{agent} read {what}"),
            false => format!("{agent} read {what} ({})", hosts.join(", ")),
        })
    }
}

/// Whether `request`'s summary is words a router wrote, not adk-core's default.
fn written(request: &ApprovalRequest) -> bool {
    let summary = request.summary.trim();
    !(summary.is_empty()
        || summary.starts_with(&format!("Tool '{}'", request.tool_name))
        || summary == format!("Approve a call to '{}'", request.tool_name))
}

#[async_trait]
impl CardHints for VerbHints {
    async fn hint(&self, request: &ApprovalRequest) -> Option<CardHint> {
        let origin = request.origin.as_ref()?;
        if origin.source != self.registry.origin() {
            return None;
        }
        let entry = self.registry.entry_for_tool(&request.tool_name)?;
        let mut hint = CardHint::new()
            .with_chat_heading(entry.spec.description.clone())
            .without_always_allow()
            .with_detail(CardDetail::new(words::AGENTS_ALWAYS_ASK));
        if written(request) {
            hint = hint.with_heading(request.summary.clone());
        }
        if entry.spec.reversible {
            hint = hint.with_reversibility(Reversibility::CanBeChangedBack);
        }
        if let Some(step) = self.first_step(request).await {
            hint = hint.with_first_step(step);
        }
        Some(hint)
    }
}
