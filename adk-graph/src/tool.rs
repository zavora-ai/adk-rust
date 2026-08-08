//! Exposing a node or a whole graph as a tool an LLM can call.
//!
//! A graph is often the deterministic part of a system: fixed steps, checked
//! state, a durable checkpoint. An `LlmAgent` is the part that decides. Handing
//! the graph to the model as a tool lets the model choose *when* the
//! deterministic part runs without deciding *how* it runs.
//!
//! adk-python exposes this as `NodeTool`; adk-go routes agent-as-tool through its
//! dynamic sub-scheduler.
//!
//! # Schemas
//!
//! [`Node`](crate::node::Node) declares no input schema, so a tool over one node
//! accepts any object unless a schema is supplied with
//! [`with_parameters_schema`](NodeTool::with_parameters_schema). A tool over a
//! whole graph derives its parameters from the graph's declared state channels,
//! which are known.
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_graph::edge::{END, START};
//! use adk_graph::graph::StateGraph;
//! use adk_graph::node::NodeOutput;
//! use adk_graph::tool::NodeTool;
//! use serde_json::json;
//! use std::sync::Arc;
//!
//! # fn build() -> Result<(), Box<dyn std::error::Error>> {
//! let graph = Arc::new(
//!     StateGraph::with_channels(&["city", "forecast"])
//!         .add_node_fn("lookup", |ctx| async move {
//!             let city = ctx.get("city").and_then(|v| v.as_str()).unwrap_or("");
//!             Ok(NodeOutput::new().with_update("forecast", json!(format!("sunny in {city}"))))
//!         })
//!         .add_edge(START, "lookup")
//!         .add_edge("lookup", END)
//!         .compile()?,
//! );
//!
//! let tool = NodeTool::for_graph(graph).with_description("Looks up a forecast.");
//! // builder.tool(Arc::new(tool))
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use adk_core::{AdkError, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::GraphError;
use crate::graph::CompiledGraph;
use crate::interrupt::GraphInterruptPayload;
use crate::node::ExecutionConfig;

/// What the tool invokes.
enum Target {
    /// One node, executed on its own.
    Node(String),
    /// The whole graph, from its entry points.
    Graph,
}

/// A [`Tool`] that runs a graph node, or a whole graph.
pub struct NodeTool {
    graph: Arc<CompiledGraph>,
    target: Target,
    name: String,
    description: String,
    parameters_schema: Option<Value>,
}

impl NodeTool {
    /// A tool that executes one node.
    ///
    /// The node runs alone: no edges are followed and no checkpoint is written.
    pub fn for_node(graph: Arc<CompiledGraph>, node: impl Into<String>) -> Self {
        let node = node.into();
        Self {
            graph,
            name: node.clone(),
            description: format!("Runs the '{node}' graph node."),
            target: Target::Node(node),
            parameters_schema: None,
        }
    }

    /// A tool that executes the whole graph.
    pub fn for_graph(graph: Arc<CompiledGraph>) -> Self {
        Self {
            graph,
            target: Target::Graph,
            name: "run_graph".to_string(),
            description: "Runs a graph workflow to completion.".to_string(),
            parameters_schema: None,
        }
    }

    /// Set the name the model sees.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the description the model sees.
    ///
    /// The default names the node or graph, which tells a model nothing about
    /// when to call it. A real deployment should set this.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Set the parameter schema explicitly.
    pub fn with_parameters_schema(mut self, schema: Value) -> Self {
        self.parameters_schema = Some(schema);
        self
    }

    /// Parameters derived from the graph's declared state channels.
    ///
    /// Every channel is optional and untyped: a channel declares a reducer, not a
    /// type, so nothing stronger is available.
    fn derived_schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        for channel in self.graph.state_channels() {
            properties.insert(channel, json!({ "description": "A graph state channel." }));
        }
        json!({ "type": "object", "properties": properties })
    }
}

#[async_trait]
impl Tool for NodeTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        if let Some(schema) = &self.parameters_schema {
            return Some(schema.clone());
        }
        match self.target {
            // A node declares no schema, so anything is accepted.
            Target::Node(_) => Some(json!({ "type": "object" })),
            Target::Graph => Some(self.derived_schema()),
        }
    }

    /// A graph may pause for approval, and a pause is not a value.
    ///
    /// Reporting the tool as long-running routes that pause through the existing
    /// tool-confirmation path in `adk-agent` rather than inventing a second
    /// mechanism for it.
    fn is_long_running(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let mut input = crate::state::State::new();
        if let Value::Object(map) = args {
            for (key, value) in map {
                input.insert(key, value);
            }
        }

        let config = ExecutionConfig::new(ctx.session_id());

        match &self.target {
            Target::Node(node) => {
                let node_impl = self
                    .graph
                    .node(node)
                    .ok_or_else(|| AdkError::tool(format!("no graph node named '{node}'")))?;
                let node_ctx = crate::node::NodeContext::new(input, config, 0);
                let output = node_impl
                    .execute(&node_ctx)
                    .await
                    .map_err(|error| AdkError::tool(error.to_string()))?;
                if let Some(interrupt) = output.interrupt {
                    return Ok(interrupt_value(&interrupt, ctx.session_id(), ""));
                }
                Ok(Value::Object(output.updates.into_iter().collect()))
            }
            Target::Graph => match self.graph.invoke(input, config).await {
                Ok(state) => Ok(Value::Object(state.into_iter().collect())),
                Err(GraphError::Interrupted(interrupted)) => Ok(interrupt_value(
                    &interrupted.interrupt,
                    &interrupted.thread_id,
                    &interrupted.checkpoint_id,
                )),
                Err(error) => Err(AdkError::tool(error.to_string())),
            },
        }
    }
}

/// The value returned when the wrapped node or graph pauses.
fn interrupt_value(
    interrupt: &crate::interrupt::Interrupt,
    thread_id: &str,
    checkpoint_id: &str,
) -> Value {
    let payload = GraphInterruptPayload::new(interrupt, thread_id, checkpoint_id);
    json!({
        "status": "interrupted",
        "interrupt": serde_json::to_value(&payload).unwrap_or(Value::Null),
    })
}
