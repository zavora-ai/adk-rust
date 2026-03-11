use adk_core::{
    AfterAgentCallback, Agent, BeforeAgentCallback, CallbackContext, Content, Event, EventStream,
    InvocationContext, ReadonlyContext, Result, Session, State,
};
use adk_skill::{SelectionPolicy, SkillIndex, load_skill_index};
use async_stream::stream;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Default maximum iterations for LoopAgent when none is specified.
/// Prevents infinite loops from consuming unbounded resources.
pub const DEFAULT_LOOP_MAX_ITERATIONS: u32 = 1000;

/// Loop agent executes sub-agents repeatedly for N iterations or until escalation
pub struct LoopAgent {
    name: String,
    description: String,
    sub_agents: Vec<Arc<dyn Agent>>,
    max_iterations: u32,
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
    before_callbacks: Arc<Vec<BeforeAgentCallback>>,
    after_callbacks: Arc<Vec<AfterAgentCallback>>,
}

impl LoopAgent {
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            sub_agents,
            max_iterations: DEFAULT_LOOP_MAX_ITERATIONS,
            skills_index: None,
            skill_policy: SelectionPolicy::default(),
            max_skill_chars: 2000,
            before_callbacks: Arc::new(Vec::new()),
            after_callbacks: Arc::new(Vec::new()),
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    pub fn with_skills(mut self, index: SkillIndex) -> Self {
        self.skills_index = Some(Arc::new(index));
        self
    }

    pub fn with_auto_skills(self) -> Result<Self> {
        self.with_skills_from_root(".")
    }

    pub fn with_skills_from_root(mut self, root: impl AsRef<std::path::Path>) -> Result<Self> {
        let index = load_skill_index(root).map_err(|e| adk_core::AdkError::Agent(e.to_string()))?;
        self.skills_index = Some(Arc::new(index));
        Ok(self)
    }

    pub fn with_skill_policy(mut self, policy: SelectionPolicy) -> Self {
        self.skill_policy = policy;
        self
    }

    pub fn with_skill_budget(mut self, max_chars: usize) -> Self {
        self.max_skill_chars = max_chars;
        self
    }

    pub fn before_callback(mut self, callback: BeforeAgentCallback) -> Self {
        Arc::get_mut(&mut self.before_callbacks)
            .expect("before_callbacks not yet shared")
            .push(callback);
        self
    }

    pub fn after_callback(mut self, callback: AfterAgentCallback) -> Self {
        Arc::get_mut(&mut self.after_callbacks)
            .expect("after_callbacks not yet shared")
            .push(callback);
        self
    }
}

struct HistoryTrackingSession {
    parent_ctx: Arc<dyn InvocationContext>,
    history: Arc<RwLock<Vec<Content>>>,
    state: StateTrackingState,
}

struct StateTrackingState {
    values: RwLock<HashMap<String, serde_json::Value>>,
}

impl StateTrackingState {
    fn new(parent_ctx: &Arc<dyn InvocationContext>) -> Self {
        Self { values: RwLock::new(parent_ctx.session().state().all()) }
    }

    fn apply_delta(&self, delta: &HashMap<String, serde_json::Value>) {
        if delta.is_empty() {
            return;
        }

        let mut values = self.values.write().unwrap_or_else(|e| e.into_inner());
        for (key, value) in delta {
            values.insert(key.clone(), value.clone());
        }
    }
}

impl State for StateTrackingState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.values.read().unwrap_or_else(|e| e.into_inner()).get(key).cloned()
    }

    fn set(&mut self, key: String, value: serde_json::Value) {
        self.values.write().unwrap_or_else(|e| e.into_inner()).insert(key, value);
    }

    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.values.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl HistoryTrackingSession {
    fn new(parent_ctx: Arc<dyn InvocationContext>) -> Self {
        Self {
            history: Arc::new(RwLock::new(parent_ctx.session().conversation_history())),
            state: StateTrackingState::new(&parent_ctx),
            parent_ctx,
        }
    }

    fn apply_event(&self, event: &Event) {
        if let Some(content) = &event.llm_response.content {
            self.append_to_history(content.clone());
        }
        self.state.apply_delta(&event.actions.state_delta);
    }
}

impl Session for HistoryTrackingSession {
    fn id(&self) -> &str {
        self.parent_ctx.session().id()
    }

    fn app_name(&self) -> &str {
        self.parent_ctx.session().app_name()
    }

    fn user_id(&self) -> &str {
        self.parent_ctx.session().user_id()
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn conversation_history(&self) -> Vec<Content> {
        self.history.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn conversation_history_for_agent(&self, _agent_name: &str) -> Vec<Content> {
        self.conversation_history()
    }

    fn append_to_history(&self, content: Content) {
        self.history.write().unwrap_or_else(|e| e.into_inner()).push(content);
    }
}

struct HistoryTrackingContext {
    parent_ctx: Arc<dyn InvocationContext>,
    session: HistoryTrackingSession,
}

impl HistoryTrackingContext {
    fn new(parent_ctx: Arc<dyn InvocationContext>) -> Self {
        let session = HistoryTrackingSession::new(parent_ctx.clone());
        Self { parent_ctx, session }
    }

    fn apply_event(&self, event: &Event) {
        self.session.apply_event(event);
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for HistoryTrackingContext {
    fn invocation_id(&self) -> &str {
        self.parent_ctx.invocation_id()
    }

    fn agent_name(&self) -> &str {
        self.parent_ctx.agent_name()
    }

    fn user_id(&self) -> &str {
        self.parent_ctx.user_id()
    }

    fn app_name(&self) -> &str {
        self.parent_ctx.app_name()
    }

    fn session_id(&self) -> &str {
        self.parent_ctx.session_id()
    }

    fn branch(&self) -> &str {
        self.parent_ctx.branch()
    }

    fn user_content(&self) -> &Content {
        self.parent_ctx.user_content()
    }
}

#[async_trait]
impl CallbackContext for HistoryTrackingContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        self.parent_ctx.artifacts()
    }
}

#[async_trait]
impl InvocationContext for HistoryTrackingContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.parent_ctx.agent()
    }

    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        self.parent_ctx.memory()
    }

    fn session(&self) -> &dyn Session {
        &self.session
    }

    fn run_config(&self) -> &adk_core::RunConfig {
        self.parent_ctx.run_config()
    }

    fn end_invocation(&self) {
        self.parent_ctx.end_invocation();
    }

    fn ended(&self) -> bool {
        self.parent_ctx.ended()
    }

    fn user_scopes(&self) -> Vec<String> {
        self.parent_ctx.user_scopes()
    }

    fn request_metadata(&self) -> HashMap<String, serde_json::Value> {
        self.parent_ctx.request_metadata()
    }
}

#[async_trait]
impl Agent for LoopAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.sub_agents
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let sub_agents = self.sub_agents.clone();
        let max_iterations = self.max_iterations;
        let before_callbacks = self.before_callbacks.clone();
        let after_callbacks = self.after_callbacks.clone();
        let agent_name = self.name.clone();
        let run_ctx = super::skill_context::with_skill_injected_context(
            ctx,
            self.skills_index.as_ref(),
            &self.skill_policy,
            self.max_skill_chars,
        );
        let run_ctx = Arc::new(HistoryTrackingContext::new(run_ctx));

        let s = stream! {
            use futures::StreamExt;

            // ===== BEFORE AGENT CALLBACKS =====
            for callback in before_callbacks.as_ref() {
                match callback(run_ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut early_event = Event::new(run_ctx.invocation_id());
                        early_event.author = agent_name.clone();
                        early_event.llm_response.content = Some(content);
                        yield Ok(early_event);

                        for after_cb in after_callbacks.as_ref() {
                            match after_cb(run_ctx.clone() as Arc<dyn CallbackContext>).await {
                                Ok(Some(after_content)) => {
                                    let mut after_event = Event::new(run_ctx.invocation_id());
                                    after_event.author = agent_name.clone();
                                    after_event.llm_response.content = Some(after_content);
                                    yield Ok(after_event);
                                    return;
                                }
                                Ok(None) => continue,
                                Err(e) => { yield Err(e); return; }
                            }
                        }
                        return;
                    }
                    Ok(None) => continue,
                    Err(e) => { yield Err(e); return; }
                }
            }

            let mut remaining = max_iterations;

            loop {
                let mut should_exit = false;

                for agent in &sub_agents {
                    let mut stream = agent.run(run_ctx.clone() as Arc<dyn InvocationContext>).await?;

                    while let Some(result) = stream.next().await {
                        match result {
                            Ok(event) => {
                                run_ctx.apply_event(&event);
                                if event.actions.escalate {
                                    should_exit = true;
                                }
                                yield Ok(event);
                            }
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                        }
                    }

                    if should_exit {
                        break;
                    }
                }

                if should_exit {
                    break;
                }

                remaining -= 1;
                if remaining == 0 {
                    break;
                }
            }

            // ===== AFTER AGENT CALLBACKS =====
            for callback in after_callbacks.as_ref() {
                match callback(run_ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut after_event = Event::new(run_ctx.invocation_id());
                        after_event.author = agent_name.clone();
                        after_event.llm_response.content = Some(content);
                        yield Ok(after_event);
                        break;
                    }
                    Ok(None) => continue,
                    Err(e) => { yield Err(e); return; }
                }
            }
        };

        Ok(Box::pin(s))
    }
}
