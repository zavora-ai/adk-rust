#[cfg(feature = "skills")]
use crate::skill_shim::load_skill_index;
use crate::skill_shim::{SelectionPolicy, SkillIndex};
use adk_core::{
    AfterAgentCallback, Agent, BeforeAgentCallback, CallbackContext, Event, EventStream,
    InvocationContext, Result, SharedState,
};
use async_stream::stream;
use async_trait::async_trait;
use std::sync::Arc;

use super::branch_context::{BranchContext, derive_sub_branch};
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
    /// Create a new parallel agent with the given name and sub-agents.
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

    /// Set the agent description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
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

    fn supports_agent_transfer(&self) -> bool {
        // Deterministic workflow agent: on cross-turn resumption the runner
        // must restart from this root so every sub-agent runs again, rather
        // than resuming a single sub-agent that responded last.
        false
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
            use futures::stream::{StreamExt, select_all};

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


            // Create shared state if enabled (fresh per run)
            let shared = if shared_state_enabled {
                Some(Arc::new(SharedState::new()))
            } else {
                None
            };

            // Each sub-agent gets its own stream that resolves `run()` and drains
            // the resulting events. Merging these with `select_all` polls every
            // sub-agent concurrently, which is what makes this agent parallel:
            // `Agent::run` only *builds* an `EventStream`, so awaiting the run
            // futures together is not enough — the streams themselves have to be
            // polled together. Draining one stream to completion before touching
            // the next made nominally parallel branches run one at a time.
            //
            // Polling from a single task also gives the backpressure the ADK
            // Python and Go implementations arrange explicitly (a resume signal
            // and an ack channel respectively): a sub-agent cannot run ahead
            // while an already-produced event is still being consumed upstream,
            // so the runner's per-event persistence stays in step with execution.
            //
            // Dropping the merged stream drops every sub-agent stream with it, so
            // a consumer that stops early tears down in-flight sub-agents instead
            // of leaving them running.
            let mut merged = {
                // Item is (sub-agent index, event result). The index lets a failure
                // be attributed to the branch that produced it.
                type BranchStream =
                    std::pin::Pin<Box<dyn futures::Stream<Item = (usize, Result<Event>)> + Send>>;
                let mut per_agent: Vec<BranchStream> = Vec::with_capacity(sub_agents.len());

                for (index, agent) in sub_agents.into_iter().enumerate() {
                    let base: Arc<dyn InvocationContext> = if let Some(ref shared) = shared {
                        Arc::new(SharedStateContext::new(run_ctx.clone(), shared.clone()))
                    } else {
                        run_ctx.clone()
                    };

                    // Each sub-agent runs on its own branch, so a history read
                    // scoped by branch excludes what its siblings produced while
                    // still seeing the conversation that led to the fan-out. The
                    // shape mirrors ADK Python (`{parent}.{agent}.{sub_agent}`)
                    // and ADK Go.
                    let branch = derive_sub_branch(
                        base.branch(),
                        &format!("{agent_name}.{}", agent.name()),
                    );
                    let ctx: Arc<dyn InvocationContext> =
                        Arc::new(BranchContext::new(base, branch.clone()));

                    per_agent.push(Box::pin(stream! {
                        match agent.run(ctx).await {
                            Ok(mut events) => {
                                while let Some(event_result) = events.next().await {
                                    let failed = event_result.is_err();
                                    // Record which branch produced the event so a
                                    // later branch-scoped history read can exclude
                                    // it from siblings. A nested workflow may have
                                    // already stamped a deeper branch; leave it.
                                    let event_result = event_result.map(|mut event| {
                                        if event.branch.is_empty() {
                                            event.branch = branch.clone();
                                        }
                                        event
                                    });
                                    yield (index, event_result);
                                    if failed {
                                        // Abandon this branch, leave the others running.
                                        break;
                                    }
                                }
                            }
                            Err(e) => yield (index, Err(e)),
                        }
                    }));
                }

                select_all(per_agent)
            };

            // Errors are collected with their sub-agent index so the reported
            // error stays deterministic. With branches running concurrently,
            // "whichever failed first" would be a race; the lowest index matches
            // the declared sub-agent order this agent was constructed with.
            let mut failures: Vec<(usize, adk_core::AdkError)> = Vec::new();

            while let Some((index, event_result)) = merged.next().await {
                match event_result {
                    Ok(event) => yield Ok(event),
                    Err(e) => failures.push((index, e)),
                }
            }

            // After all agents complete, propagate the first error if any
            if let Some((_, e)) = failures.into_iter().min_by_key(|(index, _)| *index) {
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
