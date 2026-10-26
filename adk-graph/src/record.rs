//! Run, step and lineage records carried in checkpoint metadata (spec 3.3 §12, developer
//! track).
//!
//! Every checkpoint a run writes carries three metadata keys:
//!
//! | Key | Type | Content |
//! |-----|------|---------|
//! | [`RUN_METADATA_KEY`] | [`RunRecord`] | The run the checkpoint belongs to |
//! | [`STEPS_METADATA_KEY`] | `Vec<`[`StepRecord`]`>` | The steps finished since the previous saved checkpoint |
//! | [`LINEAGE_METADATA_KEY`] | [`Lineage`] | Where a forked thread came from; absent on a thread that was not forked |
//!
//! A run is the work that started a thread from its entry nodes. It continues across
//! pauses, approval turns and resumes; a finished thread that starts again from its entry
//! opens a new run. Node lifecycle is recorded here, durably, rather than forwarded to a
//! runner's event stream.
//!
//! A pause checkpoint has the same [`Checkpoint::step`] as the completed checkpoint before
//! it, so checkpoints are addressed by id; a step number is a label. Checkpoints written
//! before these records existed read as empty.
//!
//! # Example
//!
//! ```
//! use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
//! use adk_graph::edge::{END, START};
//! use adk_graph::graph::StateGraph;
//! use adk_graph::node::{ExecutionConfig, NodeOutput};
//! use adk_graph::record::{NodeStatus, StepKind};
//! use adk_graph::state::State;
//! use serde_json::json;
//! use std::sync::Arc;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_graph::Result<()> {
//! let checkpointer = Arc::new(MemoryCheckpointer::new());
//! let graph = StateGraph::with_channels(&["invoice"])
//!     .add_node_fn("draft", |_ctx| async {
//!         Ok(NodeOutput::new().with_update("invoice", json!({ "amountCents": 4200 })))
//!     })
//!     .add_edge(START, "draft")
//!     .add_edge("draft", END)
//!     .compile()?
//!     .with_checkpointer_arc(checkpointer.clone());
//! graph.invoke(State::new(), ExecutionConfig::new("t1")).await?;
//!
//! let last = checkpointer.load("t1").await?.expect("a checkpoint");
//! let steps = last.step_records()?;
//! assert_eq!(steps[0].kind, StepKind::Completed);
//! assert_eq!(steps[0].nodes[0].status, NodeStatus::Done);
//! assert_eq!(steps[0].nodes[0].writes, vec!["invoice".to_string()]);
//! assert!(last.run_record()?.is_some());
//! assert!(last.lineage()?.is_none());
//! # Ok(())
//! # }
//! ```

use crate::error::{GraphError, Result};
use crate::state::Checkpoint;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Checkpoint metadata key holding the [`RunRecord`].
pub const RUN_METADATA_KEY: &str = "adk.graph.run";
/// Checkpoint metadata key holding the [`StepRecord`]s finished since the previous save.
pub const STEPS_METADATA_KEY: &str = "adk.graph.steps";
/// Checkpoint metadata key holding a forked thread's [`Lineage`].
pub const LINEAGE_METADATA_KEY: &str = "adk.graph.lineage";

/// The run a checkpoint belongs to (spec 3.3 §12).
///
/// The run id is the invocation that started the thread from its entry nodes: the
/// parent invocation's id under a `Runner`, otherwise a new id.
///
/// # Example
///
/// ```
/// use adk_graph::record::RunRecord;
///
/// let run = RunRecord::new("inv-1");
/// assert_eq!(serde_json::to_value(&run)?["runId"], "inv-1");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    /// The invocation that started the run.
    pub run_id: String,
    /// When the run started, to the microsecond.
    pub started_at: DateTime<Utc>,
}

impl RunRecord {
    /// A run with id `run_id` that starts now.
    pub fn new(run_id: impl Into<String>) -> Self {
        Self { run_id: run_id.into(), started_at: now() }
    }
}

/// How a step ended (spec 3.3 §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepKind {
    /// Every node of the step finished and the run moved on.
    Completed,
    /// A node paused, or the run stopped at an interrupt gate.
    Paused,
    /// A node failed and the step did not complete.
    Failed,
}

/// What became of one node in one step (spec 3.3 §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeStatus {
    /// The node ran and finished.
    Done,
    /// The node paused, or the run stopped at its interrupt gate.
    Paused,
    /// The node failed.
    Failed,
    /// A governed node whose call already happened: its record answered it and nothing
    /// was sent again.
    Recorded,
    /// A governed node whose call was refused: by policy, by a person, or by the kill
    /// switch.
    Refused,
    /// The node did not run: the step failed before it started, or a governed node had
    /// no approval gate.
    NotRun,
}

/// One node's part in a step (spec 3.3 §12).
///
/// # Example
///
/// ```
/// use adk_graph::record::{NodeRecord, NodeStatus};
///
/// let record = NodeRecord {
///     node: "pay".to_string(),
///     started_at: chrono::Utc::now(),
///     duration_ms: 300,
///     status: NodeStatus::Done,
///     writes: vec!["charge".to_string()],
///     attempt: 1,
///     effect_key: Some("graph/urn:adk:agent:app/invoice-run/t1/pay/1/fnv1a128:00".to_string()),
///     error: None,
/// };
/// assert_eq!(serde_json::to_value(&record)?["durationMs"], 300);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRecord {
    /// The node's name.
    pub node: String,
    /// When the node started, to the microsecond.
    pub started_at: DateTime<Utc>,
    /// How long the node ran, in milliseconds.
    pub duration_ms: u64,
    /// What became of the node.
    pub status: NodeStatus,
    /// The state keys the node wrote, sorted.
    pub writes: Vec<String>,
    /// The node's attempt count when it finished; 0 when it did not start.
    pub attempt: u32,
    /// The action-ledger key of a governed node's call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_key: Option<String>,
    /// The error a failed or refused node ended with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One super-step of a run (spec 3.3 §12).
///
/// `step` is the step's label: the [`Checkpoint::step`] of the completed checkpoint the
/// step produces. A paused or failed step keeps the label it would have had.
///
/// # Example
///
/// ```
/// use adk_graph::record::{StepKind, StepRecord};
///
/// let record = StepRecord { step: 2, kind: StepKind::Paused, nodes: Vec::new() };
/// assert_eq!(serde_json::to_value(&record)?["kind"], "paused");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepRecord {
    /// The step's label.
    pub step: usize,
    /// How the step ended.
    pub kind: StepKind,
    /// The step's nodes, sorted by name.
    pub nodes: Vec<NodeRecord>,
}

/// Where a forked thread came from (spec 3.3 §12).
///
/// # Example
///
/// ```
/// use adk_graph::record::Lineage;
///
/// let lineage = Lineage {
///     root: "t1".to_string(),
///     parent: "t1".to_string(),
///     parent_checkpoint: "c1".to_string(),
///     parent_step: 1,
///     edited: vec!["note".to_string()],
///     checked: true,
/// };
/// assert_eq!(serde_json::to_value(&lineage)?["parentCheckpoint"], "c1");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lineage {
    /// The thread the whole family of forks started from.
    pub root: String,
    /// The thread this one was forked from.
    pub parent: String,
    /// The checkpoint of `parent` this thread was forked at.
    pub parent_checkpoint: String,
    /// That checkpoint's step label.
    pub parent_step: usize,
    /// The state keys the fork overwrote, sorted.
    pub edited: Vec<String>,
    /// Whether the edits were checked against the graph's declared channels.
    pub checked: bool,
}

impl Checkpoint {
    /// The run this checkpoint belongs to, or `None` for a checkpoint without one.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::SerializationError`] when the record is malformed.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::state::{Checkpoint, State};
    ///
    /// let checkpoint = Checkpoint::new("t1", State::new(), 0, vec![]);
    /// assert!(checkpoint.run_record()?.is_none());
    /// # Ok::<(), adk_graph::GraphError>(())
    /// ```
    pub fn run_record(&self) -> Result<Option<RunRecord>> {
        read_key(self, RUN_METADATA_KEY)
    }

    /// The steps finished since the previous saved checkpoint of the thread, oldest first;
    /// empty for a checkpoint without them.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::SerializationError`] when the records are malformed.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::state::{Checkpoint, State};
    ///
    /// let checkpoint = Checkpoint::new("t1", State::new(), 0, vec![]);
    /// assert!(checkpoint.step_records()?.is_empty());
    /// # Ok::<(), adk_graph::GraphError>(())
    /// ```
    pub fn step_records(&self) -> Result<Vec<StepRecord>> {
        Ok(read_key(self, STEPS_METADATA_KEY)?.unwrap_or_default())
    }

    /// Where this checkpoint's thread was forked from, or `None` for a thread that was not
    /// forked.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::SerializationError`] when the record is malformed.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::state::{Checkpoint, State};
    ///
    /// let checkpoint = Checkpoint::new("t1", State::new(), 0, vec![]);
    /// assert!(checkpoint.lineage()?.is_none());
    /// # Ok::<(), adk_graph::GraphError>(())
    /// ```
    pub fn lineage(&self) -> Result<Option<Lineage>> {
        read_key(self, LINEAGE_METADATA_KEY)
    }

    /// Whether a node of this checkpoint's frontier waits mid-step: the run stopped before
    /// a gated node, or a node paused itself.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::state::{Checkpoint, State};
    ///
    /// let completed = Checkpoint::new("t1", State::new(), 1, vec!["pay".into()]);
    /// assert!(!completed.is_paused());
    /// assert!(completed.with_cleared_interrupt("pay").is_paused());
    /// ```
    pub fn is_paused(&self) -> bool {
        self.cleared_interrupt.is_some()
            || self.metadata.contains_key(crate::executor::PAUSED_NODES_METADATA_KEY)
    }
}

/// Reads one record key, `None` when the checkpoint does not carry it.
fn read_key<T: DeserializeOwned>(checkpoint: &Checkpoint, key: &str) -> Result<Option<T>> {
    let Some(value) = checkpoint.metadata.get(key) else { return Ok(None) };
    serde_json::from_value(value.clone()).map(Some).map_err(|error| {
        GraphError::SerializationError(format!(
            "checkpoint '{}' metadata '{key}' is malformed: {error}",
            checkpoint.checkpoint_id
        ))
    })
}

/// The current time to the microsecond, the finest precision every backend stores.
pub(crate) fn now() -> DateTime<Utc> {
    chrono::SubsecRound::trunc_subsecs(Utc::now(), 6)
}
