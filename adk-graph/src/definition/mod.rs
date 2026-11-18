//! Graph workflows defined in YAML or JSON.
//!
//! A [`WorkflowDefinition`] declares a graph as data — agent and LLM nodes, function, `set`,
//! action and interrupt nodes, edges with conditional routing, checkpointing — and
//! [`WorkflowDefinition::build`] compiles it into a [`CompiledGraph`](crate::CompiledGraph)
//! against a [`WorkflowRegistry`] that supplies the agents, models, tools and functions it names.
//!
//! ```yaml
//! # yaml-language-server: $schema=https://raw.githubusercontent.com/zavora-ai/adk-rust/main/adk-graph/schemas/workflow.schema.json
//! version: 1
//! name: refund_desk
//! models:
//!   fast: anthropic/claude-haiku-5-5
//! nodes:
//!   classify:
//!     llm:
//!       model: fast
//!       instruction: Reply with one word, refund or question.
//!     output_key: category
//!   approve:
//!     interrupt:
//!       message: "Refund requested: {{input}}. Approve?"
//!     output_key: approval
//!   reply:
//!     llm: { model: fast, instruction: Write a one-line reply to the customer. }
//! edges:
//!   - [START, classify]
//!   - from: classify
//!     routes:
//!       - when: { field: category, op: matches, value: "(?i)refund" }
//!         to: approve
//!     default: reply
//!   - [approve, reply, END]
//! ```
//!
//! Conditions are data — a field, an operator and a value, combined with `all`, `any` and
//! `not` — evaluated by the same operators as `adk-action`'s switch node, so a workflow file
//! never carries code. Every parse and validation issue names its path in the document and,
//! for YAML, its line and column.

mod build;
mod location;
mod types;

use std::path::Path;
use std::sync::Arc;

pub use build::{FunctionNodeFn, ModelResolver, WorkflowRegistry};
pub use location::SourceLocation;
pub use types::{
    ChannelDefinition, CheckpointDefinition, CheckpointStore, Condition, ConditionOp,
    EdgeDefinition, EdgeSpec, InterruptDefinition, LlmDefinition, NodeDefinition, ReducerKind,
    RouteDefinition, Targets, WORKFLOW_FORMAT_VERSION, WorkflowDefinition,
};

use location::LocationIndex;

/// One problem with a workflow document.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WorkflowIssue {
    /// Where in the document, such as `edges[2].to`; empty for the whole document.
    pub path: String,
    /// The line and column, for a YAML document.
    pub location: Option<SourceLocation>,
    /// What is wrong, and how to fix it.
    pub message: String,
}

impl WorkflowIssue {
    /// An issue at `path`, optionally located.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::definition::{SourceLocation, WorkflowIssue};
    ///
    /// let issue = WorkflowIssue::new(
    ///     "edges[0].to",
    ///     Some(SourceLocation { line: 9, column: 9 }),
    ///     "unknown node 'b'",
    /// );
    /// assert_eq!(issue.to_string(), "9:9: edges[0].to: unknown node 'b'");
    /// ```
    pub fn new(
        path: impl Into<String>,
        location: Option<SourceLocation>,
        message: impl Into<String>,
    ) -> Self {
        Self { path: path.into(), location, message: message.into() }
    }
}

impl std::fmt::Display for WorkflowIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(location) = self.location {
            write!(f, "{location}: ")?;
        }
        if !self.path.is_empty() {
            write!(f, "{}: ", self.path)?;
        }
        f.write_str(&self.message)
    }
}

/// A workflow document that does not parse, validate or build, with every issue found.
///
/// # Example
///
/// ```rust
/// use adk_graph::definition::WorkflowDefinition;
///
/// let error = WorkflowDefinition::from_yaml_str(
///     "version: 1\nname: w\nnodes:\n  a: { set: { x: 1 } }\nedges:\n  - [START, a, b]\n",
/// )
/// .unwrap_err();
/// assert_eq!(error.issues[0].path, "edges[0][2]");
/// assert_eq!(error.issues[0].location.unwrap().line, 6);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", self.render())]
#[non_exhaustive]
pub struct WorkflowError {
    /// The document's name, such as its file path.
    pub source_name: Option<String>,
    /// Every issue, in document order where known.
    pub issues: Vec<WorkflowIssue>,
}

impl WorkflowError {
    fn render(&self) -> String {
        let prefix = self.source_name.as_deref().map(|name| format!("{name}:")).unwrap_or_default();
        let lines: Vec<String> = self
            .issues
            .iter()
            .map(|issue| match (&prefix, issue.location) {
                (prefix, Some(_)) if !prefix.is_empty() => format!("{prefix}{issue}"),
                (prefix, _) if !prefix.is_empty() => format!("{prefix} {issue}"),
                _ => issue.to_string(),
            })
            .collect();
        lines.join("\n")
    }
}

impl From<WorkflowError> for crate::GraphError {
    fn from(error: WorkflowError) -> Self {
        crate::GraphError::InvalidGraph(error.to_string())
    }
}

impl From<WorkflowError> for adk_core::AdkError {
    fn from(error: WorkflowError) -> Self {
        crate::GraphError::from(error).into()
    }
}

/// What a document was read from, for issue locations and relative paths.
#[derive(Debug, Clone, Default)]
pub(crate) struct SourceInfo {
    pub(crate) name: Option<String>,
    pub(crate) index: Option<Arc<LocationIndex>>,
    #[cfg_attr(
        not(feature = "sqlite"),
        expect(dead_code, reason = "only the sqlite store resolves a path against the file")
    )]
    pub(crate) base_dir: Option<std::path::PathBuf>,
}

/// Source information does not take part in equality: two definitions are equal when they
/// declare the same workflow.
impl PartialEq for SourceInfo {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl SourceInfo {
    pub(crate) fn error(&self, issues: Vec<(String, String)>) -> WorkflowError {
        let mut issues: Vec<WorkflowIssue> = issues
            .into_iter()
            .map(|(path, message)| WorkflowIssue {
                location: self.index.as_ref().and_then(|index| index.locate(&path)),
                path,
                message,
            })
            .collect();
        issues.sort_by_key(|issue| issue.location.map(|l| (l.line, l.column)));
        WorkflowError { source_name: self.name.clone(), issues }
    }
}

impl WorkflowDefinition {
    /// Parses and validates a YAML workflow.
    ///
    /// # Errors
    ///
    /// Returns every syntax, type and validation issue, each with its path and its line and
    /// column.
    pub fn from_yaml_str(yaml: &str) -> Result<Self, WorkflowError> {
        Self::parse_yaml(yaml, SourceInfo::default())
    }

    /// Parses and validates a JSON workflow.
    ///
    /// # Errors
    ///
    /// Returns the syntax or type issue with its line and column, or every validation issue
    /// with its path.
    pub fn from_json_str(json: &str) -> Result<Self, WorkflowError> {
        Self::parse_json(json, SourceInfo::default())
    }

    /// Reads, parses and validates a workflow file: JSON when its extension is `.json`, YAML
    /// otherwise. A SQLite checkpoint path in it is relative to the file.
    ///
    /// # Errors
    ///
    /// Returns an issue when the file cannot be read, and every parse or validation issue,
    /// prefixed with the file's path.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, WorkflowError> {
        let path = path.as_ref();
        let source = SourceInfo {
            name: Some(path.display().to_string()),
            index: None,
            base_dir: path.parent().map(Path::to_path_buf),
        };
        let text = std::fs::read_to_string(path).map_err(|error| {
            source.error(vec![(String::new(), format!("cannot read the workflow file: {error}"))])
        })?;
        if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("json")) {
            Self::parse_json(&text, source)
        } else {
            Self::parse_yaml(&text, source)
        }
    }

    /// Serializes the workflow as YAML.
    ///
    /// # Errors
    ///
    /// Returns an issue when a value cannot be written as YAML.
    pub fn to_yaml_string(&self) -> Result<String, WorkflowError> {
        serde_saphyr::to_string(self).map_err(|error| {
            self.source.error(vec![(String::new(), format!("cannot write YAML: {error}"))])
        })
    }

    /// Checks the workflow without building it: node kinds, references between nodes and
    /// edges, conditions, interrupts and checkpointing. [`build`](Self::build) checks the
    /// references to the registry as well.
    ///
    /// # Errors
    ///
    /// Returns every issue found.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        let issues = validate::issues(self);
        if issues.is_empty() { Ok(()) } else { Err(self.source.error(issues)) }
    }

    fn parse_yaml(yaml: &str, mut source: SourceInfo) -> Result<Self, WorkflowError> {
        let index = Arc::new(LocationIndex::of_yaml(yaml));
        source.index = Some(index);
        let mut definition: Self = serde_saphyr::from_str(yaml).map_err(|error| {
            let location = error
                .location()
                .filter(|location| location.line() > 0)
                .map(|l| SourceLocation { line: l.line() as u32, column: l.column() as u32 });
            WorkflowError {
                source_name: source.name.clone(),
                issues: vec![WorkflowIssue {
                    path: String::new(),
                    location,
                    message: first_line(&error.to_string()),
                }],
            }
        })?;
        definition.source = source;
        definition.validate()?;
        Ok(definition)
    }

    fn parse_json(json: &str, source: SourceInfo) -> Result<Self, WorkflowError> {
        let mut definition: Self = serde_json::from_str(json).map_err(|error| WorkflowError {
            source_name: source.name.clone(),
            issues: vec![WorkflowIssue {
                path: String::new(),
                location: (error.line() > 0).then(|| SourceLocation {
                    line: error.line() as u32,
                    column: error.column() as u32,
                }),
                message: error.to_string(),
            }],
        })?;
        definition.source = source;
        definition.validate()?;
        Ok(definition)
    }
}

/// The first line of a parser's message, without the source snippet some parsers append.
fn first_line(message: &str) -> String {
    message.lines().next().unwrap_or(message).trim().to_string()
}

/// The JSON Schema of the workflow format, for editor completion and validation.
///
/// The committed copy is `adk-graph/schemas/workflow.schema.json`; a YAML file opts in with
/// `# yaml-language-server: $schema=<url of that file>`.
///
/// # Example
///
/// ```rust
/// let schema = adk_graph::definition::workflow_json_schema();
/// assert_eq!(schema["title"], "ADK-Rust graph workflow");
/// ```
pub fn workflow_json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(WorkflowDefinition)).unwrap_or_default()
}

mod validate {
    //! Structural checks that need no registry.

    use std::collections::{BTreeSet, HashSet, VecDeque};

    use super::types::{
        CheckpointStore, Condition, ConditionOp, EdgeDefinition, EdgeSpec, NodeDefinition,
        WORKFLOW_FORMAT_VERSION, WorkflowDefinition,
    };

    pub(super) const START_ID: &str = "START";
    pub(super) const END_ID: &str = "END";

    type Issues = Vec<(String, String)>;

    pub(super) fn issues(workflow: &WorkflowDefinition) -> Issues {
        let mut issues = Issues::new();
        if workflow.version != WORKFLOW_FORMAT_VERSION {
            issues.push((
                "version".into(),
                format!(
                    "version {} is not supported; this release reads version {WORKFLOW_FORMAT_VERSION}",
                    workflow.version
                ),
            ));
        }
        if workflow.name.trim().is_empty() {
            issues.push(("name".into(), "the workflow needs a name".into()));
        }
        if workflow.nodes.is_empty() {
            issues.push(("nodes".into(), "the workflow declares no nodes".into()));
        }
        for (name, reference) in &workflow.models {
            if reference.trim().is_empty() {
                issues.push((format!("models.{name}"), "the model reference is empty".into()));
            }
        }
        for (id, node) in &workflow.nodes {
            check_node(id, node, &mut issues);
        }

        let declared: BTreeSet<&str> = workflow.nodes.keys().map(String::as_str).collect();
        let mut successors: Vec<(&str, &str)> = Vec::new();
        for (index, edge) in workflow.edges.iter().enumerate() {
            let path = format!("edges[{index}]");
            match edge {
                EdgeDefinition::Chain(ids) => {
                    if ids.len() < 2 {
                        issues.push((path.clone(), "a chain needs at least two node ids".into()));
                    }
                    for (position, id) in ids.iter().enumerate() {
                        let is_first = position == 0;
                        let is_last = position + 1 == ids.len();
                        check_reference(
                            &format!("{path}[{position}]"),
                            id,
                            &declared,
                            is_first,
                            is_last,
                            &mut issues,
                        );
                    }
                    for pair in ids.windows(2) {
                        successors.push((pair[0].as_str(), pair[1].as_str()));
                    }
                }
                EdgeDefinition::Edge(spec) => {
                    for target in check_spec(&path, spec, &declared, &mut issues) {
                        successors.push((spec.from.as_str(), target));
                    }
                }
            }
        }

        if !successors.iter().any(|(from, _)| *from == START_ID) {
            issues.push((
                "edges".into(),
                "no edge leaves START; add one such as `- [START, <first node>]`".into(),
            ));
        } else {
            let mut reached: HashSet<&str> = HashSet::from([START_ID]);
            let mut queue = VecDeque::from([START_ID]);
            while let Some(node) = queue.pop_front() {
                for (_, next) in successors.iter().filter(|(from, _)| *from == node) {
                    if reached.insert(next) {
                        queue.push_back(next);
                    }
                }
            }
            for id in &declared {
                if !reached.contains(id) {
                    issues.push((
                        format!("nodes.{id}"),
                        format!("node '{id}' is unreachable from START; add an edge to it"),
                    ));
                }
            }
        }

        for (field, ids) in [
            ("interrupt_before", &workflow.interrupt_before),
            ("interrupt_after", &workflow.interrupt_after),
        ] {
            for (index, id) in ids.iter().enumerate() {
                if !declared.contains(id.as_str()) {
                    issues.push((format!("{field}[{index}]"), unknown_node(id, &declared)));
                }
            }
        }

        let pauses = !workflow.interrupt_before.is_empty()
            || !workflow.interrupt_after.is_empty()
            || workflow.nodes.values().any(|node| node.interrupt.is_some());
        match workflow.checkpoint.store {
            CheckpointStore::None if pauses => issues.push((
                "checkpoint.store".into(),
                "a workflow that pauses needs checkpoints to resume from; use `memory` or `sqlite`"
                    .into(),
            )),
            CheckpointStore::Sqlite if workflow.checkpoint.path.is_none() => issues.push((
                "checkpoint".into(),
                "a `sqlite` store needs a `path` to the database file".into(),
            )),
            CheckpointStore::Memory | CheckpointStore::None
                if workflow.checkpoint.path.is_some() =>
            {
                issues.push((
                    "checkpoint.path".into(),
                    "`path` applies only to the `sqlite` store".into(),
                ));
            }
            _ => {}
        }
        if workflow.recursion_limit == Some(0) {
            issues
                .push(("recursion_limit".into(), "the recursion limit must be at least 1".into()));
        }
        issues
    }

    fn check_node(id: &str, node: &NodeDefinition, issues: &mut Issues) {
        let path = format!("nodes.{id}");
        if !valid_id(id) {
            issues.push((
                path.clone(),
                format!(
                    "'{id}' is not a valid node id; use letters, digits, `_` and `-`, starting \
                     with a letter or `_`, and not START or END"
                ),
            ));
        }
        let kinds: Vec<&str> = [
            ("agent", node.agent.is_some()),
            ("llm", node.llm.is_some()),
            ("function", node.function.is_some()),
            ("set", node.set.is_some()),
            ("action", node.action.is_some()),
            ("interrupt", node.interrupt.is_some()),
        ]
        .into_iter()
        .filter_map(|(kind, set)| set.then_some(kind))
        .collect();
        match kinds.as_slice() {
            [] => issues.push((
                path.clone(),
                "the node has no kind; set one of agent, llm, function, set, action or interrupt"
                    .into(),
            )),
            [_] => {}
            many => issues.push((
                path.clone(),
                format!("the node sets {}; set exactly one kind", many.join(" and ")),
            )),
        }
        let runs_agent = node.agent.is_some() || node.llm.is_some();
        if node.input.is_some() && !runs_agent {
            issues.push((
                format!("{path}.input"),
                "`input` applies only to agent and llm nodes".into(),
            ));
        }
        if node.output_key.is_some() && (node.set.is_some() || node.function.is_some()) {
            issues.push((
                format!("{path}.output_key"),
                "`output_key` does not apply to set and function nodes, which name their own keys"
                    .into(),
            ));
        }
        if node.output_key.as_deref().is_some_and(|key| key.trim().is_empty()) {
            issues.push((format!("{path}.output_key"), "the output key is empty".into()));
        }
        if node.max_attempts == Some(0) {
            issues
                .push((format!("{path}.max_attempts"), "a node needs at least one attempt".into()));
        }
        if let Some(agent) = &node.agent
            && agent.trim().is_empty()
        {
            issues.push((format!("{path}.agent"), "the agent name is empty".into()));
        }
        if let Some(llm) = &node.llm
            && llm.model.trim().is_empty()
        {
            issues.push((format!("{path}.llm.model"), "the model reference is empty".into()));
        }
        if let Some(function) = &node.function
            && function.trim().is_empty()
        {
            issues.push((format!("{path}.function"), "the function name is empty".into()));
        }
        if let Some(set) = &node.set
            && set.is_empty()
        {
            issues.push((format!("{path}.set"), "`set` names no keys".into()));
        }
        if let Some(action) = &node.action
            && let Err(message) =
                super::build::action_config(id, action, node.output_key.as_deref())
        {
            issues.push((format!("{path}.action"), message));
        }
        if let Some(interrupt) = &node.interrupt {
            if interrupt.message.trim().is_empty() {
                issues.push((format!("{path}.interrupt.message"), "the message is empty".into()));
            }
            if let Some(schema) = &interrupt.response_schema
                && let Err(error) = jsonschema::validator_for(schema)
            {
                issues.push((
                    format!("{path}.interrupt.response_schema"),
                    format!("not a valid JSON Schema: {error}"),
                ));
            }
        }
    }

    /// Checks an edge object; returns the targets it can reach.
    fn check_spec<'a>(
        path: &str,
        spec: &'a EdgeSpec,
        declared: &BTreeSet<&str>,
        issues: &mut Issues,
    ) -> Vec<&'a str> {
        check_reference(&format!("{path}.from"), &spec.from, declared, true, false, issues);
        let forms = [spec.to.is_some(), !spec.routes.is_empty(), spec.switch.is_some()];
        let mut targets: Vec<(String, &'a str)> = Vec::new();
        match forms.iter().filter(|set| **set).count() {
            0 => issues.push((
                path.into(),
                "the edge has no target; set `to`, `routes`, or `switch` with `cases`".into(),
            )),
            1 => {}
            _ => issues.push((path.into(), "set only one of `to`, `routes` and `switch`".into())),
        }
        if let Some(to) = &spec.to {
            let ids = to.ids();
            if ids.is_empty() {
                issues.push((format!("{path}.to"), "`to` lists no nodes".into()));
            }
            if ids.len() == 1 {
                targets.push((format!("{path}.to"), ids[0]));
            } else {
                for (index, id) in ids.into_iter().enumerate() {
                    targets.push((format!("{path}.to[{index}]"), id));
                }
            }
            if spec.default.is_some() {
                issues.push((
                    format!("{path}.default"),
                    "`default` applies only to `routes` and `switch`".into(),
                ));
            }
        }
        if spec.from == START_ID && (!spec.routes.is_empty() || spec.switch.is_some()) {
            issues.push((
                format!("{path}.from"),
                "edges from START cannot be conditional; route from the first node instead".into(),
            ));
        }
        for (index, route) in spec.routes.iter().enumerate() {
            check_condition(&format!("{path}.routes[{index}].when"), &route.when, issues);
            targets.push((format!("{path}.routes[{index}].to"), route.to.as_str()));
        }
        if let Some(field) = &spec.switch {
            if field.trim().is_empty() {
                issues.push((format!("{path}.switch"), "the state path is empty".into()));
            }
            if spec.cases.is_empty() {
                issues.push((format!("{path}.cases"), "`switch` needs `cases`".into()));
            }
        }
        if !spec.cases.is_empty() && spec.switch.is_none() {
            issues.push((format!("{path}.cases"), "`cases` applies only with `switch`".into()));
        }
        for (value, target) in &spec.cases {
            targets.push((format!("{path}.cases.{value}"), target.as_str()));
        }
        if let Some(default) = &spec.default {
            targets.push((format!("{path}.default"), default.as_str()));
        }
        for (target_path, id) in &targets {
            check_reference(target_path, id, declared, false, true, issues);
        }
        targets.into_iter().map(|(_, id)| id).collect()
    }

    fn check_condition(path: &str, condition: &Condition, issues: &mut Issues) {
        let comparison =
            condition.field.is_some() || condition.op.is_some() || condition.value.is_some();
        let forms =
            [comparison, condition.all.is_some(), condition.any.is_some(), condition.not.is_some()];
        match forms.iter().filter(|set| **set).count() {
            0 => {
                issues.push((
                    path.into(),
                    "the condition is empty; set `field` and `op`, or `all`, `any` or `not`".into(),
                ));
                return;
            }
            1 => {}
            _ => issues.push((
                path.into(),
                "set only one of a comparison (`field`, `op`, `value`), `all`, `any` and `not`"
                    .into(),
            )),
        }
        if comparison {
            match &condition.field {
                Some(field) if !field.trim().is_empty() => {}
                _ => issues.push((format!("{path}.field"), "a comparison needs a `field`".into())),
            }
            match condition.op {
                None => issues.push((format!("{path}.op"), "a comparison needs an `op`".into())),
                Some(op) if op.takes_value() && condition.value.is_none() => issues
                    .push((format!("{path}.value"), format!("`{}` needs a `value`", op_name(op)))),
                Some(op) if !op.takes_value() && condition.value.is_some() => issues
                    .push((format!("{path}.value"), format!("`{}` takes no `value`", op_name(op)))),
                Some(ConditionOp::Matches) => {
                    let pattern = condition.value.as_ref().and_then(|value| value.as_str());
                    match pattern.map(regex::Regex::new) {
                        Some(Ok(_)) => {}
                        Some(Err(error)) => issues.push((
                            format!("{path}.value"),
                            format!("not a valid regular expression: {error}"),
                        )),
                        None => issues.push((
                            format!("{path}.value"),
                            "`matches` needs a regular expression string".into(),
                        )),
                    }
                }
                Some(_) => {}
            }
            if let Some(value) = &condition.value
                && (value.is_object()
                    || (value.is_array() && condition.op != Some(ConditionOp::In)))
            {
                issues.push((
                    format!("{path}.value"),
                    "the value must be a string, number or boolean (a list only for `in`)".into(),
                ));
            }
        }
        for (key, list) in [("all", &condition.all), ("any", &condition.any)] {
            if let Some(list) = list {
                if list.is_empty() {
                    issues.push((format!("{path}.{key}"), format!("`{key}` lists no conditions")));
                }
                for (index, inner) in list.iter().enumerate() {
                    check_condition(&format!("{path}.{key}[{index}]"), inner, issues);
                }
            }
        }
        if let Some(inner) = &condition.not {
            check_condition(&format!("{path}.not"), inner, issues);
        }
    }

    fn op_name(op: ConditionOp) -> String {
        serde_json::to_value(op)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default()
    }

    fn check_reference(
        path: &str,
        id: &str,
        declared: &BTreeSet<&str>,
        may_be_start: bool,
        may_be_end: bool,
        issues: &mut Issues,
    ) {
        match id {
            START_ID if may_be_start => {}
            START_ID => issues.push((path.into(), "START can only begin an edge".into())),
            END_ID if may_be_end => {}
            END_ID => issues.push((path.into(), "END can only finish an edge".into())),
            id if declared.contains(id) => {}
            id => issues.push((path.into(), unknown_node(id, declared))),
        }
    }

    fn unknown_node(id: &str, declared: &BTreeSet<&str>) -> String {
        let known: Vec<&str> = declared.iter().copied().collect();
        format!("unknown node '{id}'; declared nodes are {}", known.join(", "))
    }

    pub(super) fn valid_id(id: &str) -> bool {
        let mut chars = id.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            && !matches!(id, START_ID | END_ID | crate::START | crate::END)
    }
}
