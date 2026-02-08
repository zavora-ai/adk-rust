use adk_core::{
    AfterAgentCallback, Agent, BeforeAgentCallback, EventStream, InvocationContext, Result,
};
use adk_skill::{SelectionPolicy, SkillIndex, load_skill_index};
use async_stream::stream;
use async_trait::async_trait;
use std::sync::Arc;

/// Parallel agent executes sub-agents concurrently
pub struct ParallelAgent {
    name: String,
    description: String,
    sub_agents: Vec<Arc<dyn Agent>>,
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
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
            before_callbacks: Vec::new(),
            after_callbacks: Vec::new(),
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn before_callback(mut self, callback: BeforeAgentCallback) -> Self {
        self.before_callbacks.push(callback);
        self
    }

    pub fn after_callback(mut self, callback: AfterAgentCallback) -> Self {
        self.after_callbacks.push(callback);
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

        let s = stream! {
            use futures::stream::{FuturesUnordered, StreamExt};

            let mut futures = FuturesUnordered::new();

            for agent in sub_agents {
                let ctx = run_ctx.clone();
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
            }
        };

        Ok(Box::pin(s))
    }
}
