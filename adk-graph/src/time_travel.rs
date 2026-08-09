//! Time-travel debugging for graph execution history.
//!
//! This module provides [`TimeTravelHandle`] for navigating, inspecting, and forking
//! graph execution history. It works with any [`Checkpointer`] implementation to
//! inspect past execution steps, resume from arbitrary points, and create divergent
//! execution branches.
//!
//! # Overview
//!
//! Time-travel debugging allows you to:
//! - List all checkpointed steps for a thread
//! - Resume execution from any historical step
//! - Fork a thread at a specific step into a new independent thread
//! - Replay execution between two steps, collecting intermediate states
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_graph::prelude::*;
//! use adk_graph::time_travel::{TimeTravelHandle, StepInfo};
//!
//! // Build and execute a graph
//! let graph = StateGraph::new(StateSchema::simple(&["data"]))
//!     .add_node("process", process_fn)
//!     .add_edge(START, "process")
//!     .add_edge("process", END)
//!     .compile(Some(checkpointer.clone()));
//!
//! // Get a time-travel handle for a thread
//! let handle = graph.time_travel("thread_1").unwrap();
//!
//! // List all steps
//! let steps = handle.steps().await?;
//! for step in &steps {
//!     println!("Step {}: checkpoint={}", step.step, step.checkpoint_id);
//! }
//!
//! // Fork at step 2 into a new thread
//! handle.fork_at(2, "thread_1_fork").await?;
//!
//! // Replay steps 0 through 3
//! let states = handle.state_history(0, Some(3)).await?;
//! ```

use std::sync::Arc;

use serde::Serialize;

use crate::checkpoint::Checkpointer;
use crate::error::{GraphError, Result};
use crate::graph::CompiledGraph;
use crate::node::ExecutionConfig;
use crate::state::State;

/// Information about a single execution step in the graph history.
///
/// Each `StepInfo` corresponds to a checkpoint saved after a super-step
/// in the Pregel execution model. It provides metadata useful for
/// navigating and understanding the execution timeline.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::time_travel::StepInfo;
///
/// let step = StepInfo {
///     step: 3,
///     checkpoint_id: "abc-123".to_string(),
///     timestamp: Some("2025-01-15T10:30:00Z".to_string()),
///     pending_nodes: vec!["transform".to_string()],
///     state_keys: vec!["data".to_string(), "result".to_string()],
/// };
///
/// println!("Step {} has {} pending nodes", step.step, step.pending_nodes.len());
/// ```
#[derive(Debug, Clone, Serialize)]
pub struct StepInfo {
    /// The step number in the execution sequence (0-indexed).
    pub step: usize,
    /// The unique checkpoint identifier for this step.
    pub checkpoint_id: String,
    /// ISO 8601 timestamp when this step was checkpointed, if available.
    pub timestamp: Option<String>,
    /// Node names that were pending execution at this step.
    pub pending_nodes: Vec<String>,
    /// State keys that were present at this step.
    pub state_keys: Vec<String>,
}

/// Handle for time-travel operations on a graph thread.
///
/// `TimeTravelHandle` provides methods to navigate, replay, and fork
/// graph execution history. It holds a reference to the compiled graph
/// and uses the associated checkpointer to access historical state.
///
/// Create a handle via [`CompiledGraph::time_travel`].
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::prelude::*;
/// use adk_graph::time_travel::TimeTravelHandle;
///
/// let graph = StateGraph::new(StateSchema::simple(&["count"]))
///     .add_node("increment", increment_fn)
///     .add_edge(START, "increment")
///     .add_edge("increment", END)
///     .compile(Some(checkpointer.clone()));
///
/// // Create a time-travel handle
/// let handle = graph.time_travel("my_thread");
///
/// // Inspect execution history
/// let steps = handle.steps().await?;
/// println!("Thread has {} steps", steps.len());
///
/// // Fork at step 1 to explore an alternative path
/// handle.fork_at(1, "alternative_thread").await?;
/// ```
pub struct TimeTravelHandle<'g> {
    /// Reference to the compiled graph for re-execution in `resume_from`.
    pub(crate) graph: &'g CompiledGraph,
    /// The thread identifier whose history is being navigated.
    pub(crate) thread_id: String,
    /// The checkpointer used to load and save checkpoints.
    pub(crate) checkpointer: Arc<dyn Checkpointer>,
}

impl<'g> TimeTravelHandle<'g> {
    /// Create a new `TimeTravelHandle` for the given thread.
    ///
    /// # Arguments
    ///
    /// * `graph` - Reference to the compiled graph
    /// * `thread_id` - The thread whose history to navigate
    /// * `checkpointer` - The checkpointer for accessing historical state
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_graph::time_travel::TimeTravelHandle;
    ///
    /// let handle = TimeTravelHandle::new(&graph, "thread_1", checkpointer.clone());
    /// ```
    pub fn new(
        graph: &'g CompiledGraph,
        thread_id: &str,
        checkpointer: Arc<dyn Checkpointer>,
    ) -> Self {
        Self { graph, thread_id: thread_id.to_string(), checkpointer }
    }

    /// List all checkpointed steps for this thread, ordered by step number.
    ///
    /// Returns a [`StepInfo`] for each checkpoint, providing metadata about
    /// the execution state at that point in time.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] if the checkpointer fails to list checkpoints.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let handle = graph.time_travel("thread_1").unwrap();
    /// let steps = handle.steps().await?;
    ///
    /// for step in &steps {
    ///     println!(
    ///         "Step {}: {} (pending: {:?})",
    ///         step.step, step.checkpoint_id, step.pending_nodes
    ///     );
    /// }
    /// ```
    pub async fn steps(&self) -> Result<Vec<StepInfo>> {
        let checkpoints = self.checkpointer.list(&self.thread_id).await?;
        let mut steps: Vec<StepInfo> = checkpoints
            .into_iter()
            .map(|cp| StepInfo {
                step: cp.step,
                checkpoint_id: cp.checkpoint_id,
                timestamp: Some(cp.created_at.to_rfc3339()),
                pending_nodes: cp.pending_nodes,
                state_keys: cp.state.keys().cloned().collect(),
            })
            .collect();
        steps.sort_by_key(|s| s.step);
        Ok(steps)
    }

    /// Resume graph execution from the checkpoint at the specified step.
    ///
    /// Loads the state from the checkpoint at `step` and re-executes the graph
    /// from that point forward using the provided execution configuration.
    ///
    /// # Arguments
    ///
    /// * `step` - The step number to resume from
    /// * `config` - Execution configuration for the resumed run
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] if:
    /// - No checkpoint exists at the specified step
    /// - The graph execution fails after resuming
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let handle = graph.time_travel("thread_1").unwrap();
    ///
    /// // Resume from step 3
    /// let config = ExecutionConfig::new("thread_1");
    /// let final_state = handle.resume_from(3, config).await?;
    /// println!("Resumed execution produced: {:?}", final_state);
    /// ```
    pub async fn resume_from(&self, step: usize, config: ExecutionConfig) -> Result<State> {
        // List all checkpoints for this thread
        let checkpoints = self.checkpointer.list(&self.thread_id).await?;

        // Find the checkpoint at the specified step
        let checkpoint = checkpoints.into_iter().find(|cp| cp.step == step).ok_or_else(|| {
            GraphError::CheckpointError(format!(
                "no checkpoint found at step {step} for thread '{}'",
                self.thread_id
            ))
        })?;

        // Create an ExecutionConfig that resumes from this checkpoint
        let resume_config = ExecutionConfig::new(&config.thread_id)
            .with_resume_from(&checkpoint.checkpoint_id)
            .with_recursion_limit(config.recursion_limit);

        // Invoke the graph with the checkpoint's state — the executor will
        // restore state from the checkpoint and continue execution from there
        self.graph.invoke(State::new(), resume_config).await
    }

    /// Fork the thread at a specific step into a new independent thread.
    ///
    /// Loads the checkpoint at `step` and saves it under `new_thread_id`,
    /// creating a new execution branch. The original thread remains unchanged.
    ///
    /// # Arguments
    ///
    /// * `step` - The step number to fork from
    /// * `new_thread_id` - The identifier for the new forked thread
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] if:
    /// - No checkpoint exists at the specified step
    /// - The checkpointer fails to save the forked checkpoint
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let handle = graph.time_travel("thread_1").unwrap();
    ///
    /// // Fork at step 2 to explore an alternative execution path
    /// handle.fork_at(2, "thread_1_experiment").await?;
    ///
    /// // The original thread_1 is unaffected
    /// let original_steps = handle.steps().await?;
    /// ```
    pub async fn fork_at(&self, step: usize, new_thread_id: &str) -> Result<()> {
        // List all checkpoints for this thread
        let checkpoints = self.checkpointer.list(&self.thread_id).await?;

        // Find the checkpoint at the specified step
        let checkpoint = checkpoints.into_iter().find(|cp| cp.step == step).ok_or_else(|| {
            GraphError::CheckpointError(format!(
                "no checkpoint found at step {step} for thread '{}'",
                self.thread_id
            ))
        })?;

        // Clone the checkpoint under the new thread_id
        let forked = crate::state::Checkpoint::new(
            new_thread_id,
            checkpoint.state,
            checkpoint.step,
            checkpoint.pending_nodes,
        );

        // Save the forked checkpoint
        self.checkpointer.save(&forked).await?;
        Ok(())
    }

    /// Returns the stored state at each checkpointed step in a range.
    ///
    /// This **reads** checkpoints; it does not execute anything. No node runs, no
    /// event is regenerated, and no side effect is repeated — the states returned are
    /// the snapshots that were saved when the graph originally ran. Use it to inspect
    /// how state evolved, not to reproduce a decision.
    ///
    /// To re-run from a point in history, fork that checkpoint with `fork_at`
    /// and invoke the graph on the forked thread.
    ///
    /// # Arguments
    ///
    /// * `from_step` - First step to include
    /// * `to_step` - Last step to include, or `None` for the last checkpointed step
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] when no checkpoint exists at `from_step`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let handle = graph.time_travel("thread_1").unwrap();
    ///
    /// // The stored state at steps 1 through 4
    /// let transitions = handle.state_history(1, Some(4)).await?;
    /// for (step, state) in &transitions {
    ///     println!("Step {}: {:?}", step, state.keys().collect::<Vec<_>>());
    /// }
    ///
    /// // From step 2 to the last checkpoint
    /// let all_remaining = handle.state_history(2, None).await?;
    /// ```
    pub async fn state_history(
        &self,
        from_step: usize,
        to_step: Option<usize>,
    ) -> Result<Vec<(usize, State)>> {
        // List all checkpoints for this thread
        let mut checkpoints = self.checkpointer.list(&self.thread_id).await?;

        // Sort by step to ensure correct ordering
        checkpoints.sort_by_key(|cp| cp.step);

        // Filter to checkpoints in the requested range
        let results: Vec<(usize, State)> = checkpoints
            .into_iter()
            .filter(|cp| cp.step >= from_step && to_step.is_none_or(|end| cp.step <= end))
            .map(|cp| (cp.step, cp.state))
            .collect();

        // Verify that from_step exists in the results
        if results.is_empty() || results[0].0 != from_step {
            return Err(GraphError::CheckpointError(format!(
                "no checkpoint found at step {from_step} for thread '{}'",
                self.thread_id
            )));
        }

        Ok(results)
    }
}

impl CompiledGraph {
    /// Create a [`TimeTravelHandle`] for navigating the execution history of a thread.
    ///
    /// Requires that the graph was compiled with a checkpointer. If no checkpointer
    /// is configured, this method panics.
    ///
    /// # Arguments
    ///
    /// * `thread_id` - The thread identifier whose history to navigate
    ///
    /// # Panics
    ///
    /// Panics if the graph was compiled without a checkpointer.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_graph::prelude::*;
    ///
    /// let checkpointer = Arc::new(MemoryCheckpointer::new());
    /// let graph = StateGraph::new(StateSchema::simple(&["data"]))
    ///     .add_node("process", process_fn)
    ///     .add_edge(START, "process")
    ///     .add_edge("process", END)
    ///     .compile(Some(checkpointer));
    ///
    /// let handle = graph.time_travel("thread_1").unwrap();
    /// let steps = handle.steps().await?;
    /// ```
    /// # Errors
    ///
    /// Returns [`GraphError::CheckpointError`] when the graph has no
    /// checkpointer, because every operation on the handle reads checkpoints.
    pub fn time_travel(&self, thread_id: &str) -> Result<TimeTravelHandle<'_>> {
        let checkpointer = self.checkpointer.clone().ok_or_else(|| {
            GraphError::CheckpointError(
                "time travel needs a checkpointer. Add one with with_checkpointer before calling time_travel"
                    .to_string(),
            )
        })?;
        Ok(TimeTravelHandle::new(self, thread_id, checkpointer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::MemoryCheckpointer;
    use crate::graph::StateGraph;
    use crate::node::{ExecutionConfig, FunctionNode, NodeContext, NodeOutput};
    use crate::state::{Checkpoint, State, StateSchema};
    use serde_json::json;

    /// Helper: build a simple graph with a checkpointer for testing.
    fn build_test_graph() -> (CompiledGraph, Arc<MemoryCheckpointer>) {
        let checkpointer = Arc::new(MemoryCheckpointer::new());

        let node = FunctionNode::new("increment", |ctx: NodeContext| {
            let count = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            Box::pin(async move { Ok(NodeOutput::new().with_update("count", json!(count + 1))) })
        });

        let graph = StateGraph::new(StateSchema::simple(&["count"]))
            .add_node(node)
            .add_edge("__start__", "increment")
            .add_edge("increment", "__end__")
            .compile()
            .unwrap()
            .with_checkpointer_arc(checkpointer.clone());

        (graph, checkpointer)
    }

    /// Helper: seed checkpoints for a thread.
    async fn seed_checkpoints(checkpointer: &MemoryCheckpointer, thread_id: &str, count: usize) {
        for step in 0..count {
            let mut state = State::new();
            state.insert("count".to_string(), json!(step));
            let cp = Checkpoint::new(thread_id, state, step, vec!["increment".to_string()]);
            checkpointer.save(&cp).await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_fork_at_creates_new_thread() {
        let (graph, checkpointer) = build_test_graph();
        seed_checkpoints(&checkpointer, "thread_1", 5).await;

        let handle = graph.time_travel("thread_1").unwrap();

        // Fork at step 2
        handle.fork_at(2, "thread_1_fork").await.unwrap();

        // Verify the forked thread has a checkpoint
        let forked_checkpoints = checkpointer.list("thread_1_fork").await.unwrap();
        assert_eq!(forked_checkpoints.len(), 1);
        assert_eq!(forked_checkpoints[0].step, 2);
        assert_eq!(forked_checkpoints[0].state.get("count"), Some(&json!(2)));

        // Verify original thread is unchanged
        let original_checkpoints = checkpointer.list("thread_1").await.unwrap();
        assert_eq!(original_checkpoints.len(), 5);
    }

    #[tokio::test]
    async fn test_fork_at_nonexistent_step_errors() {
        let (graph, checkpointer) = build_test_graph();
        seed_checkpoints(&checkpointer, "thread_1", 3).await;

        let handle = graph.time_travel("thread_1").unwrap();

        let result = handle.fork_at(99, "new_thread").await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("no checkpoint found at step 99"));
    }

    #[tokio::test]
    async fn test_state_history_returns_states_in_range() {
        let (graph, checkpointer) = build_test_graph();
        seed_checkpoints(&checkpointer, "thread_1", 5).await;

        let handle = graph.time_travel("thread_1").unwrap();

        // Replay steps 1 through 3
        let results = handle.state_history(1, Some(3)).await.unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(
            results[0],
            (1, {
                let mut s = State::new();
                s.insert("count".to_string(), json!(1));
                s
            })
        );
        assert_eq!(results[1].0, 2);
        assert_eq!(results[2].0, 3);
    }

    #[tokio::test]
    async fn test_state_history_to_end() {
        let (graph, checkpointer) = build_test_graph();
        seed_checkpoints(&checkpointer, "thread_1", 5).await;

        let handle = graph.time_travel("thread_1").unwrap();

        // Replay from step 2 to end (None)
        let results = handle.state_history(2, None).await.unwrap();
        assert_eq!(results.len(), 3); // steps 2, 3, 4
        assert_eq!(results[0].0, 2);
        assert_eq!(results[1].0, 3);
        assert_eq!(results[2].0, 4);
    }

    #[tokio::test]
    async fn test_replay_nonexistent_from_step_errors() {
        let (graph, checkpointer) = build_test_graph();
        seed_checkpoints(&checkpointer, "thread_1", 3).await;

        let handle = graph.time_travel("thread_1").unwrap();

        let result = handle.state_history(99, None).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("no checkpoint found at step 99"));
    }

    #[tokio::test]
    async fn test_resume_from_executes_graph() {
        let (graph, checkpointer) = build_test_graph();

        // Seed a checkpoint at step 0 with count=5
        let mut state = State::new();
        state.insert("count".to_string(), json!(5));
        let cp = Checkpoint::new("thread_1", state, 0, vec!["increment".to_string()]);
        checkpointer.save(&cp).await.unwrap();

        let handle = graph.time_travel("thread_1").unwrap();

        // Resume from step 0 — the graph should execute "increment" and produce count=6
        let config = ExecutionConfig::new("thread_1");
        let final_state = handle.resume_from(0, config).await.unwrap();
        assert_eq!(final_state.get("count"), Some(&json!(6)));
    }

    #[tokio::test]
    async fn test_resume_from_nonexistent_step_errors() {
        let (graph, checkpointer) = build_test_graph();
        seed_checkpoints(&checkpointer, "thread_1", 3).await;

        let handle = graph.time_travel("thread_1").unwrap();

        let config = ExecutionConfig::new("thread_1");
        let result = handle.resume_from(99, config).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("no checkpoint found at step 99"));
    }

    #[tokio::test]
    async fn test_fork_independence() {
        let (graph, checkpointer) = build_test_graph();
        seed_checkpoints(&checkpointer, "thread_1", 5).await;

        let handle = graph.time_travel("thread_1").unwrap();

        // Fork at step 2
        handle.fork_at(2, "forked").await.unwrap();

        // Modify the forked thread by adding another checkpoint
        let mut new_state = State::new();
        new_state.insert("count".to_string(), json!(100));
        let new_cp = Checkpoint::new("forked", new_state, 3, vec![]);
        checkpointer.save(&new_cp).await.unwrap();

        // Verify original thread is unchanged
        let original = checkpointer.list("thread_1").await.unwrap();
        assert_eq!(original.len(), 5);
        for cp in &original {
            assert_ne!(cp.state.get("count"), Some(&json!(100)));
        }
    }
}
