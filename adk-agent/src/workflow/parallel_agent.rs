use adk_core::{
    AfterAgentCallback, Agent, BeforeAgentCallback, CallbackContext, Event, EventStream,
    InvocationContext, Result, SharedState,
};
use adk_skill::{SelectionPolicy, SkillIndex, load_skill_index};
use async_stream::stream;
use async_trait::async_trait;
use std::sync::Arc;

use super::shared_state_context::SharedStateContext;

/// Parallel agent executes sub-agents concurrently
pub struct ParallelAgent {
    name: String,
    description: String,
    sub_agents: Vec<Arc<dyn Agent>>,
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
    before_callbacks: Arc<Vec<BeforeAgentCallback>>,
    after_callbacks: Arc<Vec<AfterAgentCallback>>,
    shared_state_enabled: bool,
}

impl ParallelAgent {
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            sub_agents,
            skills_index: None,
            skill_policy: SelectionPolicy::default(),
            max_skill_chars: 2000,
            before_callbacks: Arc::new(Vec::new()),
            after_callbacks: Arc::new(Vec::new()),
            shared_state_enabled: false,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn before_callback(mut self, callback: BeforeAgentCallback) -> Self {
        if let Some(callbacks) = Arc::get_mut(&mut self.before_callbacks) {
            callbacks.push(callback);
        }
        self
    }

    pub fn after_callback(mut self, callback: AfterAgentCallback) -> Self {
        if let Some(callbacks) = Arc::get_mut(&mut self.after_callbacks) {
            callbacks.push(callback);
        }
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
        let index = load_skill_index(root).map_err(|e| adk_core::AdkError::agent(e.to_string()))?;
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

    /// Enables shared state coordination for sub-agents.
    ///
    /// When enabled, a fresh `SharedState` instance is created for each
    /// `run()` invocation and injected into each sub-agent's context.
    /// Sub-agents can then use `ctx.shared_state()` to access the store.
    pub fn with_shared_state(mut self) -> Self {
        self.shared_state_enabled = true;
        self
    }
}

#[async_trait]
impl Agent for ParallelAgent {
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
        let run_ctx = super::skill_context::with_skill_injected_context(
            ctx,
            self.skills_index.as_ref(),
            &self.skill_policy,
            self.max_skill_chars,
        );
        let before_callbacks = self.before_callbacks.clone();
        let after_callbacks = self.after_callbacks.clone();
        let agent_name = self.name.clone();
        let invocation_id = run_ctx.invocation_id().to_string();
        let shared_state_enabled = self.shared_state_enabled;

        let s = stream! {
            use futures::stream::{FuturesUnordered, StreamExt};

            for callback in before_callbacks.as_ref() {
                match callback(run_ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut early_event = Event::new(&invocation_id);
                        early_event.author = agent_name.clone();
                        early_event.llm_response.content = Some(content);
                        yield Ok(early_event);

                        for after_callback in after_callbacks.as_ref() {
                            match after_callback(run_ctx.clone() as Arc<dyn CallbackContext>).await {
                                Ok(Some(after_content)) => {
                                    let mut after_event = Event::new(&invocation_id);
                                    after_event.author = agent_name.clone();
                                    after_event.llm_response.content = Some(after_content);
                                    yield Ok(after_event);
                                    return;
                                }
                                Ok(None) => continue,
                                Err(e) => {
                                    yield Err(e);
                                    return;
                                }
                            }
                        }
                        return;
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }

            let mut futures = FuturesUnordered::new();

            // Create shared state if enabled (fresh per run)
            let shared = if shared_state_enabled {
                Some(Arc::new(SharedState::new()))
            } else {
                None
            };

            for agent in sub_agents {
                let ctx: Arc<dyn InvocationContext> = if let Some(ref shared) = shared {
                    Arc::new(SharedStateContext::new(run_ctx.clone(), shared.clone()))
                } else {
                    run_ctx.clone()
                };
                futures.push(async move {
                    agent.run(ctx).await
                });
            }

            let mut first_error: Option<adk_core::AdkError> = None;

            while let Some(result) = futures.next().await {
                match result {
                    Ok(mut stream) => {
                        while let Some(event_result) = stream.next().await {
                            match event_result {
                                Ok(event) => yield Ok(event),
                                Err(e) => {
                                    if first_error.is_none() {
                                        first_error = Some(e);
                                    }
                                    // Continue draining other agents instead of returning
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if first_error.is_none() {
                            first_error = Some(e);
                        }
                        // Continue draining remaining futures to avoid resource leaks
                    }
                }
            }

            // After all agents complete, propagate the first error if any
            if let Some(e) = first_error {
                yield Err(e);
                return;
            }

            for callback in after_callbacks.as_ref() {
                match callback(run_ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut after_event = Event::new(&invocation_id);
                        after_event.author = agent_name.clone();
                        after_event.llm_response.content = Some(content);
                        yield Ok(after_event);
                        break;
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }
}
