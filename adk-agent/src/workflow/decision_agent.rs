//! `DecisionAgent`: routes to a sub-agent by a typed decision a model makes with
//! structured output.
//!
//! Where [`LlmConditionalAgent`](crate::LlmConditionalAgent) reads a free-text label from the
//! model, `DecisionAgent` asks for a [`Decision`](adk_core::Decision) through
//! [`Decider`]: the reply is constrained to one of the route names by the provider's
//! structured outputs, carries the model's rationale and confidence, and is recorded in the
//! session. A decision below [`min_confidence`](DecisionAgentBuilder::min_confidence) goes to
//! the fallback route.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{
    AdkError, Agent, Choice, ChoiceOption, Content, Decider, Event, EventStream, InvocationContext,
    Llm, ModelCallScope, Part, Result, WorkflowPosition,
};
use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;

/// Routes each turn to one sub-agent by a model's typed decision.
///
/// Each run checks the kill switch, decides among the routes with a [`Decider`] metered
/// against the run's budget and spend ledger (`ModelCallScope::for_run`), yields a routing
/// event that names the route, rationale and confidence (and writes them to
/// [`output_key`](DecisionAgentBuilder::output_key) when set), then runs the chosen
/// sub-agent. A run that resumes a held call returns to the route that holds it without
/// deciding again.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_agent::DecisionAgent;
/// use adk_core::{Agent, Llm};
///
/// fn triage(
///     model: Arc<dyn Llm>,
///     billing: Arc<dyn Agent>,
///     technical: Arc<dyn Agent>,
///     general: Arc<dyn Agent>,
/// ) -> adk_core::Result<DecisionAgent> {
///     DecisionAgent::builder("triage", model)
///         .instruction("Route the support request to the team that should answer it.")
///         .route("billing", "Invoices, refunds and payment problems", billing)
///         .route("technical", "Errors, outages and how-to questions", technical)
///         .fallback(general)
///         .min_confidence(0.6)
///         .output_key("triage")
///         .build()
/// }
/// ```
pub struct DecisionAgent {
    name: String,
    description: String,
    model: Arc<dyn Llm>,
    instruction: Option<String>,
    options: Vec<ChoiceOption>,
    routes: HashMap<String, Arc<dyn Agent>>,
    fallback: Option<Arc<dyn Agent>>,
    min_confidence: f64,
    output_key: Option<String>,
    all_agents: Vec<Arc<dyn Agent>>,
}

impl std::fmt::Debug for DecisionAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionAgent")
            .field("name", &self.name)
            .field("options", &self.options)
            .field("min_confidence", &self.min_confidence)
            .finish_non_exhaustive()
    }
}

/// Builds a [`DecisionAgent`].
pub struct DecisionAgentBuilder {
    name: String,
    description: Option<String>,
    model: Arc<dyn Llm>,
    instruction: Option<String>,
    options: Vec<ChoiceOption>,
    routes: HashMap<String, Arc<dyn Agent>>,
    fallback: Option<Arc<dyn Agent>>,
    min_confidence: f64,
    output_key: Option<String>,
    error: Option<AdkError>,
}

impl DecisionAgentBuilder {
    /// A builder for agent `name` deciding with `model`.
    pub fn new(name: impl Into<String>, model: Arc<dyn Llm>) -> Self {
        Self {
            name: name.into(),
            description: None,
            model,
            instruction: None,
            options: Vec::new(),
            routes: HashMap::new(),
            fallback: None,
            min_confidence: 0.0,
            output_key: None,
            error: None,
        }
    }

    /// The agent description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// What to decide, shown to the model before the routes.
    #[must_use]
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// A route: the option `name`, what it is for, and the agent that handles it.
    #[must_use]
    pub fn route(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        agent: Arc<dyn Agent>,
    ) -> Self {
        let name = name.into();
        self.options.push(ChoiceOption::new(name.clone(), description));
        self.routes.insert(name, agent);
        self
    }

    /// One route per option of `T`, each to the agent `agent_for` gives its value.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let router = DecisionAgent::builder("triage", model)
    ///     .routes::<Route>(|route| match route {
    ///         Route::Billing => billing.clone(),
    ///         Route::Technical => technical.clone(),
    ///     })
    ///     .build()?;
    /// ```
    #[must_use]
    pub fn routes<T: Choice>(mut self, agent_for: impl Fn(T) -> Arc<dyn Agent>) -> Self {
        for option in T::options() {
            match T::from_option(&option.name) {
                Some(value) => {
                    let agent = agent_for(value);
                    self.routes.insert(option.name.clone(), agent);
                    self.options.push(option);
                }
                None => {
                    self.error.get_or_insert_with(|| {
                        AdkError::config(format!(
                            "option `{}` of the choice type does not map to a value",
                            option.name
                        ))
                    });
                }
            }
        }
        self
    }

    /// The agent that handles a decision below [`min_confidence`](Self::min_confidence).
    #[must_use]
    pub fn fallback(mut self, agent: Arc<dyn Agent>) -> Self {
        self.fallback = Some(agent);
        self
    }

    /// The least confidence, from 0.0 to 1.0, at which a decision is followed; below it the
    /// fallback route runs, or the run fails with `decision.low_confidence` without one.
    /// Default 0.0, which follows every decision.
    #[must_use]
    pub fn min_confidence(mut self, confidence: f64) -> Self {
        self.min_confidence = confidence;
        self
    }

    /// The session-state key the decision is written to:
    /// `{option, rationale, confidence, followed, route}`.
    #[must_use]
    pub fn output_key(mut self, key: impl Into<String>) -> Self {
        self.output_key = Some(key.into());
        self
    }

    /// Builds the agent.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when there is no route, two routes share a name, a
    /// [`routes`](Self::routes) option maps to no value, or `min_confidence` is outside 0.0 to
    /// 1.0.
    pub fn build(self) -> Result<DecisionAgent> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.options.is_empty() {
            return Err(AdkError::config(format!(
                "DecisionAgent `{}` needs at least one route",
                self.name
            )));
        }
        if self.routes.len() != self.options.len() {
            return Err(AdkError::config(format!(
                "DecisionAgent `{}` names a route twice; give each route a distinct name",
                self.name
            )));
        }
        if !(0.0..=1.0).contains(&self.min_confidence) {
            return Err(AdkError::config(format!(
                "DecisionAgent `{}`: min_confidence {} is outside 0.0 to 1.0",
                self.name, self.min_confidence
            )));
        }
        let mut all_agents: Vec<Arc<dyn Agent>> = self
            .options
            .iter()
            .filter_map(|option| self.routes.get(&option.name).cloned())
            .collect();
        all_agents.extend(self.fallback.clone());
        Ok(DecisionAgent {
            name: self.name,
            description: self.description.unwrap_or_default(),
            model: self.model,
            instruction: self.instruction,
            options: self.options,
            routes: self.routes,
            fallback: self.fallback,
            min_confidence: self.min_confidence,
            output_key: self.output_key,
            all_agents,
        })
    }
}

impl DecisionAgent {
    /// A builder for agent `name` deciding with `model`.
    pub fn builder(name: impl Into<String>, model: Arc<dyn Llm>) -> DecisionAgentBuilder {
        DecisionAgentBuilder::new(name, model)
    }

    /// The routes the model decides among, in the order it sees them.
    pub fn options(&self) -> &[ChoiceOption] {
        &self.options
    }
}

#[async_trait]
impl Agent for DecisionAgent {
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
        // On cross-turn resumption the runner restarts from this root, so the next turn is
        // decided again rather than resuming the route that answered last.
        false
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        // A call a person asked for needs an agent that holds it for their decision itself
        // (spec 3.3 §7.4 rule 5).
        if let Some(refused) = crate::requested_call::refused_by_root(
            ctx.as_ref(),
            &self.name,
            crate::requested_call::RUNS_OTHER_AGENTS,
        ) {
            return Ok(refused);
        }
        // A run that resumes a hold returns to the route that holds it: the resuming run's
        // input is not the input that chose the route.
        let resuming =
            ctx.session().workflow_resume_positions(&self.name, ctx.branch()).into_iter().find_map(
                |position| {
                    self.all_agents.iter().find(|agent| agent.name() == position.sub_agent).cloned()
                },
            );
        let (agent, routing) = match resuming {
            Some(agent) => (agent, None),
            None => {
                ctx.run_config().check_governance()?;
                let input: String =
                    ctx.user_content().parts.iter().filter_map(Part::text).collect();
                let scope = ModelCallScope::for_run(ctx.run_config(), ctx.app_name(), &self.name);
                let mut decider = Decider::new(self.model.clone()).with_scope(scope);
                if let Some(instruction) = &self.instruction {
                    decider = decider.with_instruction(instruction.clone());
                }
                let decision = decider.decide_among(&self.options, &input).await?;
                let followed = decision.confidence >= self.min_confidence;
                let agent = match (followed, &self.fallback) {
                    (true, _) => self.routes.get(&decision.option).cloned(),
                    (false, Some(fallback)) => Some(fallback.clone()),
                    (false, None) => None,
                }
                .ok_or_else(|| {
                    AdkError::new(
                        adk_core::ErrorComponent::Agent,
                        adk_core::ErrorCategory::Unavailable,
                        "decision.low_confidence",
                        format!(
                            "DecisionAgent `{}` decided `{}` with confidence {:.2}, below its \
                             min_confidence {:.2}, and has no fallback route",
                            self.name, decision.option, decision.confidence, self.min_confidence
                        ),
                    )
                })?;
                tracing::info!(
                    agent.name = %self.name,
                    decision.option = %decision.option,
                    decision.confidence = decision.confidence,
                    decision.followed = followed,
                    route = %agent.name(),
                    "decision routed"
                );
                let mut event = Event::new(ctx.invocation_id());
                event.author = self.name.clone();
                event.branch = ctx.branch().to_string();
                let note = if followed {
                    String::new()
                } else {
                    format!(" (below {:.2}; fallback {})", self.min_confidence, agent.name())
                };
                event.llm_response.content = Some(Content::new("model").with_text(format!(
                    "[Decision: {} ({:.2}){note}] {}",
                    decision.option, decision.confidence, decision.rationale
                )));
                if let Some(key) = &self.output_key {
                    event.actions.state_delta.insert(
                        key.clone(),
                        json!({
                            "option": decision.option,
                            "rationale": decision.rationale,
                            "confidence": decision.confidence,
                            "followed": followed,
                            "route": agent.name(),
                        }),
                    );
                }
                (agent, Some(event))
            }
        };
        let name = self.name.clone();
        let s = stream! {
            if let Some(routing) = routing {
                yield Ok(routing);
            }
            let mut inner = match agent.run(ctx.clone()).await {
                Ok(inner) => inner,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };
            while let Some(result) = inner.next().await {
                yield result.map(|mut event| {
                    if event.actions.tool_confirmation.is_some() {
                        event
                            .actions
                            .workflow_positions
                            .insert(name.clone(), WorkflowPosition::new(0, agent.name()));
                    }
                    event
                });
            }
        };
        Ok(Box::pin(s))
    }
}
