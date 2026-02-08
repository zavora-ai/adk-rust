//! LLM-based intelligent conditional routing agent.
//!
//! `LlmConditionalAgent` provides **intelligent, LLM-based** conditional routing.
//! The model classifies user input and routes to the appropriate sub-agent.
//!
//! # When to Use
//!
//! Use `LlmConditionalAgent` for **intelligent** routing decisions:
//! - Intent classification (technical vs general vs creative)
//! - Multi-way routing (more than 2 destinations)
//! - Context-aware routing that requires understanding the content
//!
//! # For Rule-Based Routing
//!
//! If you need **deterministic, rule-based** routing (e.g., A/B testing,
//! feature flags), use [`ConditionalAgent`] instead.
//!
//! # Example
//!
//! ```rust,ignore
//! let router = LlmConditionalAgent::new("router", model)
//!     .instruction("Classify as 'technical', 'general', or 'creative'.
//!                   Respond with ONLY the category name.")
//!     .route("technical", Arc::new(tech_agent))
//!     .route("general", Arc::new(general_agent))
//!     .route("creative", Arc::new(creative_agent))
//!     .default_route(Arc::new(general_agent))
//!     .build()?;
//! ```

use adk_core::{
    Agent, Content, Event, EventStream, InvocationContext, Llm, LlmRequest, Part, Result,
};
use adk_skill::{SelectionPolicy, SkillIndex, load_skill_index};
use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

/// LLM-based intelligent conditional routing agent.
///
/// Uses an LLM to classify user input and route to the appropriate sub-agent
/// based on the classification result. Supports multi-way routing.
///
/// For rule-based routing (A/B testing, feature flags), use [`crate::ConditionalAgent`].
///
/// # Example
///
/// ```rust,ignore
/// let router = LlmConditionalAgent::new("router", model)
///     .instruction("Classify as 'technical', 'general', or 'creative'.")
///     .route("technical", tech_agent)
///     .route("general", general_agent.clone())
///     .route("creative", creative_agent)
///     .default_route(general_agent)
///     .build()?;
/// ```
pub struct LlmConditionalAgent {
    name: String,
    description: String,
    model: Arc<dyn Llm>,
    instruction: String,
    routes: HashMap<String, Arc<dyn Agent>>,
    default_agent: Option<Arc<dyn Agent>>,
    /// Cached list of all route agents (+ default) for tree discovery via `sub_agents()`.
    all_agents: Vec<Arc<dyn Agent>>,
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
}

pub struct LlmConditionalAgentBuilder {
    name: String,
    description: Option<String>,
    model: Arc<dyn Llm>,
    instruction: Option<String>,
    routes: HashMap<String, Arc<dyn Agent>>,
    default_agent: Option<Arc<dyn Agent>>,
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
}

impl LlmConditionalAgentBuilder {
    /// Create a new builder with the given name and model.
    pub fn new(name: impl Into<String>, model: Arc<dyn Llm>) -> Self {
        Self {
            name: name.into(),
            description: None,
            model,
            instruction: None,
            routes: HashMap::new(),
            default_agent: None,
            skills_index: None,
            skill_policy: SelectionPolicy::default(),
            max_skill_chars: 2000,
        }
    }

    /// Set a description for the agent.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set the classification instruction.
    ///
    /// The instruction should tell the LLM to classify the user's input
    /// and respond with ONLY the category name (matching a route key).
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Add a route mapping a classification label to an agent.
    ///
    /// When the LLM's response contains this label, execution transfers
    /// to the specified agent.
    pub fn route(mut self, label: impl Into<String>, agent: Arc<dyn Agent>) -> Self {
        self.routes.insert(label.into().to_lowercase(), agent);
        self
    }

    /// Set the default agent to use when no route matches.
    pub fn default_route(mut self, agent: Arc<dyn Agent>) -> Self {
        self.default_agent = Some(agent);
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

    /// Build the LlmConditionalAgent.
    pub fn build(self) -> Result<LlmConditionalAgent> {
        let instruction = self.instruction.ok_or_else(|| {
            adk_core::AdkError::Agent("Instruction is required for LlmConditionalAgent".to_string())
        })?;

        if self.routes.is_empty() {
            return Err(adk_core::AdkError::Agent(
                "At least one route is required for LlmConditionalAgent".to_string(),
            ));
        }

        // Collect all agents for sub_agents() tree discovery
        let mut all_agents: Vec<Arc<dyn Agent>> = self.routes.values().cloned().collect();
        if let Some(ref default) = self.default_agent {
            all_agents.push(default.clone());
        }

        Ok(LlmConditionalAgent {
            name: self.name,
            description: self.description.unwrap_or_default(),
            model: self.model,
            instruction,
            routes: self.routes,
            default_agent: self.default_agent,
            all_agents,
            skills_index: self.skills_index,
            skill_policy: self.skill_policy,
            max_skill_chars: self.max_skill_chars,
        })
    }
}

impl LlmConditionalAgent {
    /// Create a new builder for LlmConditionalAgent.
    pub fn builder(name: impl Into<String>, model: Arc<dyn Llm>) -> LlmConditionalAgentBuilder {
        LlmConditionalAgentBuilder::new(name, model)
    }
}

#[async_trait]
impl Agent for LlmConditionalAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.all_agents
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let run_ctx = super::skill_context::with_skill_injected_context(
            ctx,
            self.skills_index.as_ref(),
            &self.skill_policy,
            self.max_skill_chars,
        );
        let model = self.model.clone();
        let instruction = self.instruction.clone();
        let routes = self.routes.clone();
        let default_agent = self.default_agent.clone();
        let invocation_id = run_ctx.invocation_id().to_string();
        let agent_name = self.name.clone();

        let s = stream! {
            // Build classification request
            let user_content = run_ctx.user_content().clone();
            let user_text: String = user_content.parts.iter()
                .filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None })
                .collect::<Vec<_>>()
                .join(" ");

            let classification_prompt = format!(
                "{}\n\nUser input: {}",
                instruction,
                user_text
            );

            let request = LlmRequest {
                model: model.name().to_string(),
                contents: vec![Content::new("user").with_text(&classification_prompt)],
                tools: HashMap::new(),
                config: None,
            };

            // Call LLM for classification
            let mut response_stream = match model.generate_content(request, false).await {
                Ok(stream) => stream,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            // Collect classification response
            let mut classification = String::new();
            while let Some(chunk_result) = response_stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        if let Some(content) = chunk.content {
                            for part in content.parts {
                                if let Part::Text { text } = part {
                                    classification.push_str(&text);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }

            // Normalize classification
            let classification = classification.trim().to_lowercase();

            // Emit routing event
            let mut routing_event = Event::new(&invocation_id);
            routing_event.author = agent_name.clone();
            routing_event.llm_response.content = Some(
                Content::new("model").with_text(format!("[Routing to: {}]", classification))
            );
            yield Ok(routing_event);

            // Find matching route
            let target_agent = routes.iter()
                .find(|(label, _)| classification.contains(label.as_str()))
                .map(|(_, agent)| agent.clone())
                .or(default_agent);

            // Execute target agent
            if let Some(agent) = target_agent {
                match agent.run(run_ctx.clone()).await {
                    Ok(mut stream) => {
                        while let Some(event) = stream.next().await {
                            yield event;
                        }
                    }
                    Err(e) => {
                        yield Err(e);
                    }
                }
            } else {
                // No matching route and no default
                let mut error_event = Event::new(&invocation_id);
                error_event.author = agent_name;
                error_event.llm_response.content = Some(
                    Content::new("model").with_text(format!(
                        "No route found for classification '{}'. Available routes: {:?}",
                        classification,
                        routes.keys().collect::<Vec<_>>()
                    ))
                );
                yield Ok(error_event);
            }
        };

        Ok(Box::pin(s))
    }
}
