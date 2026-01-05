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
//! let router = LlmConditionalAgent::new("router", model)
//!     .instruction("Classify as 'technical' or 'general'")
//!     .route("technical", tech_agent)
//!     .route("general", general_agent)
//!     .build()?;
//! ```
//!
//! See [`crate::workflow::LlmConditionalAgent`] for details.

use adk_core::{
    AfterAgentCallback, Agent, BeforeAgentCallback, EventStream, InvocationContext, Result,
};
use async_trait::async_trait;
use std::sync::Arc;

type ConditionFn = Box<dyn Fn(&dyn InvocationContext) -> bool + Send + Sync>;

/// Rule-based conditional routing agent.
///
/// Executes one of two sub-agents based on a synchronous condition function.
/// For LLM-based intelligent routing, use [`LlmConditionalAgent`] instead.
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
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
}

impl ConditionalAgent {
    pub fn new<F>(name: impl Into<String>, condition: F, if_agent: Arc<dyn Agent>) -> Self
    where
        F: Fn(&dyn InvocationContext) -> bool + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            description: String::new(),
            condition: Box::new(condition),
            if_agent,
            else_agent: None,
            before_callbacks: Vec::new(),
            after_callbacks: Vec::new(),
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn with_else(mut self, else_agent: Arc<dyn Agent>) -> Self {
        self.else_agent = Some(else_agent);
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
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let agent = if (self.condition)(ctx.as_ref()) {
            self.if_agent.clone()
        } else if let Some(else_agent) = &self.else_agent {
            else_agent.clone()
        } else {
            return Ok(Box::pin(futures::stream::empty()));
        };

        agent.run(ctx).await
    }
}
