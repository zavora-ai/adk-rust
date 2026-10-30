//! How an agent's screen actions are governed and shown (spec 3.3 §7.1, §7.2, owner question
//! 22): held for a person every time, at most three waiting, on a card that offers Allow once ·
//! Edit · Deny and never **Always allow**.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use adk_core::{
    AgentId, ApprovalAction, ApprovalCall, ApprovalRequest, ApprovalRequestQuery, ApprovalRisk,
    ApprovalRoute, ApprovalRouter, ApprovalScope, ApprovalState, ApprovalStore, Event, Part,
    PolicyCall, PolicyDecision, ToolConfirmationRequest, ToolPolicy, ToolPolicyRequest,
};
use adk_kernel::PartitionId;
use adk_kernel::drivers::EffectClass;
use adk_session::GetRequest;
use async_trait::async_trait;

use super::registry::VerbRegistry;
use super::{Callers, MAX_WAITING, verbs_principal, words};
use crate::hints::{CardHint, CardHints};
use crate::model::{CardDetail, Reversibility};
use crate::source::Source;

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
/// | From an agent with [`MAX_WAITING`] requests waiting for the same person already, unless the person answered this call | Deny: "Finance already has 3 changes waiting for you" |
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

    /// How many of `agent`'s screen actions wait for `user` to answer, up to
    /// [`MAX_WAITING`]: requests still pending, on every page of the store.
    async fn waiting(&self, agent: &AgentId, user: &str) -> usize {
        let registry = self.registry.inner();
        let Some(store) = &registry.approvals else {
            return 0;
        };
        let query = ApprovalRequestQuery::default()
            .with_states([ApprovalState::Pending])
            .with_app_name(&registry.app)
            .with_user_id(user);
        let origin = self.registry.origin();
        let mut count = 0;
        let found = every_request(store.as_ref(), query, |request| {
            if request.origin.as_ref().is_some_and(|by| by.source == origin)
                && request.acting.as_ref().is_some_and(|acting| acting.agent.id == *agent)
            {
                count += 1;
            }
            count >= MAX_WAITING
        })
        .await;
        if let Err(error) = found {
            tracing::warn!(error = %error, "the waiting screen actions could not be counted");
        }
        count
    }

    /// Whether the person already answered this call, so it asks for nothing new: a held
    /// call resuming with its decision waiting, or the arguments a person's Edit gave it.
    async fn answered(&self, request: &ToolPolicyRequest, call: &PolicyCall<'_>) -> bool {
        let Some(store) = &self.registry.inner().approvals else {
            return false;
        };
        let Some(acting) = &request.acting else {
            return false;
        };
        let scope = ApprovalScope::new(&request.app_name, &request.user_id, &request.session_id);
        let mut held = ToolConfirmationRequest::new(&request.tool_name, request.args.clone())
            .with_acting_context(acting.clone());
        if let Some(origin) = call.origin {
            held = held.with_origin(origin.clone());
        }
        if store.has_decision(&scope, &held.fingerprint()).await.unwrap_or(false) {
            return true;
        }
        let query = ApprovalRequestQuery::default()
            .with_session(&scope)
            .with_states([ApprovalState::Approved]);
        let mut edited = false;
        let _ = every_request(store.as_ref(), query, |approved| {
            edited = approved.tool_name == request.tool_name
                && approved.acting.as_ref().is_some_and(|by| by.agent.id == acting.agent.id)
                && approved
                    .decisions
                    .last()
                    .and_then(|decision| decision.edited_args.as_ref())
                    .is_some_and(|args| *args == request.args);
            edited
        })
        .await;
        edited
    }
}

/// Reads every request `query` selects, a page at a time, until `seen` returns true.
pub(crate) async fn every_request(
    store: &dyn ApprovalStore,
    mut query: ApprovalRequestQuery,
    mut seen: impl FnMut(&ApprovalRequest) -> bool,
) -> adk_core::Result<()> {
    loop {
        let page = store.list_requests(&query).await?;
        let full = page.len() >= query.limit;
        for request in &page {
            if seen(request) {
                return Ok(());
            }
        }
        match page.last() {
            Some(last) if full => query = query.with_after(Some(last.cursor())),
            _ => return Ok(()),
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
            && self.waiting(agent, &request.user_id).await >= MAX_WAITING
            && !self.answered(request, call).await
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
/// It reads only the inbox entry and, for the first step, the asking turn's tool results in
/// the session store: the pages its `web_fetch` calls fetched, by host. It never reads text
/// the agent wrote, and a call that was refused or failed is not a page read. Each request's
/// turn is read once.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::{LocalSource, Source, SurfaceEngine};
///
/// # fn example(registry: adk_surface::verbs::VerbRegistry, source: LocalSource) {
/// let source: Arc<dyn Source> = Arc::new(source);
/// let engine = SurfaceEngine::builder(Arc::clone(&source))
///     .card_hints(Arc::new(registry.hints().reading(source)))
///     .grant(registry.grant())
///     .build();
/// # let _ = engine;
/// # }
/// ```
#[derive(Clone)]
pub struct VerbHints {
    registry: VerbRegistry,
    source: Option<Arc<dyn Source>>,
    /// What the asking turn of each request read, by request: that turn never changes.
    reads: Arc<Mutex<HashMap<String, Reads>>>,
}

impl std::fmt::Debug for VerbHints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerbHints")
            .field("registry", &self.registry)
            .field("source", &self.source.is_some())
            .finish_non_exhaustive()
    }
}

/// What an agent read in the turn that asked.
#[derive(Debug, Clone)]
struct Reads {
    /// The agent, as the session names it.
    agent: String,
    /// Pages fetched.
    pages: usize,
    /// Their hosts.
    hosts: BTreeSet<String>,
}

/// The most requests whose asking turn is kept.
const KEPT_TURNS: usize = 512;

/// The longest host name (RFC 1035).
const MAX_HOST: usize = 253;

impl VerbHints {
    pub(crate) fn new(registry: VerbRegistry) -> Self {
        Self { registry, source: None, reads: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// Reads the asking turn and the agents' names through `source`, the engine's own (spec
    /// 3.3 §5.7, §7.1): an agent the person added or renamed is named as the card names it,
    /// and a process without the session store, such as a terminal attached to a kernel, still
    /// shows what the agent read.
    #[must_use]
    pub fn reading(mut self, source: Arc<dyn Source>) -> Self {
        self.source = Some(source);
        self
    }

    /// The events of `request`'s session, from the registry's session store, else the source.
    async fn events(&self, request: &ApprovalRequest) -> Option<Vec<Event>> {
        let scope = &request.scope;
        if let Some(sessions) = &self.registry.inner().sessions {
            let session = sessions
                .get(GetRequest {
                    app_name: scope.app_name.clone(),
                    user_id: scope.user_id.clone(),
                    session_id: scope.session_id.clone(),
                    num_recent_events: None,
                    after: None,
                })
                .await
                .ok()?;
            return Some(session.events().all());
        }
        let source = self.source.as_ref()?;
        source.transcript(&scope.user_id, &scope.session_id).await.ok().flatten()
    }

    /// What the turn that made `request`'s call read, once that turn is in the session.
    async fn reads(&self, request: &ApprovalRequest) -> Option<Reads> {
        if let Some(reads) = self.reads.lock().unwrap_or_else(|e| e.into_inner()).get(&request.id) {
            return Some(reads.clone());
        }
        let events = self.events(request).await?;
        // The newest turn that made this call asked.
        let (at, asking) = events.iter().enumerate().rev().find(|(_, event)| {
            event.content().is_some_and(|content| {
                content.parts.iter().any(|part| {
                    matches!(part, Part::FunctionCall { name, args, .. }
                        if *name == request.tool_name && *args == request.args_redacted)
                })
            })
        })?;
        let mut reads = Reads {
            // The agent that made the call, as the session names it.
            agent: Some(asking.author.clone())
                .filter(|author| !author.is_empty() && author != "user")
                .or_else(|| {
                    request.acting.as_ref().and_then(|acting| {
                        acting.agent.id.as_str().rsplit('/').next().map(str::to_string)
                    })
                })
                .unwrap_or_else(|| "The agent".to_string()),
            pages: 0,
            hosts: BTreeSet::new(),
        };
        let turn = events[..at].iter().filter(|event| event.invocation_id == asking.invocation_id);
        for part in turn.filter_map(Event::content).flat_map(|content| content.parts.iter()) {
            if let Part::FunctionResponse { function_response, .. } = part
                && function_response.name == "web_fetch"
                && let Some(host) = fetched(&function_response.response)
            {
                reads.pages += 1;
                reads.hosts.extend(host);
            }
        }
        let mut kept = self.reads.lock().unwrap_or_else(|e| e.into_inner());
        if kept.len() >= KEPT_TURNS {
            kept.clear();
        }
        kept.insert(request.id.clone(), reads.clone());
        Some(reads)
    }

    /// "Finance read 1 web page (example.com)": the pages the agent fetched in the turn that
    /// asked, named as the card names the agent.
    async fn first_step(&self, request: &ApprovalRequest) -> Option<String> {
        let reads = self.reads(request).await?;
        if reads.pages == 0 {
            return None;
        }
        let added = match &self.source {
            Some(source) => source.agent(&request.scope.user_id, &reads.agent).await.ok().flatten(),
            None => None,
        };
        let agent = match added {
            Some(detail) => detail.agent.display_name,
            None => self.registry.display_name(&reads.agent),
        };
        let what = match reads.pages {
            1 => "1 web page".to_string(),
            pages => format!("{pages} web pages"),
        };
        let hosts: Vec<String> = reads.hosts.into_iter().collect();
        Some(match hosts.is_empty() {
            true => format!("{agent} read {what}"),
            false => format!("{agent} read {what} ({})", hosts.join(", ")),
        })
    }
}

/// For a `web_fetch` result that fetched a page, the host it was fetched from, when that is a
/// host name; `None` for a call that was refused or failed.
fn fetched(response: &serde_json::Value) -> Option<Option<String>> {
    if response.get("error").is_some() || response.get("status").is_none() {
        return None;
    }
    let url = response.get("final_url").or_else(|| response.get("url"))?.as_str()?;
    let host = reqwest::Url::parse(url)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .filter(|host| !host.is_empty() && host.len() <= MAX_HOST);
    Some(host)
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

    /// The item's title, from the kernel's agenda; without it — a terminal attached to the
    /// kernel, or an item that has ended — a snooze's card summary, which the kernel wrote,
    /// still names it (spec 3.3 §8).
    async fn names(&self, request: &ApprovalRequest) -> Option<String> {
        let origin = request.origin.as_ref()?;
        if origin.source != self.registry.origin() {
            return None;
        }
        let entry = self.registry.entry_for_tool(&request.tool_name)?;
        let id = request.args_redacted.get("item").and_then(serde_json::Value::as_str)?;
        match self.registry.scope(&request.scope.user_id, None).item(id).await {
            Ok(item) => Some(item.title),
            Err(_) if entry.spec.name == "snooze" => {
                words::title_of_move(&request.summary).map(str::to_string)
            }
            Err(_) => None,
        }
    }
}
