//! The workflow definition format: what a YAML or JSON workflow file holds.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::de::value::{MapAccessDeserializer, SeqAccessDeserializer};
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The workflow format version this release reads and writes.
pub const WORKFLOW_FORMAT_VERSION: u32 = 1;

/// A graph workflow, as a YAML or JSON file declares it.
///
/// [`WorkflowDefinition::build`](crate::definition::WorkflowDefinition::build) compiles it into
/// an [`adk-graph`](crate) graph. The format is documented in the workflow guide and described by
/// the JSON Schema [`workflow_json_schema`](crate::definition::workflow_json_schema) returns.
///
/// # Example
///
/// ```rust
/// use adk_graph::definition::WorkflowDefinition;
///
/// let workflow = WorkflowDefinition::from_yaml_str(
///     r#"
/// version: 1
/// name: greet
/// nodes:
///   hello:
///     set: { greeting: "Hello, {{input}}" }
/// edges:
///   - [START, hello, END]
/// "#,
/// )
/// .unwrap();
/// assert_eq!(workflow.nodes.len(), 1);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "ADK-Rust graph workflow",
    description = "A graph workflow that adk-graph compiles into a graph: nodes, edges with \
                   conditional routes, state channels, interrupts and checkpointing."
)]
#[non_exhaustive]
pub struct WorkflowDefinition {
    /// Format version. Must be `1`.
    pub version: u32,
    /// Workflow name; a graph agent built from it takes this name.
    pub name: String,
    /// What the workflow does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// State channels that need a reducer or a default. A key no channel names is overwritten
    /// by each update. `messages`, which agent nodes append to, appends unless declared here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub state: BTreeMap<String, ChannelDefinition>,
    /// Model aliases: a name an `llm` node can use in place of a `provider/model` reference.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, String>,
    /// The nodes, by id.
    pub nodes: BTreeMap<String, NodeDefinition>,
    /// The edges, in order. `START` and `END` name the graph's entry and exit.
    pub edges: Vec<EdgeDefinition>,
    /// Nodes the run pauses before; resuming the thread runs the node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interrupt_before: Vec<String>,
    /// Nodes the run pauses after; resuming the thread continues past them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interrupt_after: Vec<String>,
    /// Where checkpoints are kept. Defaults to memory.
    #[serde(default, skip_serializing_if = "CheckpointDefinition::is_default")]
    pub checkpoint: CheckpointDefinition,
    /// The most supersteps one run may take. Defaults to the graph's limit, 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recursion_limit: Option<usize>,
    /// Where the document was read from: its name, value locations and directory.
    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) source: super::SourceInfo,
}

/// A state channel.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ChannelDefinition {
    /// How an update combines with the current value. Defaults to `overwrite`.
    #[serde(default)]
    pub reducer: ReducerKind,
    /// The channel's value before any node writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

/// How a channel combines an update with its current value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReducerKind {
    /// The update replaces the value.
    #[default]
    Overwrite,
    /// The update is appended; a list update appends each item.
    Append,
    /// The update is added to a number.
    Sum,
}

/// One node. Exactly one of `agent`, `llm`, `function`, `set`, `action` and `interrupt` is set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct NodeDefinition {
    /// What the node does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Runs a registered agent, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Runs an LLM agent declared here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmDefinition>,
    /// Runs a registered function node, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// Writes state keys. A string value is a template: `{{key}}` and `{{key.field}}` read state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<BTreeMap<String, Value>>,
    /// Runs an `adk-action` action node (`type: http`, `transform`, `switch`, ...). Its
    /// `id`, `name`, error handling, tracing and output mapping default from the node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<BTreeMap<String, Value>>")]
    pub action: Option<Value>,
    /// Pauses the run for an answer, which a resume supplies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt: Option<InterruptDefinition>,
    /// For `agent` and `llm` nodes: a template for the message the agent receives. Defaults to
    /// the last message in `messages`, else `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// The state key the node writes: an agent's final text, an interrupt's answer, or an
    /// action's result. Agent nodes also append their text to `messages`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_key: Option<String>,
    /// Attempts for a node that fails, including the first. Defaults to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
}

/// An LLM agent declared in the workflow.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct LlmDefinition {
    /// A model alias from `models`, a registered model name, or a `provider/model` reference
    /// such as `anthropic/claude-haiku-5-5`.
    pub model: String,
    /// The agent's instruction. `{key}` placeholders read session state, as in any `LlmAgent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// What the agent does, for its description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Registered tools the agent may call, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
}

/// A pause for an answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct InterruptDefinition {
    /// What the run asks; a template over state.
    pub message: String,
    /// A JSON Schema the answer must satisfy; checked when the run resumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Value>,
}

/// Where a workflow keeps its checkpoints.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CheckpointDefinition {
    /// The store. Defaults to `memory`.
    #[serde(default)]
    pub store: CheckpointStore,
    /// For `sqlite`: the database file, relative to the workflow file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl CheckpointDefinition {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// A checkpoint store.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CheckpointStore {
    /// In process memory: a thread resumes only in the process that paused it.
    #[default]
    Memory,
    /// A SQLite file (the `sqlite` feature).
    Sqlite,
    /// No checkpoints: a workflow with interrupts cannot use this.
    None,
}

/// An edge item: a chain of node ids, or an edge object.
///
/// A chain `[START, a, b, END]` adds an edge between each consecutive pair.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(untagged)]
#[non_exhaustive]
pub enum EdgeDefinition {
    /// Consecutive node ids, each connected to the next.
    Chain(Vec<String>),
    /// One edge from a node.
    Edge(EdgeSpec),
}

impl<'de> Deserialize<'de> for EdgeDefinition {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EdgeVisitor;

        impl<'de> Visitor<'de> for EdgeVisitor {
            type Value = EdgeDefinition;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a chain of node ids, or an edge object with `from`")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                Vec::deserialize(SeqAccessDeserializer::new(seq)).map(EdgeDefinition::Chain)
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                EdgeSpec::deserialize(MapAccessDeserializer::new(map)).map(EdgeDefinition::Edge)
            }
        }

        deserializer.deserialize_any(EdgeVisitor)
    }
}

/// One edge from a node. Exactly one of `to`, `routes` and `switch` is set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct EdgeSpec {
    /// The source node, or `START`.
    pub from: String,
    /// Always continue to these nodes; more than one runs them in parallel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Targets>,
    /// Conditional routes, tried in order; the first whose `when` holds is taken.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<RouteDefinition>,
    /// A state path whose value selects the next node from `cases`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch: Option<String>,
    /// For `switch`: the node for each value, compared as text after trimming.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cases: BTreeMap<String, String>,
    /// For `routes` and `switch`: the node when nothing matches. Defaults to `END`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// One node id, or a list of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(untagged)]
#[non_exhaustive]
pub enum Targets {
    /// One node.
    One(String),
    /// Several nodes, run in parallel.
    Many(Vec<String>),
}

impl Targets {
    /// The node ids, in order.
    pub fn ids(&self) -> Vec<&str> {
        match self {
            Self::One(id) => vec![id.as_str()],
            Self::Many(ids) => ids.iter().map(String::as_str).collect(),
        }
    }
}

impl<'de> Deserialize<'de> for Targets {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TargetsVisitor;

        impl<'de> Visitor<'de> for TargetsVisitor {
            type Value = Targets;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a node id or a list of node ids")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(Targets::One(value.to_string()))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                Vec::deserialize(SeqAccessDeserializer::new(seq)).map(Targets::Many)
            }
        }

        deserializer.deserialize_any(TargetsVisitor)
    }
}

/// A conditional route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RouteDefinition {
    /// When to take the route.
    pub when: Condition,
    /// The node the route goes to, or `END`.
    pub to: String,
}

/// A condition over state: a comparison (`field`, `op`, `value`), or `all`, `any` or `not` of
/// conditions. There is no free-form expression: a condition is data, never code.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Condition {
    /// A dotted state path, such as `review.approved`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<ConditionOp>,
    /// The value to compare with: a string, number or boolean, or a list for `in`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Holds when every condition holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all: Option<Vec<Condition>>,
    /// Holds when any condition holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub any: Option<Vec<Condition>>,
    /// Holds when the condition does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not: Option<Box<Condition>>,
}

impl Condition {
    /// A comparison of the state at `field` with `value`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::definition::{Condition, ConditionOp};
    /// use serde_json::json;
    ///
    /// let refund = Condition::compare("category", ConditionOp::Contains, json!("refund"));
    /// assert_eq!(refund.op, Some(ConditionOp::Contains));
    /// ```
    pub fn compare(field: impl Into<String>, op: ConditionOp, value: Value) -> Self {
        Self { field: Some(field.into()), op: Some(op), value: Some(value), ..Self::default() }
    }
}

/// A comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConditionOp {
    /// Equal; numbers compare numerically, booleans as `true`/`false`.
    Eq,
    /// Not equal.
    Neq,
    /// Greater than, numerically.
    Gt,
    /// Less than, numerically.
    Lt,
    /// Greater than or equal, numerically.
    Gte,
    /// Less than or equal, numerically.
    Lte,
    /// A string contains the value, or a list holds it.
    Contains,
    /// A string starts with the value.
    StartsWith,
    /// A string ends with the value.
    EndsWith,
    /// A regular expression matches; `(?i)` makes it case-insensitive.
    Matches,
    /// The value is one of a list.
    In,
    /// The value is absent, null, or empty.
    Empty,
    /// The value is present.
    Exists,
}

impl ConditionOp {
    /// The operator's name in `adk-action`'s switch conditions.
    pub(crate) fn action_name(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Neq => "neq",
            Self::Gt => "gt",
            Self::Lt => "lt",
            Self::Gte => "gte",
            Self::Lte => "lte",
            Self::Contains => "contains",
            Self::StartsWith => "startsWith",
            Self::EndsWith => "endsWith",
            Self::Matches => "matches",
            Self::In => "in",
            Self::Empty => "empty",
            Self::Exists => "exists",
        }
    }

    /// Whether the operator compares with a `value`.
    pub(crate) fn takes_value(self) -> bool {
        !matches!(self, Self::Empty | Self::Exists)
    }
}
