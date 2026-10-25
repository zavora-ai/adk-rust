//! The public partition at the edge (spec 3.5 §7.10, A-I2, A-I3).
//!
//! The conversation endpoints — A2A messages without a capability, and `GET /awp/v2/chat` —
//! host an agent the caller supplies (the Concierge and Agent desk of `adk-website`). The edge
//! hands it a fixed set of tools, [`PublicTools`], and runs it under
//! [`PublicPartitionPolicy`](super::policy::PublicPartitionPolicy), which denies every other
//! tool by name and origin:
//!
//! | Tool | Effect | Does |
//! |---|---|---|
//! | Each declared capability | as declared | Calls the capability through the edge's governed path with the conversation's own tier |
//! | `search_site` | read-only | Searches the current release's content through a [`SiteSearch`] |
//! | `reply_in_thread` | external send | Answers the person who wrote, in the same thread, inside a template, rate-limited, with no new recipients and no attachments |
//! | `emit_signal` | append | Files one typed [`Signal`] with the back office through a [`SignalEmitter`] |
//!
//! No tool fetches the web, publishes, deploys, changes DNS, spends, sends in bulk or reads the
//! owner's memory.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_awp::v2::capability::{CapabilityTool, META_CONSENT, META_REQUESTER, META_TIER};
use adk_awp::v2::executor::{Binding, Invocation, InvocationOutcome};
use adk_core::{
    AdkError, Agent, Content, ErrorCategory, ErrorComponent, RequestContext, Result, RunConfig,
    SessionId, Tool, ToolContext, ToolEffect, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, SessionService};
use async_trait::async_trait;
use awp_types::{RequesterId, Signal, SignalKind, ThreadRef, TrustTier};
use futures::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::executor::{GovernedExecutor, KeySources};
use super::policy::PublicPartitionPolicy;
use super::tiers::EdgeCaller;

/// The site search tool's name.
pub const SEARCH_SITE: &str = "search_site";
/// The in-thread reply tool's name.
pub const REPLY_IN_THREAD: &str = "reply_in_thread";
/// The signal tool's name.
pub const EMIT_SIGNAL: &str = "emit_signal";

/// Request-metadata key holding the conversation's thread.
pub const META_THREAD: &str = "awp.thread";
/// Request-metadata key holding the binding the conversation arrived over.
pub const META_BINDING: &str = "awp.binding";
/// Request-metadata key holding the caller's limit key.
pub const META_LIMIT_KEY: &str = "awp.limit_key";

/// The longest reply text `reply_in_thread` sends, before the template.
pub const MAX_REPLY_CHARS: usize = 2_000;

/// The tool origin of the edge's own public tools for the site at `domain`.
pub fn public_tool_origin(domain: &str) -> String {
    format!("awp-edge:{domain}")
}

fn tool_error(category: ErrorCategory, code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Tool, category, code, message)
}

fn metadata(ctx: &dyn ToolContext, key: &str) -> Option<String> {
    ctx.request_metadata().get(key).and_then(Value::as_str).map(str::to_owned)
}

// --- search_site ---------------------------------------------------------------------------

/// One page a site search found (spec 3.5 §7.10).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteHit {
    /// The page's path.
    pub path: String,
    /// The page's title.
    pub title: String,
    /// A short excerpt.
    pub snippet: String,
}

/// Searches the current release's content (spec 3.5 §7.10).
///
/// `adk-website` implements it over the release's content index; [`StaticSiteIndex`] is a
/// built-in index of pages held in memory.
#[async_trait]
pub trait SiteSearch: Send + Sync {
    /// Returns at most `limit` pages matching `query`, best first.
    ///
    /// # Errors
    ///
    /// Returns an error when the index cannot be read.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SiteHit>>;
}

/// An in-memory [`SiteSearch`] over pages, ranked by how many query terms each contains.
///
/// # Example
///
/// ```rust
/// use adk_server::awp::public::{SiteSearch, StaticSiteIndex};
///
/// # futures::executor::block_on(async {
/// let index = StaticSiteIndex::new()
///     .page("/hours", "Opening hours", "We are open Monday to Friday, 8:00 to 17:00.")
///     .page("/solar", "Solar panels", "Panels from 400 W, installed in a day.");
/// let hits = index.search("when are you open", 3).await.unwrap();
/// assert_eq!(hits[0].path, "/hours");
/// # });
/// ```
#[derive(Debug, Clone, Default)]
pub struct StaticSiteIndex {
    pages: Vec<(String, String, String)>,
}

impl StaticSiteIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a page.
    #[must_use]
    pub fn page(
        mut self,
        path: impl Into<String>,
        title: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        self.pages.push((path.into(), title.into(), text.into()));
        self
    }
}

#[async_trait]
impl SiteSearch for StaticSiteIndex {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SiteHit>> {
        let terms: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|term| term.chars().count() > 2)
            .map(str::to_lowercase)
            .collect();
        let mut scored: Vec<(usize, &(String, String, String))> = self
            .pages
            .iter()
            .map(|page| {
                let haystack = format!("{} {}", page.1, page.2).to_lowercase();
                (terms.iter().filter(|term| haystack.contains(term.as_str())).count(), page)
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(_, (path, title, text))| SiteHit {
                path: path.clone(),
                title: title.clone(),
                snippet: text.chars().take(280).collect(),
            })
            .collect())
    }
}

struct SearchSiteTool {
    search: Arc<dyn SiteSearch>,
    origin: String,
}

#[async_trait]
impl Tool for SearchSiteTool {
    fn name(&self) -> &str {
        SEARCH_SITE
    }

    fn description(&self) -> &str {
        "Searches the pages of this website's current release and returns matching pages with a short excerpt."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "maxLength": 200 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 10 }
            },
            "required": ["query"]
        }))
    }

    fn origin(&self) -> &str {
        &self.origin
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let query: String = args["query"].as_str().unwrap_or_default().chars().take(200).collect();
        let limit = args["limit"].as_u64().map_or(5, |limit| limit.clamp(1, 10)) as usize;
        let hits = self.search.search(&query, limit).await?;
        Ok(json!({ "hits": hits }))
    }
}

// --- reply_in_thread -----------------------------------------------------------------------

/// Delivers an in-thread reply to the person who wrote (spec 3.5 §7.10).
///
/// `adk-website` implements it over its inbox drivers (email, social); the edge delivers
/// replies to its own open chat sockets itself.
#[async_trait]
pub trait ThreadSink: Send + Sync {
    /// Sends `text` to the thread `thread`; never to anyone else.
    ///
    /// # Errors
    ///
    /// Returns an error when the thread is unknown or delivery fails.
    async fn reply(&self, thread: &ThreadRef, text: &str) -> Result<()>;
}

/// A [`ThreadSink`] that records replies, for tests and for deployments with no inbox.
#[derive(Debug, Clone, Default)]
pub struct RecordingThreadSink {
    replies: Arc<Mutex<Vec<(ThreadRef, String)>>>,
}

impl RecordingThreadSink {
    /// An empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// The replies sent, oldest first.
    pub fn replies(&self) -> Vec<(ThreadRef, String)> {
        self.replies.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

#[async_trait]
impl ThreadSink for RecordingThreadSink {
    async fn reply(&self, thread: &ThreadRef, text: &str) -> Result<()> {
        self.replies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((thread.clone(), text.to_owned()));
        Ok(())
    }
}

/// Open chat sockets by thread, so `reply_in_thread` reaches a visitor still connected.
#[derive(Debug, Clone, Default)]
pub(crate) struct LiveThreads {
    open: Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>,
}

impl LiveThreads {
    pub(crate) fn open(&self, thread: &str, sender: mpsc::Sender<String>) {
        self.open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(thread.to_owned(), sender);
    }

    pub(crate) fn close(&self, thread: &str) {
        self.open.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(thread);
    }

    fn sender(&self, thread: &str) -> Option<mpsc::Sender<String>> {
        self.open.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(thread).cloned()
    }
}

/// How `reply_in_thread` frames and paces replies (spec 3.5 §7.10).
#[derive(Debug, Clone)]
pub struct ReplyPolicy {
    /// The playbook template; `{reply}` is replaced by the agent's text.
    pub template: String,
    /// Replies allowed per thread in [`window`](Self::window).
    pub max_per_window: usize,
    /// The rate window.
    pub window: Duration,
}

impl Default for ReplyPolicy {
    /// The reply alone; five replies per thread per ten minutes.
    fn default() -> Self {
        Self { template: "{reply}".to_owned(), max_per_window: 5, window: Duration::from_secs(600) }
    }
}

struct ReplyInThreadTool {
    sink: Arc<dyn ThreadSink>,
    live: LiveThreads,
    policy: ReplyPolicy,
    sent: Mutex<HashMap<String, VecDeque<Instant>>>,
    origin: String,
}

#[async_trait]
impl Tool for ReplyInThreadTool {
    fn name(&self) -> &str {
        REPLY_IN_THREAD
    }

    fn description(&self) -> &str {
        "Sends a short text reply to the person in this conversation's thread. It cannot add recipients or attachments."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "text": { "type": "string", "maxLength": MAX_REPLY_CHARS } },
            "required": ["text"],
            "additionalProperties": false
        }))
    }

    fn origin(&self) -> &str {
        &self.origin
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        // The thread comes from the conversation, never from the model, so a reply cannot
        // reach anyone new.
        let Some(thread) = metadata(ctx.as_ref(), META_THREAD) else {
            return Err(tool_error(
                ErrorCategory::InvalidInput,
                "awp.reply.no_thread",
                "this conversation has no thread to reply in",
            ));
        };
        let text: String =
            args["text"].as_str().unwrap_or_default().chars().take(MAX_REPLY_CHARS).collect();
        if text.trim().is_empty() {
            return Err(tool_error(
                ErrorCategory::InvalidInput,
                "awp.reply.empty",
                "the reply is empty",
            ));
        }
        {
            let mut sent = self.sent.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let times = sent.entry(thread.clone()).or_default();
            let now = Instant::now();
            while times.front().is_some_and(|at| now.duration_since(*at) > self.policy.window) {
                times.pop_front();
            }
            if times.len() >= self.policy.max_per_window {
                return Err(tool_error(
                    ErrorCategory::RateLimited,
                    "awp.reply.rate_limited",
                    "this thread has had its replies for now",
                ));
            }
            times.push_back(now);
        }
        let rendered = self.policy.template.replace("{reply}", &text);
        match self.live.sender(&thread) {
            Some(sender) => sender.send(rendered).await.map_err(|_| {
                tool_error(ErrorCategory::Unavailable, "awp.reply.closed", "the visitor left")
            })?,
            None => self.sink.reply(&ThreadRef(thread.clone()), &rendered).await?,
        }
        Ok(json!({ "status": "sent" }))
    }
}

// --- emit_signal ---------------------------------------------------------------------------

/// Files a typed signal with the back office (spec 3.5 §7.10, A-I3).
///
/// `adk-website` forwards to the kernel's `SignalSink`; [`RecordingSignals`] keeps them in
/// memory.
#[async_trait]
pub trait SignalEmitter: Send + Sync {
    /// Files `signal`.
    ///
    /// # Errors
    ///
    /// Returns an error when the back office does not accept it.
    async fn emit(&self, signal: Signal) -> Result<()>;
}

/// A [`SignalEmitter`] that keeps signals in memory.
#[derive(Debug, Clone, Default)]
pub struct RecordingSignals {
    signals: Arc<Mutex<Vec<Signal>>>,
}

impl RecordingSignals {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// The signals filed, oldest first.
    pub fn signals(&self) -> Vec<Signal> {
        self.signals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

#[async_trait]
impl SignalEmitter for RecordingSignals {
    async fn emit(&self, signal: Signal) -> Result<()> {
        self.signals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(signal);
        Ok(())
    }
}

struct EmitSignalTool {
    emitter: Arc<dyn SignalEmitter>,
    emitted_by: String,
    origin: String,
}

#[async_trait]
impl Tool for EmitSignalTool {
    fn name(&self) -> &str {
        EMIT_SIGNAL
    }

    fn description(&self) -> &str {
        "Tells the site's back office about something that happened in this conversation, as one typed record: a lead, a content gap, visitor intent, a support escalation, a review, an inbound message, or abuse."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "object",
                    "description": "The signal, tagged by `type`: content_gap {topic, questionCount, samples}, visitor_intent {capability, outcome}, support_escalation {reason, contactRef}, lead_captured {leadRef, source, consent, answers}, review_seen {platform, rating, excerpt}, inbound_message {channel, threadRef, category}, agent_interaction {capability, outcome}, abuse_observed {kind}. Free text goes in {\"text\": ...} objects.",
                    "properties": { "type": { "type": "string" } },
                    "required": ["type"]
                }
            },
            "required": ["kind"]
        }))
    }

    fn origin(&self) -> &str {
        &self.origin
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let mut kind_value = args.get("kind").cloned().unwrap_or(Value::Null);
        let requester = metadata(ctx.as_ref(), META_REQUESTER);
        // A signal naming a requester names the verified caller, never one the model chose.
        if let Some(object) = kind_value.as_object_mut()
            && object.contains_key("type")
            && matches!(
                object.get("type").and_then(Value::as_str),
                Some("agent_interaction" | "abuse_observed")
            )
        {
            match &requester {
                Some(requester) => {
                    object.insert("requester".to_owned(), Value::String(requester.clone()));
                }
                None => {
                    return Err(tool_error(
                        ErrorCategory::InvalidInput,
                        "awp.signal.no_requester",
                        "this conversation has no verified requester to name",
                    ));
                }
            }
        }
        let kind: SignalKind = serde_json::from_value(kind_value).map_err(|error| {
            tool_error(
                ErrorCategory::InvalidInput,
                "awp.signal.invalid",
                format!("the signal does not match a signal kind: {error}"),
            )
        })?;
        let signal = Signal {
            id: Uuid::new_v4(),
            kind,
            emitted_by: self.emitted_by.clone(),
            emitted_at: chrono::Utc::now(),
        };
        let name = signal.kind.name();
        self.emitter.emit(signal).await?;
        tracing::info!(signal.kind = name, "awp public partition emitted a signal");
        Ok(json!({ "status": "filed", "kind": name }))
    }
}

// --- capabilities ----------------------------------------------------------------------------

/// A declared capability as a public agent's tool: it calls the edge's governed path with the
/// conversation's own tier and requester, so a visitor's agent reaches only what the visitor
/// may call.
struct CapabilityProxy {
    executor: GovernedExecutor,
    capability: Arc<CapabilityTool>,
}

#[async_trait]
impl Tool for CapabilityProxy {
    fn name(&self) -> &str {
        self.capability.name()
    }

    fn description(&self) -> &str {
        self.capability.description()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.capability.parameters_schema()
    }

    fn annotations(&self) -> Option<&Value> {
        self.capability.annotations()
    }

    fn origin(&self) -> &str {
        self.capability.origin()
    }

    fn effect(&self) -> ToolEffect {
        self.capability.effect()
    }

    fn is_read_only(&self) -> bool {
        self.capability.is_read_only()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let meta = |key| metadata(ctx.as_ref(), key);
        let tier = meta(META_TIER)
            .and_then(|tier| serde_json::from_value(Value::String(tier)).ok())
            .unwrap_or(TrustTier::Public);
        let requester = meta(META_REQUESTER).map(RequesterId::new);
        let binding = match meta(META_BINDING).as_deref() {
            Some("a2a") => Binding::A2a,
            _ => Binding::Http,
        };
        let caller = EdgeCaller {
            tier,
            requester: requester.clone(),
            limit_key: meta(META_LIMIT_KEY).unwrap_or_else(|| "ip:unknown".to_owned()),
            rate_class: None,
            claimed_agent: None,
        };
        let keys = KeySources {
            explicit: Some(ctx.idempotency_key()),
            binding_id: Some(ctx.idempotency_key()),
        };
        let invocation = Invocation {
            capability: self.capability.declaration().name.clone(),
            input: args,
            tier,
            requester,
            idempotency_key: keys.key_for(self.capability.declaration().idempotency, binding),
            consent: meta(META_CONSENT)
                .map(|ids| ids.split(',').filter_map(|id| id.trim().parse().ok()).collect())
                .unwrap_or_default(),
            binding,
        };
        match self.executor.invoke_as(&caller, invocation).await {
            InvocationOutcome::Completed(output) => Ok(output),
            InvocationOutcome::Held(call) => Ok(json!({
                "status": "held",
                "note": "the site's owner decides this request; tell the person it is waiting for approval",
                "call": call,
            })),
            InvocationOutcome::Failed(error) => Err(tool_error(
                ErrorCategory::InvalidInput,
                "awp.capability.refused",
                format!("{}: {}", error.code, error.message),
            )),
            _ => {
                Err(tool_error(ErrorCategory::Internal, "awp.capability.failed", "the call failed"))
            }
        }
    }
}

/// The tools the edge gives a public agent (spec 3.5 §7.10, A-I2).
#[derive(Clone)]
#[non_exhaustive]
pub struct PublicTools {
    /// Each declared capability, called through the edge's governed path.
    pub capabilities: Vec<Arc<dyn Tool>>,
    /// `search_site`.
    pub search_site: Arc<dyn Tool>,
    /// `reply_in_thread`.
    pub reply_in_thread: Arc<dyn Tool>,
    /// `emit_signal`.
    pub emit_signal: Arc<dyn Tool>,
}

impl std::fmt::Debug for PublicTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.all().iter().map(|tool| tool.name().to_owned())).finish()
    }
}

impl PublicTools {
    /// Every tool, capabilities first.
    pub fn all(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.capabilities.clone();
        tools.extend([
            Arc::clone(&self.search_site),
            Arc::clone(&self.reply_in_thread),
            Arc::clone(&self.emit_signal),
        ]);
        tools
    }

    /// The policy allowing exactly these tools, by name and origin.
    pub fn policy(&self) -> PublicPartitionPolicy {
        PublicPartitionPolicy::new(
            self.all().iter().map(|tool| (tool.name().to_owned(), tool.origin().to_owned())),
        )
    }
}

/// What the public partition is built from.
pub(crate) struct PublicParts {
    pub(crate) search: Arc<dyn SiteSearch>,
    pub(crate) threads: Arc<dyn ThreadSink>,
    pub(crate) signals: Arc<dyn SignalEmitter>,
    pub(crate) reply_policy: ReplyPolicy,
    pub(crate) emitted_by: String,
}

pub(crate) fn public_tools(
    executor: &GovernedExecutor,
    parts: PublicParts,
    live: &LiveThreads,
) -> PublicTools {
    let domain = executor.registry().domain().to_owned();
    let origin = public_tool_origin(&domain);
    let capabilities = executor
        .registry()
        .tools()
        .into_iter()
        .filter_map(|tool| {
            let name = tool.name().to_owned();
            executor.registry().get(&name).cloned()
        })
        .map(|capability| {
            Arc::new(CapabilityProxy { executor: executor.clone(), capability }) as Arc<dyn Tool>
        })
        .collect();
    PublicTools {
        capabilities,
        search_site: Arc::new(SearchSiteTool { search: parts.search, origin: origin.clone() }),
        reply_in_thread: Arc::new(ReplyInThreadTool {
            sink: parts.threads,
            live: live.clone(),
            policy: parts.reply_policy,
            sent: Mutex::new(HashMap::new()),
            origin: origin.clone(),
        }),
        emit_signal: Arc::new(EmitSignalTool {
            emitter: parts.signals,
            emitted_by: parts.emitted_by,
            origin,
        }),
    }
}

// --- the hosted agent ----------------------------------------------------------------------

/// The public agent the conversation endpoints run, under the public partition's policy.
pub(crate) struct PublicPartition {
    runner: Runner,
    sessions: Arc<dyn SessionService>,
    app_name: String,
}

impl PublicPartition {
    pub(crate) fn new(
        agent: Arc<dyn Agent>,
        tools: &PublicTools,
        sessions: Arc<dyn SessionService>,
        app_name: &str,
        audit: Option<&adk_core::audit_log::AuditTrail>,
    ) -> Result<Self> {
        let mut builder = Runner::builder()
            .app_name(app_name)
            .agent(agent)
            .session_service(Arc::clone(&sessions))
            .tool_policy(Arc::new(tools.policy()));
        // Refusals of tools outside the public set are audited (spec 3.5 S7).
        if let Some(audit) = audit {
            builder = builder.audit_log(Arc::clone(audit.recorder()));
        }
        let runner = builder.build()?;
        Ok(Self { runner, sessions, app_name: app_name.to_owned() })
    }

    /// The session user a caller converses as: its requester id, else a digest of its limit key.
    fn user_for(caller: &EdgeCaller) -> String {
        caller.requester.as_ref().map_or_else(
            || format!("public-{}", hex::encode(&Sha256::digest(caller.limit_key.as_bytes())[..8])),
            |requester| requester.as_str().to_owned(),
        )
    }

    /// Runs one turn of the conversation `session` for `caller` and returns the agent's reply;
    /// `reply_in_thread` answers `thread`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn converse(
        &self,
        caller: &EdgeCaller,
        binding: &str,
        session: &str,
        thread: &str,
        text: &str,
        consent: &[Uuid],
        governance: Option<adk_core::GovernanceControl>,
    ) -> Result<String> {
        let user = Self::user_for(caller);
        let existing = self
            .sessions
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: user.clone(),
                session_id: session.to_owned(),
                num_recent_events: None,
                after: None,
            })
            .await;
        if let Err(error) = existing {
            if !error.is_not_found() {
                return Err(error);
            }
            let created = self
                .sessions
                .create(CreateRequest {
                    app_name: self.app_name.clone(),
                    user_id: user.clone(),
                    session_id: Some(session.to_owned()),
                    state: HashMap::new(),
                })
                .await;
            if let Err(error) = created
                && !error.is_conflict()
            {
                return Err(error);
            }
        }
        let mut metadata = HashMap::from([
            (META_TIER.to_owned(), caller.tier.as_str().to_owned()),
            (META_THREAD.to_owned(), thread.to_owned()),
            (META_BINDING.to_owned(), binding.to_owned()),
            (META_LIMIT_KEY.to_owned(), caller.limit_key.clone()),
            (
                META_CONSENT.to_owned(),
                consent.iter().map(ToString::to_string).collect::<Vec<_>>().join(","),
            ),
        ]);
        if let Some(requester) = &caller.requester {
            metadata.insert(META_REQUESTER.to_owned(), requester.as_str().to_owned());
        }
        let run_config = RunConfig {
            request_context: Some(RequestContext {
                user_id: user.clone(),
                scopes: Vec::new(),
                metadata,
            }),
            governance: governance.into_iter().collect(),
            ..RunConfig::default()
        };
        let mut events = self
            .runner
            .run_with_config(
                UserId::try_from(user)?,
                SessionId::try_from(session.to_owned())?,
                Content::new("user").with_text(text),
                Some(run_config),
            )
            .await?;
        let mut reply = String::new();
        while let Some(event) = events.next().await {
            let event = event?;
            if event.author == "user" || event.llm_response.partial {
                continue;
            }
            if let Some(content) = &event.llm_response.content {
                let text: String = content
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        adk_core::Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if !text.is_empty() {
                    reply = text;
                }
            }
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_static_index_ranks_by_matching_terms() {
        let index = StaticSiteIndex::new()
            .page("/a", "Parking", "Free parking behind the shop.")
            .page("/b", "Delivery", "We deliver panels and batteries, parking permits included.");
        let hits = index.search("parking batteries", 5).await.unwrap();
        assert_eq!(hits.iter().map(|hit| hit.path.as_str()).collect::<Vec<_>>(), ["/b", "/a"]);
        assert!(index.search("zz", 5).await.unwrap().is_empty());
    }
}
