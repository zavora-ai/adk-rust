//! Rule-based conditional routing agent.
//!
//! `ConditionalAgent` provides **synchronous, rule-based** conditional routing.
//! The condition function is evaluated synchronously and must return a boolean.
//!
//! # When to Use
//!
//! Use `ConditionalAgent` for **deterministic** routing decisions:
//! - A/B testing based on session state or flags
//! - Environment-based routing (e.g., production vs staging)
//! - Feature flag checks
//!
//! # For Intelligent Routing
//!
//! If you need **LLM-based intelligent routing** where the model classifies
//! user intent and routes accordingly, use [`LlmConditionalAgent`] instead:
//!
//! ```rust,ignore
//! // LLM decides which agent to route to
//! let router = LlmConditionalAgent::builder("router", model)
//!     .instruction("Classify as 'technical' or 'general'")
//!     .route("technical", tech_agent)
//!     .route("general", general_agent)
//!     .build()?;
//! ```
//!
//! See [`crate::workflow::LlmConditionalAgent`] for details.

#[cfg(feature = "skills")]
use crate::skill_shim::load_skill_index;
use crate::skill_shim::{SelectionPolicy, SkillIndex};
use adk_core::{
    AfterAgentCallback, Agent, BeforeAgentCallback, CallbackContext, Event, EventStream,
    InvocationContext, Result, WorkflowPosition,
};
use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::Arc;

type ConditionFn = Arc<dyn Fn(&dyn InvocationContext) -> bool + Send + Sync>;

/// Rule-based conditional routing agent.
///
/// Executes one of two sub-agents based on a synchronous condition function.
/// For LLM-based intelligent routing, use [`crate::LlmConditionalAgent`] instead.
///
/// # Example
///
/// ```rust,ignore
/// // Route based on session state flag
/// let router = ConditionalAgent::new(
///     "premium_router",
///     |ctx| ctx.session().state().get("is_premium").map(|v| v.as_bool()).flatten().unwrap_or(false),
///     Arc::new(premium_agent),
/// ).with_else(Arc::new(basic_agent));
/// ```
pub struct ConditionalAgent {
    name: String,
    description: String,
    condition: ConditionFn,
    if_agent: Arc<dyn Agent>,
    else_agent: Option<Arc<dyn Agent>>,
    /// Cached list of all branch agents for tree discovery via `sub_agents()`.
    all_agents: Vec<Arc<dyn Agent>>,
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
    before_callbacks: Arc<Vec<BeforeAgentCallback>>,
    after_callbacks: Arc<Vec<AfterAgentCallback>>,
}

impl ConditionalAgent {
    /// Create a new conditional agent with a condition function and the if-branch agent.
    pub fn new<F>(name: impl Into<String>, condition: F, if_agent: Arc<dyn Agent>) -> Self
    where
        F: Fn(&dyn InvocationContext) -> bool + Send + Sync + 'static,
    {
        let all_agents = vec![if_agent.clone()];
        Self {
            name: name.into(),
            description: String::new(),
            condition: Arc::new(condition),
            if_agent,
            else_agent: None,
            all_agents,
            skills_index: None,
            skill_policy: SelectionPolicy::default(),
            max_skill_chars: 2000,
            before_callbacks: Arc::new(Vec::new()),
            after_callbacks: Arc::new(Vec::new()),
        }
    }

    /// Set the agent description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Set the else-branch agent executed when the condition is false.
    pub fn with_else(mut self, else_agent: Arc<dyn Agent>) -> Self {
        self.all_agents.push(else_agent.clone());
        self.else_agent = Some(else_agent);
        self
    }

    /// Add a before-agent callback.
    pub fn before_callback(mut self, callback: BeforeAgentCallback) -> Self {
        if let Some(callbacks) = Arc::get_mut(&mut self.before_callbacks) {
            callbacks.push(callback);
        }
        self
    }

    /// Add an after-agent callback.
    pub fn after_callback(mut self, callback: AfterAgentCallback) -> Self {
        if let Some(callbacks) = Arc::get_mut(&mut self.after_callbacks) {
            callbacks.push(callback);
        }
        self
    }

    /// Set a preloaded skills index for this agent.
    #[cfg(feature = "skills")]
    pub fn with_skills(mut self, index: SkillIndex) -> Self {
        self.skills_index = Some(Arc::new(index));
        self
    }

    /// Auto-load skills from `.skills/` in the current working directory.
    #[cfg(feature = "skills")]
    pub fn with_auto_skills(self) -> Result<Self> {
        self.with_skills_from_root(".")
    }

    /// Auto-load skills from `.skills/` under a custom root directory.
    #[cfg(feature = "skills")]
    pub fn with_skills_from_root(mut self, root: impl AsRef<std::path::Path>) -> Result<Self> {
        let index = load_skill_index(root).map_err(|e| adk_core::AdkError::agent(e.to_string()))?;
        self.skills_index = Some(Arc::new(index));
        Ok(self)
    }

    /// Customize skill selection behavior.
    #[cfg(feature = "skills")]
    pub fn with_skill_policy(mut self, policy: SelectionPolicy) -> Self {
        self.skill_policy = policy;
        self
    }

    /// Limit injected skill content length.
    #[cfg(feature = "skills")]
    pub fn with_skill_budget(mut self, max_chars: usize) -> Self {
        self.max_skill_chars = max_chars;
        self
    }
}

#[async_trait]
impl Agent for ConditionalAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.all_agents
    }

    fn supports_agent_transfer(&self) -> bool {
        // Deterministic workflow agent: on cross-turn resumption the runner
        // must restart from this root so the condition is re-evaluated, rather
        // than resuming a single branch that responded last.
        false
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let run_ctx = super::skill_context::with_skill_injected_context(
            ctx,
            self.skills_index.as_ref(),
            &self.skill_policy,
            self.max_skill_chars,
        );
        let before_callbacks = self.before_callbacks.clone();
        let after_callbacks = self.after_callbacks.clone();
        let if_agent = self.if_agent.clone();
        let else_agent = self.else_agent.clone();
        let agent_name = self.name.clone();
        let invocation_id = run_ctx.invocation_id().to_string();
        let condition = self.condition.clone();
        // A run that resumes a hold returns to the branch that holds it without evaluating
        // the condition again, which may have changed since.
        let resuming = run_ctx
            .session()
            .workflow_resume_positions(&agent_name, run_ctx.branch())
            .into_iter()
            .find_map(|position| {
                self.all_agents.iter().find(|agent| agent.name() == position.sub_agent).cloned()
            });

        let s = stream! {
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

            let target_agent = match resuming {
                Some(agent) => Some(agent),
                None if condition(run_ctx.as_ref()) => Some(if_agent),
                None => else_agent,
            };

            if let Some(agent) = target_agent {
                let mut stream = match agent.run(run_ctx.clone()).await {
                    Ok(stream) => stream,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };

                let mut held = false;
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(mut event) => {
                            if event.actions.tool_confirmation.is_some() {
                                held = true;
                                event.actions.workflow_positions.insert(
                                    agent_name.clone(),
                                    WorkflowPosition::new(0, agent.name()),
                                );
                            }
                            yield Ok(event);
                        }
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    }
                }
                // A held call pauses the branch; the after-agent callbacks wait for the run
                // that finishes it.
                if held {
                    return;
                }
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
