//! Context wrappers the team, the plan graph and the goal engine run agents and tools
//! under (spec 3.1 §7.10, §7.11).
//!
//! Every wrapper forwards the whole context contract — identity, acting context,
//! authentication, cancellation, delegation depth, secrets — and overrides only what it
//! names, so an agent behaves the same under it as under the runner's own context.

use std::collections::HashMap;
#[cfg(feature = "team")]
use std::sync::RwLock;
use std::sync::{Arc, Mutex};

#[cfg(any(feature = "team", feature = "graph"))]
use adk_core::{Agent, SecretRequest};
use adk_core::{
    Artifacts, CallbackContext, Content, Event, EventActions, InvocationContext, Memory,
    MemoryEntry, ReadonlyContext, Result, RunConfig, Session, SharedState, State, ToolContext,
};
use async_trait::async_trait;
use serde_json::Value;

#[cfg(feature = "team")]
/// Session state seen through a member's own, unmerged writes (spec 3.1 §7.10).
///
/// A member's state writes never reach its caller (`state_write_keys: []`); they stay in
/// this overlay so the member still reads what it wrote, such as its own todos.
pub(crate) struct OverlayState {
    inner: Arc<dyn InvocationContext>,
    writes: RwLock<HashMap<String, Value>>,
}

#[cfg(feature = "team")]
impl OverlayState {
    fn new(inner: Arc<dyn InvocationContext>) -> Self {
        Self { inner, writes: RwLock::new(HashMap::new()) }
    }

    /// Keeps `delta` local to the member.
    pub(crate) fn absorb(&self, delta: &HashMap<String, Value>) {
        if delta.is_empty() {
            return;
        }
        let mut writes = self.writes.write().unwrap_or_else(|e| e.into_inner());
        for (key, value) in delta {
            writes.insert(key.clone(), value.clone());
        }
    }
}

#[cfg(feature = "team")]
impl State for OverlayState {
    fn get(&self, key: &str) -> Option<Value> {
        let local = self.writes.read().unwrap_or_else(|e| e.into_inner()).get(key).cloned();
        match local {
            Some(Value::Null) => None,
            Some(value) => Some(value),
            None => self.inner.session().state().get(key),
        }
    }

    fn set(&mut self, key: String, value: Value) {
        self.writes.get_mut().unwrap_or_else(|e| e.into_inner()).insert(key, value);
    }

    fn all(&self) -> HashMap<String, Value> {
        let mut all = self.inner.session().state().all();
        for (key, value) in self.writes.read().unwrap_or_else(|e| e.into_inner()).iter() {
            if value.is_null() {
                all.remove(key);
            } else {
                all.insert(key.clone(), value.clone());
            }
        }
        all
    }
}

/// The wrapped context's session with [`OverlayState`] as its state.
#[cfg(feature = "team")]
pub(crate) struct OverlaySession {
    inner: Arc<dyn InvocationContext>,
    state: OverlayState,
}

#[cfg(feature = "team")]
impl Session for OverlaySession {
    fn id(&self) -> &str {
        self.inner.session().id()
    }

    fn app_name(&self) -> &str {
        self.inner.session().app_name()
    }

    fn user_id(&self) -> &str {
        self.inner.session().user_id()
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn conversation_history(&self) -> Vec<Content> {
        self.inner.session().conversation_history()
    }

    fn conversation_history_for_agent(&self, agent_name: &str) -> Vec<Content> {
        self.inner.session().conversation_history_for_agent(agent_name)
    }

    fn conversation_history_scoped(&self, agent_name: Option<&str>, branch: &str) -> Vec<Content> {
        self.inner.session().conversation_history_scoped(agent_name, branch)
    }

    fn conversation_history_with_context(&self, agent_name: &str, branch: &str) -> Vec<Content> {
        self.inner.session().conversation_history_with_context(agent_name, branch)
    }

    fn records_invocation_events(&self) -> bool {
        self.inner.session().records_invocation_events()
    }

    fn held_tool_calls(&self, agent_name: &str, branch: &str) -> Vec<adk_core::HeldToolCall> {
        self.inner.session().held_tool_calls(agent_name, branch)
    }

    fn workflow_resume_positions(
        &self,
        workflow: &str,
        branch: &str,
    ) -> Vec<adk_core::WorkflowPosition> {
        self.inner.session().workflow_resume_positions(workflow, branch)
    }

    fn append_to_history(&self, content: Content) {
        self.inner.session().append_to_history(content);
    }
}

/// An invocation context over another one, overriding only what is set.
#[cfg(any(feature = "team", feature = "graph"))]
pub(crate) struct ScopedContext {
    inner: Arc<dyn InvocationContext>,
    agent: Option<Arc<dyn Agent>>,
    agent_name: Option<String>,
    user_content: Option<Content>,
    #[cfg(feature = "team")]
    overlay: Option<OverlaySession>,
}

#[cfg(any(feature = "team", feature = "graph"))]
impl ScopedContext {
    pub(crate) fn new(inner: Arc<dyn InvocationContext>) -> Self {
        Self {
            inner,
            agent: None,
            agent_name: None,
            user_content: None,
            #[cfg(feature = "team")]
            overlay: None,
        }
    }

    /// Runs `agent` under the wrapped context, named `name`.
    #[cfg(feature = "graph")]
    pub(crate) fn for_agent(mut self, agent: Arc<dyn Agent>, name: impl Into<String>) -> Self {
        self.agent = Some(agent);
        self.agent_name = Some(name.into());
        self
    }

    /// Presents `content` as the turn's user content.
    #[cfg(feature = "graph")]
    pub(crate) fn with_user_content(mut self, content: Content) -> Self {
        self.user_content = Some(content);
        self
    }

    /// Keeps state writes local to this context (see [`OverlayState`]).
    #[cfg(feature = "team")]
    pub(crate) fn with_state_overlay(mut self) -> Self {
        self.overlay = Some(OverlaySession {
            inner: self.inner.clone(),
            state: OverlayState::new(self.inner.clone()),
        });
        self
    }

    /// The overlay's state, when [`with_state_overlay`](Self::with_state_overlay) set one.
    #[cfg(feature = "team")]
    pub(crate) fn overlay(&self) -> Option<&OverlayState> {
        self.overlay.as_ref().map(|overlay| &overlay.state)
    }
}

#[cfg(any(feature = "team", feature = "graph"))]
#[async_trait]
impl ReadonlyContext for ScopedContext {
    fn invocation_id(&self) -> &str {
        self.inner.invocation_id()
    }

    fn agent_name(&self) -> &str {
        self.agent_name.as_deref().unwrap_or_else(|| self.inner.agent_name())
    }

    fn user_id(&self) -> &str {
        self.inner.user_id()
    }

    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    fn session_id(&self) -> &str {
        self.inner.session_id()
    }

    fn branch(&self) -> &str {
        self.inner.branch()
    }

    fn user_content(&self) -> &Content {
        self.user_content.as_ref().unwrap_or_else(|| self.inner.user_content())
    }

    fn acting_context(&self) -> adk_core::ActingContext {
        self.inner.acting_context()
    }

    fn is_authenticated(&self) -> bool {
        self.inner.is_authenticated()
    }

    // A wrapper context that drops `state()` (the team's does) would hide the session state
    // from dynamic toolsets; the session's own state is the same view the runner exposes.
    fn state(&self) -> Option<&dyn State> {
        #[cfg(feature = "team")]
        if let Some(overlay) = &self.overlay {
            return Some(&overlay.state);
        }
        ReadonlyContext::state(self.inner.as_ref()).or_else(|| Some(self.inner.session().state()))
    }
}

#[cfg(any(feature = "team", feature = "graph"))]
#[async_trait]
impl CallbackContext for ScopedContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.inner.artifacts()
    }

    fn tool_outcome(&self) -> Option<adk_core::ToolOutcome> {
        self.inner.tool_outcome()
    }

    fn tool_name(&self) -> Option<&str> {
        self.inner.tool_name()
    }

    fn tool_input(&self) -> Option<&Value> {
        self.inner.tool_input()
    }

    fn shared_state(&self) -> Option<Arc<SharedState>> {
        self.inner.shared_state()
    }
}

#[cfg(any(feature = "team", feature = "graph"))]
#[async_trait]
impl InvocationContext for ScopedContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone().unwrap_or_else(|| self.inner.agent())
    }

    fn memory(&self) -> Option<Arc<dyn Memory>> {
        self.inner.memory()
    }

    fn session(&self) -> &dyn Session {
        #[cfg(feature = "team")]
        if let Some(overlay) = &self.overlay {
            return overlay;
        }
        self.inner.session()
    }

    fn run_config(&self) -> &RunConfig {
        self.inner.run_config()
    }

    fn end_invocation(&self) {
        self.inner.end_invocation();
    }

    fn ended(&self) -> bool {
        self.inner.ended()
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn user_scopes(&self) -> Vec<String> {
        self.inner.user_scopes()
    }

    fn request_metadata(&self) -> HashMap<String, Value> {
        self.inner.request_metadata()
    }

    fn authoritative_transfer_targets(&self) -> bool {
        self.inner.authoritative_transfer_targets()
    }

    fn delegation_depth(&self) -> u32 {
        self.inner.delegation_depth()
    }

    fn max_delegation_depth(&self) -> Option<u32> {
        self.inner.max_delegation_depth()
    }

    fn orchestration_root_invocation_id(&self) -> &str {
        self.inner.orchestration_root_invocation_id()
    }

    fn orchestration_edge_id(&self) -> Option<&str> {
        self.inner.orchestration_edge_id()
    }

    fn requires_tool_confirmation(&self, tool_name: &str) -> bool {
        self.inner.requires_tool_confirmation(tool_name)
    }

    fn steering_inbox(&self) -> Option<adk_core::SteeringInbox> {
        self.inner.steering_inbox()
    }

    async fn get_secret(&self, name: &str) -> Result<Option<String>> {
        self.inner.get_secret(name).await
    }

    async fn get_secret_for(&self, request: &SecretRequest) -> Result<Option<String>> {
        self.inner.get_secret_for(request).await
    }
}

/// A tool context over an invocation, for the harness's own tool executions outside a
/// model turn: the goal's `until` check and a plan step's review delegation.
pub(crate) struct InvocationToolContext {
    inner: Arc<dyn InvocationContext>,
    function_call_id: String,
    actions: Mutex<EventActions>,
    events: Option<EventSink>,
}

/// Where a tool's emitted events go.
pub(crate) type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

impl InvocationToolContext {
    pub(crate) fn new(
        inner: Arc<dyn InvocationContext>,
        function_call_id: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            function_call_id: function_call_id.into(),
            actions: Mutex::new(EventActions::default()),
            events: None,
        }
    }

    /// Sends the events a tool emits, such as a delegate's, to `sink`.
    #[cfg(all(feature = "graph", feature = "team"))]
    pub(crate) fn with_event_sink(mut self, sink: EventSink) -> Self {
        self.events = Some(sink);
        self
    }
}

#[async_trait]
impl ReadonlyContext for InvocationToolContext {
    fn invocation_id(&self) -> &str {
        self.inner.invocation_id()
    }

    fn agent_name(&self) -> &str {
        self.inner.agent_name()
    }

    fn user_id(&self) -> &str {
        self.inner.user_id()
    }

    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    fn session_id(&self) -> &str {
        self.inner.session_id()
    }

    fn branch(&self) -> &str {
        self.inner.branch()
    }

    fn user_content(&self) -> &Content {
        self.inner.user_content()
    }

    fn acting_context(&self) -> adk_core::ActingContext {
        self.inner.acting_context()
    }

    fn is_authenticated(&self) -> bool {
        self.inner.is_authenticated()
    }

    fn state(&self) -> Option<&dyn State> {
        ReadonlyContext::state(self.inner.as_ref())
    }
}

#[async_trait]
impl CallbackContext for InvocationToolContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.inner.artifacts()
    }

    fn shared_state(&self) -> Option<Arc<SharedState>> {
        self.inner.shared_state()
    }
}

#[async_trait]
impl ToolContext for InvocationToolContext {
    fn function_call_id(&self) -> &str {
        &self.function_call_id
    }

    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap_or_else(|e| e.into_inner()) = actions;
    }

    async fn search_memory(&self, query: &str) -> Result<Vec<MemoryEntry>> {
        match self.inner.memory() {
            Some(memory) => memory.search(query).await,
            None => Ok(Vec::new()),
        }
    }

    fn memory(&self) -> Option<Arc<dyn Memory>> {
        self.inner.memory()
    }

    fn session(&self) -> Option<&dyn Session> {
        Some(self.inner.session())
    }

    fn run_config(&self) -> Option<&RunConfig> {
        Some(self.inner.run_config())
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn request_metadata(&self) -> HashMap<String, Value> {
        self.inner.request_metadata()
    }

    fn delegation_depth(&self) -> u32 {
        self.inner.delegation_depth()
    }

    fn max_delegation_depth(&self) -> Option<u32> {
        self.inner.max_delegation_depth()
    }

    fn orchestration_root_invocation_id(&self) -> &str {
        self.inner.orchestration_root_invocation_id()
    }

    fn orchestration_edge_id(&self) -> Option<&str> {
        self.inner.orchestration_edge_id()
    }

    fn user_scopes(&self) -> Vec<String> {
        self.inner.user_scopes()
    }

    async fn get_secret(&self, name: &str) -> Result<Option<String>> {
        self.inner.get_secret(name).await
    }

    async fn emit_event(&self, event: Event) {
        if let Some(sink) = &self.events {
            sink(event);
        }
    }
}
