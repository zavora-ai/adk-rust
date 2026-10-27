//! The workflow observer's view models (spec 3.3 §12 developer track): what the web home's
//! Runs tab and `adk-rust graph steps` show, built in Rust so every renderer shares them.
//!
//! Developer panels may use technical words (spec 3.3 §8); a decision's card stays the
//! engine's, drawn from the inbox entry.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A graph agent the observer shows: its nodes, edges and channels (spec 3.3 §12).
///
/// # Example
///
/// ```rust
/// use adk_surface::workflow::{EdgeView, GraphView};
///
/// let graph = GraphView {
///     name: "invoice-run".into(),
///     description: String::new(),
///     nodes: Vec::new(),
///     edges: vec![EdgeView { from: "draft".into(), to: "pay".into(), conditional: true }],
///     entry: vec!["draft".into()],
///     channels: vec!["invoice".into(), "note".into()],
/// };
/// assert_eq!(serde_json::to_value(&graph).unwrap()["edges"][0]["conditional"], true);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphView {
    /// The graph agent's name: `invoice-run`.
    pub name: String,
    /// The graph agent's description.
    pub description: String,
    /// Every node, sorted by name.
    pub nodes: Vec<NodeView>,
    /// Every edge; `to` is `__end__` for an edge that ends a branch.
    pub edges: Vec<EdgeView>,
    /// The nodes a run starts with.
    pub entry: Vec<String>,
    /// The state channels the graph declares, which a fork may edit.
    pub channels: Vec<String>,
}

/// One node of a [`GraphView`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    /// The node's name.
    pub name: String,
    /// The node's description.
    pub description: String,
    /// What kind of step it is, as adk-graph names it: `modelStep`, `function`, `httpCall`, …
    pub kind: String,
    /// The kind in words: "Model step", "HTTP call", "Function".
    pub kind_text: String,
    /// "Asks first", "Spends money", "Can't be undone".
    pub badges: Vec<String>,
}

/// One edge of a [`GraphView`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EdgeView {
    /// The node the edge leaves.
    pub from: String,
    /// The node the edge reaches, or `__end__`.
    pub to: String,
    /// Whether a router chooses the edge at run time.
    pub conditional: bool,
}

/// How a run stands (spec 3.3 §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus {
    /// The session's thread has no checkpoint yet.
    NoRun,
    /// A fork made and not started.
    NotStarted,
    /// A step is running.
    Working,
    /// A governed node waits for a decision.
    Waiting,
    /// A node paused with a question that is not a decision.
    Paused,
    /// Everything is stopped while the run has work left.
    Stopped,
    /// The run finished.
    Done,
    /// A step failed.
    Failed,
}

/// What became of a node, as the run view shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum NodeState {
    /// Not reached.
    Idle,
    /// Running now.
    Working,
    /// Finished.
    Done,
    /// A governed node waiting for its decision.
    Waiting,
    /// Paused with a question.
    Paused,
    /// Failed.
    Failed,
    /// A governed call that already happened, answered from its record.
    Recorded,
    /// Not run: refused, denied, or the step failed before it started.
    NotRun,
}

/// One node's part in a run or a step (spec 3.3 §12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeRun {
    /// The node's name.
    pub node: String,
    /// What became of it.
    pub state: NodeState,
    /// In words: "Done", "Waiting for you", "Not sent again", "Not run", …
    pub state_text: String,
    /// When it started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// That time as the home shows it: `09:14:01`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_text: Option<String>,
    /// How long it ran, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// That duration in words: `1.2 s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_text: Option<String>,
    /// Its attempt count when it finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// The state keys it wrote, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<String>,
    /// The error it failed or was refused with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The decision a waiting node waits on, from the inbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    /// For a fork, the time the original run did this node: "from 09:14".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_text: Option<String>,
}

/// How a step ended, as the run view lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StepState {
    /// The person's message, which started the run.
    Input,
    /// Every node finished.
    Completed,
    /// A node paused or waits.
    Paused,
    /// A node failed.
    Failed,
}

/// One step of a run, as the Steps list shows it (spec 3.3 §12).
///
/// A step that paused and later completed is one row, addressed by its completed
/// checkpoint; "Step n" is a label, never an address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStep {
    /// The step's label.
    pub step: usize,
    /// "Step 1".
    pub label: String,
    /// How the step ended.
    pub state: StepState,
    /// When it started.
    pub at: DateTime<Utc>,
    /// That time as the home shows it: `09:14:01`.
    pub at_text: String,
    /// The checkpoint the step detail and a fork read: the completed checkpoint of the step,
    /// else the one its pause or failure wrote. Absent for the input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,
    /// The step's nodes.
    pub nodes: Vec<NodeRun>,
    /// The keys the step wrote, sorted.
    pub wrote: Vec<String>,
    /// "Your message" for the input, "nothing written yet" for a step that waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Whether a fork may start here: a completed step with work after it.
    pub forkable: bool,
}

/// Where a forked thread came from (spec 3.3 §12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunLineage {
    /// The thread the family of forks started from.
    pub root: String,
    /// The thread this one was forked from.
    pub parent: String,
    /// The session of `parent`, when it is the caller's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    /// The checkpoint of `parent` it was forked at.
    pub parent_checkpoint: String,
    /// That checkpoint's step label.
    pub parent_step: usize,
    /// The keys the fork overwrote.
    pub edited: Vec<String>,
    /// Whether the edits were checked against the graph's channels.
    pub checked: bool,
    /// "Fork of the 09:14 run at step 1 · edited note".
    pub text: String,
}

/// A fork of a run, as "Forks of this run" lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ForkSummary {
    /// The fork's session.
    pub session_id: String,
    /// When the fork started, or was made.
    pub at: DateTime<Utc>,
    /// `09:31`.
    pub at_text: String,
    /// The step it was forked at.
    pub parent_step: usize,
    /// The keys it overwrote.
    pub edited: Vec<String>,
    /// "from step 1 · edited note".
    pub text: String,
}

/// A run of a session's graph, as the Runs tab shows it (spec 3.3 §12).
///
/// Built from the checkpoints' run and step records, the effect log and the inbox; never
/// from agent output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    /// The session.
    pub session_id: String,
    /// The session's checkpoint thread.
    pub thread_id: String,
    /// The graph agent.
    pub graph: String,
    /// The run's id: the invocation that started it. Absent before the first run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// How the run stands.
    pub status: RunStatus,
    /// In words: "Waiting for you", "Done", …
    pub status_text: String,
    /// When the run started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// From the start to its latest checkpoint, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// "Started 09:14 · 4.2 s · 3 steps", or what there is to say before a run.
    pub summary: String,
    /// Every node of the graph, as it stands in this run.
    pub nodes: Vec<NodeRun>,
    /// The edges the run took.
    pub taken: Vec<EdgeView>,
    /// The run's steps, oldest first.
    pub steps: Vec<RunStep>,
    /// Where the thread was forked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<RunLineage>,
    /// The forks made from this thread, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forks: Vec<ForkSummary>,
    /// The first step still kept, when retention removed the ones before it: "Steps before 12
    /// were removed".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_before: Option<usize>,
    /// The error a failed run ended with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The question a paused node asked: "Which month should the invoice cover?".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
}

/// One run of a session, as `GET …/runs` lists it, from its records only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    /// The run's id.
    pub run_id: String,
    /// When it started.
    pub started_at: DateTime<Utc>,
    /// `09:14`.
    pub started_text: String,
    /// Its steps, oldest first.
    pub steps: Vec<RunStep>,
}

/// `GET /surface/workflow/sessions/{session}/runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunsResponse {
    /// The session.
    pub session_id: String,
    /// The graph agent.
    pub graph: String,
    /// The session's checkpoint thread.
    pub thread_id: String,
    /// Every run whose records are kept, oldest first.
    pub runs: Vec<RunSummary>,
    /// Where the thread was forked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<RunLineage>,
    /// The first step still kept, when retention removed the ones before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_before: Option<usize>,
}

/// `GET /surface/workflow/graphs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphsResponse {
    /// Every graph agent the home observes.
    pub graphs: Vec<GraphView>,
}

/// How a state key changed in a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Change {
    /// The key was not in the state before.
    Added,
    /// The key's value changed.
    Changed,
    /// The key is not in the state after.
    Removed,
}

/// One key of "What changed" (spec 3.3 §12): computed here so the web pane and the terminal
/// show the same diff. Values pass secret redaction, the host's hidden keys and a size cap.
///
/// # Example
///
/// ```rust
/// use adk_surface::workflow::{Change, StateChange};
///
/// let change = StateChange {
///     key: "invoice".into(),
///     change: Change::Added,
///     change_text: "Added".into(),
///     before: None,
///     after: Some(serde_json::json!({ "amountCents": 4200 })),
///     note: None,
/// };
/// assert_eq!(serde_json::to_value(&change).unwrap()["change"], "added");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StateChange {
    /// The key.
    pub key: String,
    /// How it changed.
    pub change: Change,
    /// "Added", "Changed" or "Removed".
    pub change_text: String,
    /// The value before; absent when added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    /// The value after; absent when removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
    /// Why a value is not shown as it is: "Hidden: looks like a secret".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One channel's value at a checkpoint, for the fork dialog's editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelValue {
    /// The channel.
    pub key: String,
    /// Its value, shown as every value is; `null` when the state does not hold it.
    pub value: Value,
    /// Why the value is not shown as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Whether a fork may edit it here: a value too large to show in full may not. A hidden
    /// or redacted part of an edited value keeps the value it stands for.
    pub editable: bool,
}

/// A governed call that already happened, from the effect log (spec 3.3 §12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffectView {
    /// The governed node.
    pub node: String,
    /// The call in short: `POST 127.0.0.1/charges`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// When it happened.
    pub at: DateTime<Utc>,
    /// `09:14`.
    pub at_text: String,
    /// The keys it wrote.
    pub writes: Vec<String>,
    /// "pay — POST 127.0.0.1/charges at 09:14".
    pub text: String,
}

/// The decision a governed node waited on, from the inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeDecision {
    /// The decision's id.
    pub id: String,
    /// Whether it still waits.
    pub waiting: bool,
    /// "Asked at 09:14", or who answered: "You said yes from the home at 09:15".
    pub text: String,
}

/// One node of a step detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeDetail {
    /// What became of the node.
    pub run: NodeRun,
    /// "Model step", "HTTP call", "Function".
    pub kind_text: String,
    /// A model step: its model and tool activity is in the session's thread.
    pub model_step: bool,
    /// For a replayed call, the record that answered it: "Not sent again — the record from
    /// 09:14 answered it".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<EffectView>,
    /// The decision a governed node waited on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<NodeDecision>,
    /// A line under the node: "Not sent again — the record from 09:14 answered it".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// What a fork from a step would do, for the fork dialog (spec 3.3 §12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ForkPoint {
    /// Whether a fork may start here.
    pub allowed: bool,
    /// Why not: "Nothing runs after the last step".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// The nodes that run again: `pay`, `receipt`.
    pub runs_again: Vec<String>,
    /// Governed calls after this step that already happened and are answered from their
    /// record unless the fork changes them.
    pub already_happened: Vec<EffectView>,
    /// What becomes of a changed governed call, when one runs again: "A changed call waits
    /// for you." when its node asks first, else that the host's rules decide it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_calls: Option<String>,
}

/// `GET /surface/workflow/sessions/{session}/checkpoints/{id}`: one step, from one
/// checkpoint (spec 3.3 §12). Reading it changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StepDetail {
    /// The session.
    pub session_id: String,
    /// The checkpoint.
    pub checkpoint_id: String,
    /// The step's label.
    pub step: usize,
    /// "Step 2".
    pub label: String,
    /// How the step ended.
    pub state: StepState,
    /// When it started: `09:14:03`.
    pub at_text: String,
    /// The step's nodes.
    pub nodes: Vec<NodeDetail>,
    /// What changed, by key.
    pub changes: Vec<StateChange>,
    /// The state at this checkpoint, one row per channel.
    pub state_values: Vec<ChannelValue>,
    /// What a fork from here would do.
    pub fork: ForkPoint,
}

/// `POST /surface/workflow/sessions/{session}/fork` (spec 3.3 §12).
///
/// # Example
///
/// ```rust
/// use adk_surface::workflow::ForkRequest;
///
/// let request: ForkRequest = serde_json::from_value(serde_json::json!({
///     "from": "c1",
///     "set": { "note": "re-run for audit" },
///     "start": true,
/// }))
/// .unwrap();
/// assert!(request.start);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ForkRequest {
    /// The completed checkpoint of the step to fork from.
    pub from: String,
    /// Channels to overwrite in the fork, whole.
    #[serde(default)]
    pub set: BTreeMap<String, Value>,
    /// Start the fork's run now.
    #[serde(default)]
    pub start: bool,
}

/// What a fork did (spec 3.3 §12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ForkOutcome {
    /// The fork's new session.
    pub session_id: String,
    /// Its checkpoint thread.
    pub thread_id: String,
    /// The checkpoint it was forked from.
    pub from: String,
    /// The keys it overwrote.
    pub edited: Vec<String>,
    /// Whether its run started.
    pub started: bool,
    /// "Forked. The original run is unchanged."
    pub text: String,
}
