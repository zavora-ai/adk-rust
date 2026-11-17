//! [`DecisionNode`]: a graph router node whose route a model decides, with structured output.
//!
//! The node reads its input from the state, asks a [`Decider`] to pick one of its options,
//! and writes the option's name to its output key, where a conditional edge reads it with
//! [`Router::by_field`](crate::edge::Router::by_field). It writes the rationale and the
//! confidence beside it under `<output_key>_decision`.

use std::sync::Arc;

use adk_core::{Choice, ChoiceOption, Decider, Llm, ModelCallScope};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{GraphError, Result};
use crate::node::{Node, NodeContext, NodeOutput};

/// A graph node that routes by a model's typed decision.
///
/// The decision's call runs in the graph run's [`ModelCallScope`] when the graph runs with a
/// `RunConfig` (as a graph agent under a runner, or
/// [`invoke_with_run_config`](crate::CompiledGraph::invoke_with_run_config)), so it counts
/// against the run's budget and spend ledger; otherwise in the scope current at the call. A
/// decision below [`min_confidence`](Self::min_confidence) routes to the
/// [`fallback`](Self::fallback) option.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_graph::node::FunctionNode;
/// use adk_graph::{DecisionNode, END, NodeOutput, Router, START, StateGraph, StateSchema};
///
/// fn triage_graph(
///     model: Arc<dyn adk_core::Llm>,
/// ) -> adk_graph::Result<adk_graph::CompiledGraph> {
///     let triage = DecisionNode::new("triage", model)
///         .instruction("Route the support ticket.")
///         .option("billing", "Invoices, refunds and payment problems")
///         .option("technical", "Errors, outages and how-to questions")
///         .input_key("ticket")
///         .output_key("route");
///     StateGraph::new(StateSchema::simple(&["ticket", "route"]))
///         .add_node(triage)
///         .add_node(FunctionNode::new("billing", |_| async { Ok(NodeOutput::new()) }))
///         .add_node(FunctionNode::new("technical", |_| async { Ok(NodeOutput::new()) }))
///         .add_edge(START, "triage")
///         .add_conditional_edges(
///             "triage",
///             Router::by_field("route"),
///             [("billing", "billing"), ("technical", "technical")],
///         )
///         .add_edge("billing", END)
///         .add_edge("technical", END)
///         .compile()
/// }
/// ```
#[derive(Clone)]
pub struct DecisionNode {
    name: String,
    model: Arc<dyn Llm>,
    instruction: Option<String>,
    options: Vec<ChoiceOption>,
    input_key: String,
    output_key: String,
    min_confidence: f64,
    fallback: Option<String>,
}

impl std::fmt::Debug for DecisionNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionNode")
            .field("name", &self.name)
            .field("options", &self.options)
            .field("input_key", &self.input_key)
            .field("output_key", &self.output_key)
            .finish_non_exhaustive()
    }
}

impl DecisionNode {
    /// A node named `name` deciding with `model`; it reads state key `input` and writes the
    /// option's name to `route` until configured otherwise.
    pub fn new(name: impl Into<String>, model: Arc<dyn Llm>) -> Self {
        Self {
            name: name.into(),
            model,
            instruction: None,
            options: Vec::new(),
            input_key: "input".to_string(),
            output_key: "route".to_string(),
            min_confidence: 0.0,
            fallback: None,
        }
    }

    /// A node whose options are those of `T`.
    pub fn for_choice<T: Choice>(name: impl Into<String>, model: Arc<dyn Llm>) -> Self {
        let mut node = Self::new(name, model);
        node.options = T::options();
        node
    }

    /// What to decide, shown to the model before the options.
    #[must_use]
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// An option: the route `name` and what it is for.
    #[must_use]
    pub fn option(mut self, name: impl Into<String>, description: impl Into<String>) -> Self {
        self.options.push(ChoiceOption::new(name, description));
        self
    }

    /// The state key the decision reads: a string as is, any other value as JSON.
    #[must_use]
    pub fn input_key(mut self, key: impl Into<String>) -> Self {
        self.input_key = key.into();
        self
    }

    /// The state key the option's name is written to; the rationale and confidence go to
    /// `<key>_decision`.
    #[must_use]
    pub fn output_key(mut self, key: impl Into<String>) -> Self {
        self.output_key = key.into();
        self
    }

    /// The least confidence, from 0.0 to 1.0, at which the decision is followed; below it the
    /// node routes to [`fallback`](Self::fallback), or fails without one. Default 0.0.
    #[must_use]
    pub fn min_confidence(mut self, confidence: f64) -> Self {
        self.min_confidence = confidence;
        self
    }

    /// The route written when the decision is below `min_confidence`.
    #[must_use]
    pub fn fallback(mut self, route: impl Into<String>) -> Self {
        self.fallback = Some(route.into());
        self
    }

    fn failed(&self, message: String) -> GraphError {
        GraphError::NodeExecutionFailed { node: self.name.clone(), message }
    }
}

#[async_trait]
impl Node for DecisionNode {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Routes by a model's typed decision"
    }

    fn kind(&self) -> crate::shape::NodeKind {
        crate::shape::NodeKind::ModelStep
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput> {
        if !(0.0..=1.0).contains(&self.min_confidence) {
            return Err(self
                .failed(format!("min_confidence {} is outside 0.0 to 1.0", self.min_confidence)));
        }
        let input = match ctx.state.get(&self.input_key) {
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
            None => {
                return Err(self.failed(format!(
                    "state key `{}`, the decision's input, is not set",
                    self.input_key
                )));
            }
        };
        let mut decider = Decider::new(self.model.clone());
        if let Some(config) = ctx.run_config() {
            let app = ctx.config.parent_context.as_ref().map_or_else(
                || crate::node::STANDALONE_APP_NAME.to_string(),
                |parent| parent.app_name().to_string(),
            );
            decider = decider.with_scope(ModelCallScope::for_run(&config, &app, &self.name));
        }
        if let Some(instruction) = &self.instruction {
            decider = decider.with_instruction(instruction.clone());
        }
        let decision = decider.decide_among(&self.options, &input).await.map_err(|error| {
            GraphError::AgentFailed { node: self.name.clone(), source: Box::new(error) }
        })?;
        let followed = decision.confidence >= self.min_confidence;
        let route = match (followed, &self.fallback) {
            (true, _) => decision.option.clone(),
            (false, Some(fallback)) => fallback.clone(),
            (false, None) => {
                return Err(self.failed(format!(
                    "decided `{}` with confidence {:.2}, below min_confidence {:.2}, and has no \
                     fallback route",
                    decision.option, decision.confidence, self.min_confidence
                )));
            }
        };
        tracing::info!(
            node.name = %self.name,
            decision.option = %decision.option,
            decision.confidence = decision.confidence,
            decision.followed = followed,
            route = %route,
            "graph decision routed"
        );
        Ok(NodeOutput::new().with_update(&self.output_key, json!(route)).with_update(
            &format!("{}_decision", self.output_key),
            json!({
                "option": decision.option,
                "rationale": decision.rationale,
                "confidence": decision.confidence,
                "followed": followed,
            }),
        ))
    }
}
