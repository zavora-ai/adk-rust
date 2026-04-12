use adk_core::{
    AdkIdentity, Agent, AppName, Artifacts, CallbackContext, Content, Event, ExecutionIdentity,
    InvocationContext as InvocationContextTrait, InvocationId, Memory, ReadonlyContext,
    RequestContext, RunConfig, SessionId, UserId,
};
use adk_session::Session as AdkSession;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock, atomic::AtomicBool};

/// MutableSession wraps a session with shared mutable state.
///
/// This mirrors ADK-Go's MutableSession pattern where state changes from
/// events are immediately visible to all agents sharing the same context.
/// This is critical for SequentialAgent/LoopAgent patterns where downstream
/// agents need to read state set by upstream agents via output_key.
pub struct MutableSession {
    /// The original session snapshot (for metadata like id, app_name, user_id)
    inner: Arc<dyn AdkSession>,
    /// Shared mutable state - updated when events are processed
    /// This is the key difference from the old SessionAdapter which used immutable snapshots
    state: Arc<RwLock<HashMap<String, serde_json::Value>>>,
    /// Accumulated events during this invocation (uses adk_core::Event which is re-exported by adk_session)
    events: Arc<RwLock<Vec<Event>>>,
}

impl MutableSession {
    /// Create a new MutableSession from a session snapshot.
    /// The state is copied from the session and becomes mutable.
    pub fn new(session: Arc<dyn AdkSession>) -> Self {
        // Clone the initial state from the session
        let initial_state = session.state().all();
        // Clone the initial events
        let initial_events = session.events().all();

        Self {
            inner: session,
            state: Arc::new(RwLock::new(initial_state)),
            events: Arc::new(RwLock::new(initial_events)),
        }
    }

    /// Apply state delta from an event to the mutable state.
    /// This is called by the Runner when events are yielded.
    pub fn apply_state_delta(&self, delta: &HashMap<String, serde_json::Value>) {
        if delta.is_empty() {
            return;
        }

        let Ok(mut state) = self.state.write() else {
            tracing::error!("state RwLock poisoned in apply_state_delta — skipping delta");
            return;
        };
        for (key, value) in delta {
            // Skip temp: prefixed keys (they shouldn't persist)
            if !key.starts_with("temp:") {
                state.insert(key.clone(), value.clone());
            }
        }
    }

    /// Append an event to the session's event list.
    /// This keeps the in-memory view consistent.
    pub fn append_event(&self, event: Event) {
        let Ok(mut events) = self.events.write() else {
            tracing::error!("events RwLock poisoned in append_event — event dropped");
            return;
        };
        events.push(event);
    }

    /// Get a snapshot of all events in the session.
    /// Used by the runner for compaction decisions.
    pub fn events_snapshot(&self) -> Vec<Event> {
        let Ok(events) = self.events.read() else {
            tracing::error!("events RwLock poisoned in events_snapshot — returning empty");
            return Vec::new();
        };
        events.clone()
    }

    /// Return the number of accumulated events without cloning the full list.
    pub fn events_len(&self) -> usize {
        let Ok(events) = self.events.read() else {
            tracing::error!("events RwLock poisoned in events_len — returning 0");
            return 0;
        };
        events.len()
    }

    /// Build conversation history, optionally filtered for a specific agent.
    ///
    /// When `agent_name` is `Some`, events authored by other agents (not "user",
    /// not the named agent, and not function/tool responses) are excluded. This
    /// prevents a transferred sub-agent from seeing the parent's tool calls
    /// mapped as "model" role, which would cause the LLM to think work is
    /// already done.
    ///
    /// When `agent_name` is `None`, all events are included (backward-compatible).
    pub fn conversation_history_for_agent_impl(
        &self,
        agent_name: Option<&str>,
    ) -> Vec<adk_core::Content> {
        let Ok(events) = self.events.read() else {
            tracing::error!("events RwLock poisoned in conversation_history — returning empty");
            return Vec::new();
        };
        let mut history = Vec::new();

        // Find the most recent compaction event — everything before its
        // end_timestamp has been summarized and should be replaced by the
        // compacted content.
        let mut compaction_boundary = None;
        for event in events.iter().rev() {
            if let Some(ref compaction) = event.actions.compaction {
                history.push(compaction.compacted_content.clone());
                compaction_boundary = Some(compaction.end_timestamp);
                break;
            }
        }

        for event in events.iter() {
            // Skip the compaction event itself
            if event.actions.compaction.is_some() {
                continue;
            }

            // Skip events that were already compacted
            if let Some(boundary) = compaction_boundary {
                if event.timestamp <= boundary {
                    continue;
                }
            }

            // When filtering for a specific agent, skip events from other agents.
            // Keep: user messages and the agent's own events.
            // Skip: other agents' events entirely (model-role, function calls,
            // and function/tool responses). This prevents the sub-agent from
            // seeing orphaned function responses without their preceding calls.
            if let Some(name) = agent_name {
                if event.author != "user" && event.author != name {
                    continue;
                }
            }

            if let Some(content) = &event.llm_response.content {
                let mut mapped_content = content.clone();
                mapped_content.role = match (event.author.as_str(), content.role.as_str()) {
                    ("user", _) => "user",
                    (_, "function" | "tool") => content.role.as_str(),
                    _ => "model",
                }
                .to_string();
                history.push(mapped_content);
            }
        }

        history
    }
}

impl adk_core::Session for MutableSession {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    fn user_id(&self) -> &str {
        self.inner.user_id()
    }

    fn state(&self) -> &dyn adk_core::State {
        self
    }

    fn conversation_history(&self) -> Vec<adk_core::Content> {
        self.conversation_history_for_agent_impl(None)
    }

    fn conversation_history_for_agent(&self, agent_name: &str) -> Vec<adk_core::Content> {
        self.conversation_history_for_agent_impl(Some(agent_name))
    }
}

impl adk_core::State for MutableSession {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        let Ok(state) = self.state.read() else {
            tracing::error!("state RwLock poisoned in State::get — returning None");
            return None;
        };
        state.get(key).cloned()
    }

    fn set(&mut self, key: String, value: serde_json::Value) {
        if let Err(msg) = adk_core::validate_state_key(&key) {
            tracing::warn!(key = %key, "rejecting invalid state key: {msg}");
            return;
        }
        let Ok(mut state) = self.state.write() else {
            tracing::error!("state RwLock poisoned in State::set — value dropped");
            return;
        };
        state.insert(key, value);
    }

    fn all(&self) -> HashMap<String, serde_json::Value> {
        let Ok(state) = self.state.read() else {
            tracing::error!("state RwLock poisoned in State::all — returning empty");
            return HashMap::new();
        };
        state.clone()
    }
}

pub struct InvocationContext {
    identity: ExecutionIdentity,
    agent: Arc<dyn Agent>,
    user_content: Content,
    artifacts: Option<Arc<dyn Artifacts>>,
    memory: Option<Arc<dyn Memory>>,
    run_config: RunConfig,
    ended: Arc<AtomicBool>,
    /// Mutable session that allows state to be updated during execution.
    /// This is shared across all agents in a workflow, enabling state
    /// propagation between sequential/parallel agents.
    session: Arc<MutableSession>,
    /// Optional request context from the server's auth middleware bridge.
    /// When present, `user_id()` returns `request_context.user_id` and
    /// `user_scopes()` returns `request_context.scopes`.
    request_context: Option<RequestContext>,
    /// Optional shared state for parallel agent coordination.
    shared_state: Option<Arc<adk_core::SharedState>>,
}

impl InvocationContext {
    /// Create a new invocation context from validated typed identifiers.
    pub fn new_typed(
        invocation_id: String,
        agent: Arc<dyn Agent>,
        user_id: UserId,
        app_name: AppName,
        session_id: SessionId,
        user_content: Content,
        session: Arc<dyn AdkSession>,
    ) -> adk_core::Result<Self> {
        let identity = ExecutionIdentity {
            adk: AdkIdentity { app_name, user_id, session_id },
            invocation_id: InvocationId::try_from(invocation_id)?,
            branch: String::new(),
            agent_name: agent.name().to_string(),
        };
        Ok(Self {
            identity,
            agent,
            user_content,
            artifacts: None,
            memory: None,
            run_config: RunConfig::default(),
            ended: Arc::new(AtomicBool::new(false)),
            session: Arc::new(MutableSession::new(session)),
            request_context: None,
            shared_state: None,
        })
    }

    pub fn new(
        invocation_id: String,
        agent: Arc<dyn Agent>,
        user_id: String,
        app_name: String,
        session_id: String,
        user_content: Content,
        session: Arc<dyn AdkSession>,
    ) -> adk_core::Result<Self> {
        Self::new_typed(
            invocation_id,
            agent,
            UserId::try_from(user_id)?,
            AppName::try_from(app_name)?,
            SessionId::try_from(session_id)?,
            user_content,
            session,
        )
    }

    /// Create an invocation context that reuses an existing mutable session and
    /// validated typed identifiers.
    pub fn with_mutable_session_typed(
        invocation_id: String,
        agent: Arc<dyn Agent>,
        user_id: UserId,
        app_name: AppName,
        session_id: SessionId,
        user_content: Content,
        session: Arc<MutableSession>,
    ) -> adk_core::Result<Self> {
        let identity = ExecutionIdentity {
            adk: AdkIdentity { app_name, user_id, session_id },
            invocation_id: InvocationId::try_from(invocation_id)?,
            branch: String::new(),
            agent_name: agent.name().to_string(),
        };
        Ok(Self {
            identity,
            agent,
            user_content,
            artifacts: None,
            memory: None,
            run_config: RunConfig::default(),
            ended: Arc::new(AtomicBool::new(false)),
            session,
            request_context: None,
            shared_state: None,
        })
    }

    /// Create an InvocationContext with an existing MutableSession.
    /// This allows sharing the same mutable session across multiple contexts
    /// (e.g., for agent transfers).
    pub fn with_mutable_session(
        invocation_id: String,
        agent: Arc<dyn Agent>,
        user_id: String,
        app_name: String,
        session_id: String,
        user_content: Content,
        session: Arc<MutableSession>,
    ) -> adk_core::Result<Self> {
        Self::with_mutable_session_typed(
            invocation_id,
            agent,
            UserId::try_from(user_id)?,
            AppName::try_from(app_name)?,
            SessionId::try_from(session_id)?,
            user_content,
            session,
        )
    }

    pub fn with_branch(mut self, branch: String) -> Self {
        self.identity.branch = branch;
        self
    }

    pub fn with_artifacts(mut self, artifacts: Arc<dyn Artifacts>) -> Self {
        self.artifacts = Some(artifacts);
        self
    }

    pub fn with_memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_run_config(mut self, config: RunConfig) -> Self {
        self.run_config = config;
        self
    }

    /// Set the request context from the server's auth middleware bridge.
    ///
    /// When set, `user_id()` returns `request_context.user_id` (overriding
    /// the session-scoped identity), and `user_scopes()` returns
    /// `request_context.scopes`. This is the explicit authenticated user
    /// override — `RequestContext` remains separate from `ExecutionIdentity`
    /// and `AdkIdentity` (it does not carry session or invocation IDs).
    pub fn with_request_context(mut self, ctx: RequestContext) -> Self {
        self.request_context = Some(ctx);
        self
    }

    /// Set the shared state for parallel agent coordination.
    pub fn with_shared_state(mut self, shared: Arc<adk_core::SharedState>) -> Self {
        self.shared_state = Some(shared);
        self
    }

    /// Get a reference to the mutable session.
    /// This allows the Runner to apply state deltas when events are processed.
    pub fn mutable_session(&self) -> &Arc<MutableSession> {
        &self.session
    }
}

#[async_trait]
impl ReadonlyContext for InvocationContext {
    fn invocation_id(&self) -> &str {
        self.identity.invocation_id.as_ref()
    }

    fn agent_name(&self) -> &str {
        self.agent.name()
    }

    fn user_id(&self) -> &str {
        // Explicit authenticated user override: when a RequestContext is
        // present (set via with_request_context from the auth middleware
        // bridge), the authenticated user_id takes precedence over the
        // session-scoped identity. This keeps auth binding explicit and
        // ensures the runtime reflects the verified caller identity.
        self.request_context.as_ref().map_or(self.identity.adk.user_id.as_ref(), |rc| &rc.user_id)
    }

    fn app_name(&self) -> &str {
        self.identity.adk.app_name.as_ref()
    }

    fn session_id(&self) -> &str {
        self.identity.adk.session_id.as_ref()
    }

    fn branch(&self) -> &str {
        &self.identity.branch
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for InvocationContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.artifacts.clone()
    }

    fn shared_state(&self) -> Option<Arc<adk_core::SharedState>> {
        self.shared_state.clone()
    }
}

#[async_trait]
impl InvocationContextTrait for InvocationContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }

    fn memory(&self) -> Option<Arc<dyn Memory>> {
        self.memory.clone()
    }

    fn session(&self) -> &dyn adk_core::Session {
        self.session.as_ref()
    }

    fn run_config(&self) -> &RunConfig {
        &self.run_config
    }

    fn end_invocation(&self) {
        self.ended.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn user_scopes(&self) -> Vec<String> {
        self.request_context.as_ref().map_or_else(Vec::new, |rc| rc.scopes.clone())
    }

    fn request_metadata(&self) -> HashMap<String, serde_json::Value> {
        self.request_context.as_ref().map_or_else(HashMap::new, |rc| {
            rc.metadata
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect()
        })
    }
}
