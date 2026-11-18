//! Compiles a [`WorkflowDefinition`] into a graph.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use adk_action::{ActionNodeConfig, ExpressionMode, get_nested_value, interpolate_variables};
use adk_core::{Agent, Llm, Tool};
use serde_json::{Map, Value, json};

use super::WorkflowError;
use super::types::{
    CheckpointStore, Condition, EdgeDefinition, EdgeSpec, LlmDefinition, NodeDefinition,
    ReducerKind, WorkflowDefinition,
};
use super::validate::{END_ID, START_ID};
use crate::action::ActionNodeExecutor;
use crate::agent::GraphAgent;
use crate::checkpoint::{Checkpointer, MemoryCheckpointer};
use crate::edge::{Edge, EdgeTarget};
use crate::graph::{CompiledGraph, StateGraph};
use crate::node::{AgentNode, FunctionNode, Node, NodeContext, NodeOutput};
use crate::retry::RetryPolicy;
use crate::state::{Reducer, State, StateSchema};

/// A function node a workflow names with `function:`.
pub type FunctionNodeFn = Arc<
    dyn Fn(NodeContext) -> Pin<Box<dyn Future<Output = crate::Result<NodeOutput>> + Send>>
        + Send
        + Sync,
>;

/// Resolves a `provider/model` reference, such as `anthropic/claude-haiku-5-5`, to a model.
pub type ModelResolver = Arc<dyn Fn(&str) -> adk_core::Result<Arc<dyn Llm>> + Send + Sync>;

/// What a workflow's names refer to: agents, models, tools and functions, registered by name.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_core::{Agent, Llm};
/// use adk_graph::definition::{WorkflowDefinition, WorkflowRegistry};
///
/// # async fn run(researcher: Arc<dyn Agent>, haiku: Arc<dyn Llm>) -> Result<(), Box<dyn std::error::Error>> {
/// let registry = WorkflowRegistry::new()
///     .agent(researcher)
///     .model("anthropic/claude-haiku-5-5", haiku);
/// let graph = WorkflowDefinition::from_file("workflow.yaml")?.build(&registry).await?;
/// # let _ = graph;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Default)]
pub struct WorkflowRegistry {
    agents: BTreeMap<String, Arc<dyn Agent>>,
    models: BTreeMap<String, Arc<dyn Llm>>,
    tools: BTreeMap<String, Arc<dyn Tool>>,
    functions: BTreeMap<String, FunctionNodeFn>,
    model_resolver: Option<ModelResolver>,
    checkpointer: Option<Arc<dyn Checkpointer>>,
}

impl std::fmt::Debug for WorkflowRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowRegistry")
            .field("agents", &self.agents.keys().collect::<Vec<_>>())
            .field("models", &self.models.keys().collect::<Vec<_>>())
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .field("functions", &self.functions.keys().collect::<Vec<_>>())
            .field("model_resolver", &self.model_resolver.is_some())
            .field("checkpointer", &self.checkpointer.is_some())
            .finish()
    }
}

impl WorkflowRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `agent` under its name, for `agent:` nodes.
    #[must_use]
    pub fn agent(mut self, agent: Arc<dyn Agent>) -> Self {
        self.agents.insert(agent.name().to_string(), agent);
        self
    }

    /// Registers `model` under `name`, for `llm.model` references and `models` aliases.
    #[must_use]
    pub fn model(mut self, name: impl Into<String>, model: Arc<dyn Llm>) -> Self {
        self.models.insert(name.into(), model);
        self
    }

    /// Resolves model references no registered model answers, such as
    /// `anthropic/claude-haiku-5-5`.
    #[must_use]
    pub fn model_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn(&str) -> adk_core::Result<Arc<dyn Llm>> + Send + Sync + 'static,
    {
        self.model_resolver = Some(Arc::new(resolver));
        self
    }

    /// Registers `tool` under its name, for `llm.tools`.
    #[must_use]
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.insert(tool.name().to_string(), tool);
        self
    }

    /// Registers a function node under `name`, for `function:` nodes.
    #[must_use]
    pub fn function<F, Fut>(mut self, name: impl Into<String>, function: F) -> Self
    where
        F: Fn(NodeContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = crate::Result<NodeOutput>> + Send + 'static,
    {
        self.functions.insert(name.into(), Arc::new(move |ctx| Box::pin(function(ctx))));
        self
    }

    /// Keeps checkpoints in `checkpointer`, in place of the store the workflow names.
    #[must_use]
    pub fn checkpointer(mut self, checkpointer: Arc<dyn Checkpointer>) -> Self {
        self.checkpointer = Some(checkpointer);
        self
    }

    fn resolve_model(
        &self,
        workflow: &WorkflowDefinition,
        name: &str,
    ) -> Result<Arc<dyn Llm>, String> {
        let reference = workflow.models.get(name).map_or(name, String::as_str);
        if let Some(model) = self.models.get(reference).or_else(|| self.models.get(name)) {
            return Ok(Arc::clone(model));
        }
        match &self.model_resolver {
            Some(resolve) => resolve(reference)
                .map_err(|error| format!("cannot resolve model '{reference}': {}", error.message)),
            None => Err(format!(
                "no model '{reference}' is registered and the registry has no model resolver; \
                 register it with WorkflowRegistry::model or set a model_resolver"
            )),
        }
    }
}

fn graph_id(id: &str) -> &str {
    match id {
        START_ID => crate::START,
        END_ID => crate::END,
        other => other,
    }
}

impl WorkflowDefinition {
    /// Compiles the workflow into a graph with its checkpointer, interrupts, recursion limit
    /// and retries.
    ///
    /// # Errors
    ///
    /// Returns every validation issue, and every name the registry cannot resolve (an agent,
    /// model, tool or function), each with its path and location.
    pub async fn build(&self, registry: &WorkflowRegistry) -> Result<CompiledGraph, WorkflowError> {
        self.validate()?;
        let mut issues: Vec<(String, String)> = Vec::new();
        let mut graph = StateGraph::new(self.state_schema());

        for (id, node) in &self.nodes {
            match self.node(id, node, registry) {
                Ok(built) => {
                    graph.nodes.insert(id.clone(), built);
                }
                Err(issue) => issues.push(issue),
            }
        }
        if !issues.is_empty() {
            return Err(self.source.error(issues));
        }

        for edge in &self.edges {
            graph = match edge {
                EdgeDefinition::Chain(ids) => ids.windows(2).fold(graph, |graph, pair| {
                    graph.add_edge(graph_id(&pair[0]), graph_id(&pair[1]))
                }),
                EdgeDefinition::Edge(spec) => add_spec(graph, spec),
            };
        }

        let mut compiled = graph
            .compile()
            .map_err(|error| self.source.error(vec![(String::new(), error.to_string())]))?;

        let checkpointer = match &registry.checkpointer {
            Some(checkpointer) => Some(Arc::clone(checkpointer)),
            None => self.store().await?,
        };
        if let Some(checkpointer) = checkpointer {
            compiled = compiled.with_checkpointer_arc(checkpointer);
        }
        if !self.interrupt_before.is_empty() {
            let nodes: Vec<&str> = self.interrupt_before.iter().map(String::as_str).collect();
            compiled = compiled.with_interrupt_before(&nodes);
        }
        if !self.interrupt_after.is_empty() {
            let nodes: Vec<&str> = self.interrupt_after.iter().map(String::as_str).collect();
            compiled = compiled.with_interrupt_after(&nodes);
        }
        if let Some(limit) = self.recursion_limit {
            compiled = compiled.with_recursion_limit(limit);
        }
        for (id, node) in &self.nodes {
            if let Some(attempts) = node.max_attempts.filter(|attempts| *attempts > 1) {
                compiled = compiled.with_node_retry(id, RetryPolicy::new(attempts));
            }
        }
        Ok(compiled)
    }

    /// Compiles the workflow into a [`GraphAgent`] named after it, which a `Runner` can run;
    /// each turn of a paused thread resumes it with the turn's text.
    ///
    /// # Errors
    ///
    /// Returns the issues [`build`](Self::build) returns.
    pub async fn build_agent(
        &self,
        registry: &WorkflowRegistry,
    ) -> Result<GraphAgent, WorkflowError> {
        Ok(GraphAgent::from_graph(&self.name, self.build(registry).await?))
    }

    fn state_schema(&self) -> StateSchema {
        let mut builder = StateSchema::builder().list_channel("messages");
        for (name, channel) in &self.state {
            let reducer = match channel.reducer {
                ReducerKind::Overwrite => Reducer::Overwrite,
                ReducerKind::Append => Reducer::Append,
                ReducerKind::Sum => Reducer::Sum,
            };
            builder = builder.channel_with_reducer(name, reducer);
        }
        let mut schema = builder.build();
        for (name, channel) in &self.state {
            if let Some(default) = &channel.default
                && let Some(declared) = schema.channels.get_mut(name)
            {
                declared.default = Some(default.clone());
            }
        }
        schema
    }

    async fn store(&self) -> Result<Option<Arc<dyn Checkpointer>>, WorkflowError> {
        match self.checkpoint.store {
            CheckpointStore::Memory => Ok(Some(Arc::new(MemoryCheckpointer::new()))),
            CheckpointStore::None => Ok(None),
            CheckpointStore::Sqlite => self.sqlite_store().await,
        }
    }

    #[cfg(feature = "sqlite")]
    async fn sqlite_store(&self) -> Result<Option<Arc<dyn Checkpointer>>, WorkflowError> {
        let path = self.checkpoint.path.as_deref().unwrap_or_default();
        let path = match &self.source.base_dir {
            Some(dir) if !std::path::Path::new(path).is_absolute() => dir.join(path),
            _ => std::path::PathBuf::from(path),
        };
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let checkpointer =
            crate::checkpoint::SqliteCheckpointer::new(&url).await.map_err(|error| {
                self.source.error(vec![(
                    "checkpoint.path".into(),
                    format!("cannot open the SQLite checkpoint database: {error}"),
                )])
            })?;
        Ok(Some(Arc::new(checkpointer)))
    }

    #[cfg(not(feature = "sqlite"))]
    async fn sqlite_store(&self) -> Result<Option<Arc<dyn Checkpointer>>, WorkflowError> {
        Err(self.source.error(vec![(
            "checkpoint.store".into(),
            "the `sqlite` store needs adk-graph's `sqlite` feature; rebuild with it, or register \
             a checkpointer with WorkflowRegistry::checkpointer"
                .into(),
        )]))
    }

    fn node(
        &self,
        id: &str,
        node: &NodeDefinition,
        registry: &WorkflowRegistry,
    ) -> Result<Arc<dyn Node>, (String, String)> {
        let path = format!("nodes.{id}");
        if let Some(name) = &node.agent {
            let agent = registry.agents.get(name).cloned().ok_or_else(|| {
                let known: Vec<&str> = registry.agents.keys().map(String::as_str).collect();
                (
                    format!("{path}.agent"),
                    format!(
                        "no agent named '{name}' is registered; registered agents: {}",
                        if known.is_empty() { "none".to_string() } else { known.join(", ") }
                    ),
                )
            })?;
            return Ok(Arc::new(agent_node(id, agent, node)));
        }
        if let Some(llm) = &node.llm {
            let agent = self
                .llm_agent(id, llm, registry)
                .map_err(|(field, message)| (format!("{path}.llm{field}"), message))?;
            return Ok(Arc::new(agent_node(id, agent, node)));
        }
        if let Some(name) = &node.function {
            let function = registry.functions.get(name).cloned().ok_or_else(|| {
                (format!("{path}.function"), format!("no function named '{name}' is registered"))
            })?;
            return Ok(Arc::new(FunctionNode::new(id, move |ctx| function(ctx))));
        }
        if let Some(values) = &node.set {
            let values = Arc::new(values.clone());
            return Ok(Arc::new(FunctionNode::new(id, move |ctx| {
                let values = Arc::clone(&values);
                async move {
                    let mut output = NodeOutput::new();
                    for (key, value) in values.iter() {
                        output = output.with_update(key, interpolate_value(value, &ctx.state));
                    }
                    Ok(output)
                }
            })));
        }
        if let Some(action) = &node.action {
            let config = action_config(id, action, node.output_key.as_deref())
                .map_err(|message| (format!("{path}.action"), message))?;
            return Ok(Arc::new(ActionNodeExecutor::new(config)));
        }
        if let Some(interrupt) = &node.interrupt {
            let message = interrupt.message.clone();
            let schema = interrupt.response_schema.clone();
            let key = node.output_key.clone().unwrap_or_else(|| id.to_string());
            return Ok(Arc::new(FunctionNode::new(id, move |ctx| {
                let (message, schema, key) = (message.clone(), schema.clone(), key.clone());
                async move {
                    if let Some(answer) = ctx.resume_value() {
                        return Ok(NodeOutput::new().with_update(&key, answer.clone()));
                    }
                    let output =
                        NodeOutput::interrupt(&interpolate_variables(&message, &ctx.state));
                    Ok(match schema {
                        Some(schema) => output.with_response_schema(schema),
                        None => output,
                    })
                }
            })));
        }
        Err((path, "the node has no kind".into()))
    }

    fn llm_agent(
        &self,
        id: &str,
        llm: &LlmDefinition,
        registry: &WorkflowRegistry,
    ) -> Result<Arc<dyn Agent>, (String, String)> {
        let model = registry
            .resolve_model(self, &llm.model)
            .map_err(|message| (".model".into(), message))?;
        let mut builder = adk_agent::LlmAgentBuilder::new(id).model(model);
        if let Some(instruction) = &llm.instruction {
            builder = builder.instruction(instruction);
        }
        if let Some(description) = &llm.description {
            builder = builder.description(description);
        }
        for (index, name) in llm.tools.iter().enumerate() {
            let tool = registry.tools.get(name).cloned().ok_or_else(|| {
                (format!(".tools[{index}]"), format!("no tool named '{name}' is registered"))
            })?;
            builder = builder.tool(tool);
        }
        let agent = builder.build().map_err(|error| (String::new(), error.message))?;
        Ok(Arc::new(agent))
    }
}

/// An agent node named `id`, whose input and output follow the node's `input` and `output_key`.
fn agent_node(id: &str, agent: Arc<dyn Agent>, node: &NodeDefinition) -> AgentNode {
    let mut built = AgentNode::new(agent).with_name(id);
    if let Some(template) = node.input.clone() {
        built = built.with_input_mapper(move |state| {
            adk_core::Content::new("user").with_text(interpolate_variables(&template, state))
        });
    }
    let key = node.output_key.clone();
    built.with_output_mapper(move |events| {
        let responses = response_texts(events);
        let mut updates = HashMap::new();
        if let Some(key) = &key {
            updates.insert(key.clone(), json!(responses.last().cloned().unwrap_or_default()));
        }
        if !responses.is_empty() {
            let messages: Vec<Value> = responses
                .into_iter()
                .map(|text| json!({ "role": "assistant", "content": text }))
                .collect();
            updates.insert("messages".to_string(), Value::Array(messages));
        }
        updates
    })
}

/// The text of each model response in `events`, with a streamed response's deltas joined.
fn response_texts(events: &[adk_core::Event]) -> Vec<String> {
    let mut deltas = adk_core::EventTextDeltas::default();
    let mut responses: Vec<(String, String)> = Vec::new();
    for event in events {
        let shown = deltas.push(event);
        let text: String = shown
            .content()
            .map(|content| content.parts.iter().filter_map(adk_core::Part::text).collect())
            .unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        match responses.last_mut() {
            Some((id, joined)) if *id == event.id => joined.push_str(&text),
            _ => responses.push((event.id.clone(), text)),
        }
    }
    responses.into_iter().map(|(_, text)| text).collect()
}

fn add_spec(mut graph: StateGraph, spec: &EdgeSpec) -> StateGraph {
    let source = graph_id(&spec.from).to_string();
    if let Some(to) = &spec.to {
        for target in to.ids() {
            graph = graph.add_edge(&source, graph_id(target));
        }
        return graph;
    }
    let default = graph_id(spec.default.as_deref().unwrap_or(END_ID)).to_string();
    let mut targets: HashMap<String, EdgeTarget> = HashMap::new();
    targets.insert(default.clone(), EdgeTarget::from(default.as_str()));
    let router: crate::edge::RouterFn = if let Some(field) = &spec.switch {
        let cases: Vec<(String, String)> = spec
            .cases
            .iter()
            .map(|(value, target)| (value.trim().to_string(), graph_id(target).to_string()))
            .collect();
        for (_, target) in &cases {
            targets.insert(target.clone(), EdgeTarget::from(target.as_str()));
        }
        let field = field.clone();
        Arc::new(move |state: &State| {
            let value = get_nested_value(state, &field).map(value_text).unwrap_or_default();
            let value = value.trim();
            cases
                .iter()
                .find(|(case, _)| case == value)
                .map_or_else(|| default.clone(), |(_, target)| target.clone())
        })
    } else {
        let routes: Vec<(Condition, String)> = spec
            .routes
            .iter()
            .map(|route| (route.when.clone(), graph_id(&route.to).to_string()))
            .collect();
        for (_, target) in &routes {
            targets.insert(target.clone(), EdgeTarget::from(target.as_str()));
        }
        Arc::new(move |state: &State| {
            routes
                .iter()
                .find(|(condition, _)| holds(condition, state))
                .map_or_else(|| default.clone(), |(_, target)| target.clone())
        })
    };
    graph.edges.push(Edge::Conditional { source, router, targets });
    graph
}

/// Whether `condition` holds over `state`, with `adk-action`'s switch operators.
pub(crate) fn holds(condition: &Condition, state: &State) -> bool {
    if let Some(all) = &condition.all {
        return all.iter().all(|inner| holds(inner, state));
    }
    if let Some(any) = &condition.any {
        return any.iter().any(|inner| holds(inner, state));
    }
    if let Some(inner) = &condition.not {
        return !holds(inner, state);
    }
    let (Some(field), Some(op)) = (&condition.field, condition.op) else {
        return false;
    };
    let value = match &condition.value {
        Some(Value::Array(items)) => items.iter().map(value_text).collect::<Vec<_>>().join(","),
        Some(value) => value_text(value),
        None => String::new(),
    };
    let expression =
        ExpressionMode { field: field.clone(), operator: op.action_name().to_string(), value };
    crate::action::switch::evaluate_condition(&expression, state)
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A `set` value with each string's `{{key}}` templates filled from state.
fn interpolate_value(value: &Value, state: &State) -> Value {
    match value {
        Value::String(template) => Value::String(interpolate_variables(template, state)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| interpolate_value(item, state)).collect())
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, item)| (key.clone(), interpolate_value(item, state)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The `adk-action` config of an `action:` node, with the standard properties it leaves out
/// taken from the node: its id and name, stop-on-error, tracing off, a 30 s timeout, and the
/// node's `output_key` (else its id) as the output key.
pub(crate) fn action_config(
    id: &str,
    action: &Value,
    output_key: Option<&str>,
) -> Result<ActionNodeConfig, String> {
    let Value::Object(fields) = action else {
        return Err("an action is a mapping with a `type`, such as `type: transform`".into());
    };
    if let Some(given) = fields.get("id").and_then(Value::as_str)
        && given != id
    {
        return Err(format!("the action's id '{given}' differs from its node id '{id}'; omit it"));
    }
    let mut fields: Map<String, Value> = fields.clone();
    let defaults = [
        ("id", json!(id)),
        ("name", json!(id)),
        ("errorHandling", json!({ "mode": "stop" })),
        ("tracing", json!({ "enabled": false, "logLevel": "none" })),
        ("callbacks", json!({})),
        ("execution", json!({ "timeout": 30_000 })),
        ("mapping", json!({ "outputKey": output_key.unwrap_or(id) })),
    ];
    for (key, value) in defaults {
        fields.entry(key).or_insert(value);
    }
    serde_json::from_value(Value::Object(fields))
        .map_err(|error| format!("not a valid action: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, Event};

    fn chunk(id: &str, text: &str, partial: bool) -> Event {
        let mut event = Event::new("invocation");
        event.id = id.to_string();
        event.llm_response.partial = partial;
        event.set_content(Content::new("model").with_text(text));
        event
    }

    #[test]
    fn a_streamed_response_is_one_text() {
        let mut terminal = chunk("r1", "Your refund is approved.", false);
        terminal.llm_response.provider_metadata = Some(json!({ "content_complete": true }));
        let events = [
            chunk("r1", "Your refund", true),
            chunk("r1", " is approved", true),
            chunk("r1", ".", true),
            terminal,
            chunk("r2", "Anything else?", false),
        ];
        assert_eq!(response_texts(&events), ["Your refund is approved.", "Anything else?"]);
    }
}
