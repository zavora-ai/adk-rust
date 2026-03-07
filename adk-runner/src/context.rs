use adk_core::{
    Agent, Artifacts, CallbackContext, Content, Event, InvocationContext as InvocationContextTrait,
    Memory, ReadonlyContext, RunConfig,
    types::{AdkIdentity, InvocationId, SessionId, UserId},
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

        let mut state = self.state.write().unwrap();
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
        let mut events = self.events.write().unwrap();
        events.push(event);
    }

    /// Get a snapshot of all events in the session.
    /// Used by the runner for compaction decisions.
    pub fn events_snapshot(&self) -> Vec<Event> {
        let events = self.events.read().unwrap();
        events.clone()
    }
}

impl adk_core::Session for MutableSession {
    fn id(&self) -> &SessionId {
        self.inner.id()
    }

    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    fn user_id(&self) -> &UserId {
        self.inner.user_id()
    }

    fn state(&self) -> &dyn adk_core::State {
        // SAFETY: We implement State for MutableSession, so this cast is valid.
        // This pattern allows us to return a reference to self as a State trait object.
        unsafe { &*(self as *const Self as *const dyn adk_core::State) }
    }

    fn conversation_history(&self) -> Vec<adk_core::Content> {
        let events = self.events.read().unwrap();
        let mut history = Vec::new();

        // Find the most recent compaction event — everything before its
        // end_timestamp has been summarized and should be replaced by the
        // compacted content.
        let mut compaction_boundary = None;
        for event in events.iter().rev() {
            if let Some(ref compaction) = event.actions.compaction {
                // Insert the summary as the first history entry
                history.push(compaction.compacted_content.clone());
                compaction_boundary = Some(compaction.end_timestamp);
                break;
            }
        }

        for event in events.iter() {
            // Skip the compaction event itself (author == "system" with compaction data)
            if event.actions.compaction.is_some() {
                continue;
            }

            // Skip events that were already compacted
            if let Some(boundary) = compaction_boundary {
                if event.timestamp <= boundary {
                    continue;
                }
            }

            if let Some(content) = &event.llm_response.content {
                let mut mapped_content = content.clone();
                let author_str = event.author.to_string();
                let role_str = content.role.to_string();
                mapped_content.role = match (author_str.as_str(), role_str.as_str()) {
                    ("user", _) => adk_core::types::Role::User,
                    (_, "function" | "tool") => content.role.clone(),
                    _ => adk_core::types::Role::Model,
                };
                history.push(mapped_content);
            }
        }

        history
    }
}

impl adk_core::State for MutableSession {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        let state = self.state.read().unwrap();
        state.get(key).cloned()
    }

    fn set(&mut self, key: String, value: serde_json::Value) {
        let mut state = self.state.write().unwrap();
        state.insert(key, value);
    }

    fn all(&self) -> HashMap<String, serde_json::Value> {
        let state = self.state.read().unwrap();
        state.clone()
    }
}

/// `RunnerContext` is the concrete implementation of `InvocationContextTrait` used during agent execution.
///
/// It holds the reference to the agent, the artifacts and memory services,
/// and the session state.
pub struct RunnerContext {
    base: adk_core::AdkContext,
    agent: Arc<dyn Agent>,
    artifacts: Option<Arc<dyn Artifacts>>,
    memory: Option<Arc<dyn Memory>>,
    run_config: RunConfig,
    ended: Arc<AtomicBool>,
    /// Mutable session that allows state to be updated during execution.
    /// This is shared across all agents in a workflow, enabling state
    /// propagation between sequential/parallel agents.
    session: Arc<MutableSession>,
}

impl RunnerContext {
    pub fn new(
        invocation_id: InvocationId,
        agent: Arc<dyn Agent>,
        user_id: UserId,
        app_name: String,
        session_id: SessionId,
        user_content: Content,
        session: Arc<dyn AdkSession>,
    ) -> Self {
        let base = adk_core::AdkContext::builder()
            .invocation_id(invocation_id)
            .agent_name(agent.name())
            .user_id(user_id)
            .app_name(app_name)
            .session_id(session_id)
            .user_content(user_content)
            .build();
        Self {
            base,
            agent,
            artifacts: None,
            memory: None,
            run_config: RunConfig::default(),
            ended: Arc::new(AtomicBool::new(false)),
            session: Arc::new(MutableSession::new(session)),
        }
    }

    /// Create an RunnerContext with an existing MutableSession.
    /// This allows sharing the same mutable session across multiple contexts
    /// (e.g., for agent transfers).
    pub fn with_mutable_session(
        invocation_id: InvocationId,
        agent: Arc<dyn Agent>,
        user_id: UserId,
        app_name: String,
        session_id: SessionId,
        user_content: Content,
        session: Arc<MutableSession>,
    ) -> Self {
        let base = adk_core::AdkContext::builder()
            .invocation_id(invocation_id)
            .agent_name(agent.name())
            .user_id(user_id)
            .app_name(app_name)
            .session_id(session_id)
            .user_content(user_content)
            .build();
        Self {
            base,
            agent,
            artifacts: None,
            memory: None,
            run_config: RunConfig::default(),
            ended: Arc::new(AtomicBool::new(false)),
            session,
        }
    }

    pub fn with_branch(mut self, branch: String) -> Self {
        self.base.set_branch(branch);
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

    /// Get a reference to the mutable session.
    /// This allows the Runner to apply state deltas when events are processed.
    pub fn mutable_session(&self) -> &Arc<MutableSession> {
        &self.session
    }
}

#[async_trait]
impl ReadonlyContext for RunnerContext {
    fn identity(&self) -> &AdkIdentity {
        self.base.identity()
    }

    fn user_content(&self) -> &Content {
        self.base.user_content()
    }

    fn metadata(&self) -> &HashMap<String, String> {
        self.base.metadata()
    }
}

#[async_trait]
impl CallbackContext for RunnerContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.artifacts.clone()
    }
}

#[async_trait]
impl InvocationContextTrait for RunnerContext {
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
}
