//! Pregel-based execution engine for graphs
//!
//! Executes graphs using the Pregel model with super-steps.

#[cfg(feature = "node-cache")]
use crate::cache::compute_cache_key;
use crate::checkpoint::Durability;
use crate::deferred::{FAN_IN_METADATA_KEY, FanInSnapshot, FanInTracker};
use crate::error::{GraphError, InterruptedExecution, Result};
use crate::governed::{GovernedDispatch, GovernedRun};
use crate::graph::{CompiledGraph, Route};
use crate::interrupt::{GraphToolConfirmationPause, Interrupt};
use crate::node::{ExecutionConfig, Node, NodeContext};
use crate::record::{
    LINEAGE_METADATA_KEY, Lineage, NodeRecord, NodeStatus, RUN_METADATA_KEY, RunRecord,
    STEPS_METADATA_KEY, StepKind, StepRecord, now as record_now,
};
use crate::state::{Checkpoint, PendingWrite, State};
use crate::stream::{StreamEvent, StreamMode};
use crate::timeout::{OnTimeout, ProgressHandle, execute_with_timeout, item_timeout_budget};
use futures::stream::{self, StreamExt};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Instant;

/// Result of a super-step execution
#[derive(Default)]
pub struct SuperStepResult {
    /// Nodes that were executed
    pub executed_nodes: Vec<String>,
    /// Every pause of the step with the node it stopped, in node-name order.
    /// Empty when nothing paused.
    pub interrupts: Vec<(String, Interrupt)>,
    /// Stream events generated
    pub events: Vec<StreamEvent>,
    /// Nodes that named their own successors, keyed by node name.
    pub goto: HashMap<String, Vec<String>>,
    /// Each executed node's own updates, keyed by node name.
    ///
    /// A fan-in node records these as the arrival of the predecessor that wrote
    /// them, so an arrival never embeds the whole state.
    pub updates: HashMap<String, HashMap<String, serde_json::Value>>,
}

/// What a completed run produced, and what it asks of its caller.
#[derive(Debug, Clone)]
pub struct GraphOutcome {
    /// The final state.
    pub state: State,
    /// Nodes of the parent graph a node asked to run next, if any.
    ///
    /// Set by [`NodeOutput::with_goto_parent`](crate::node::NodeOutput::with_goto_parent).
    /// A graph that is not a subgraph has no parent, so this is ignored.
    pub goto_parent: Option<Vec<String>>,
}

/// Pregel-based executor for graphs
pub struct PregelExecutor<'a> {
    graph: &'a CompiledGraph,
    config: ExecutionConfig,
    /// ADK configuration supplied through an additive direct-graph API.
    run_config: Option<adk_core::RunConfig>,
    state: State,
    step: usize,
    pending_nodes: Vec<String>,
    /// Parent nodes a node asked to run next; see `NodeOutput::with_goto_parent`.
    goto_parent: Option<Vec<String>>,
    /// Tracks deferred nodes waiting for all upstream paths to complete.
    pending_deferred: HashMap<String, FanInTracker>,
    /// Tracks when each deferred node first entered the pending state (for fan-in timeout).
    deferred_start_times: HashMap<String, Instant>,
    /// Predecessors a `fan_in_timeout` released a node without, keyed by node. Their
    /// next arrival is ignored, so the node does not run again for an abandoned branch.
    abandoned_arrivals: HashMap<String, std::collections::HashSet<String>>,
    /// Attempts already spent per node, carried through a resume so a retry
    /// budget is not restarted.
    attempts: HashMap<String, u32>,
    /// Outputs of children invoked imperatively, keyed by child path. Shared with
    /// every node's invoker so a resumed parent serves finished children from it.
    child_ledger: Arc<std::sync::Mutex<HashMap<String, serde_json::Value>>>,
    /// The node whose static interrupt this run has already answered.
    ///
    /// Restored from the checkpoint on resume and cleared once that node has
    /// executed, so the gate re-arms for a later arrival through a cycle.
    cleared_interrupt: Option<String>,
    /// Whether a thread whose last run finished starts again from its entry
    /// nodes instead of returning the finished state.
    restart_completed: bool,
    /// The step this run started at; the recursion limit counts from here.
    run_start_step: usize,
    /// The highest checkpoint version this run knows of for its thread; its next
    /// checkpoint is one higher.
    head_version: u64,
    /// Writes of the nodes that finished in a super-step that has not completed.
    pending_writes: BTreeMap<String, PendingWrite>,
    /// A checkpoint being written in the background under `Durability::Async`.
    inflight: Option<tokio::task::JoinHandle<Result<String>>>,
    /// Whether a completed super-step has not been checkpointed yet.
    unsaved: bool,
    /// Nodes that paused with a dynamic interrupt and have not run since.
    ///
    /// Restored from the checkpoint on resume, so the step that resumes them
    /// hands each its answer from `ExecutionConfig::resume_values`.
    paused_nodes: BTreeSet<String>,
    /// The run the next checkpoint belongs to; opened when the run starts from its
    /// entry nodes and restored on resume.
    run: Option<RunRecord>,
    /// Where this thread was forked from, if it was.
    lineage: Option<Lineage>,
    /// Steps finished since the last checkpoint was written.
    unsaved_steps: Vec<StepRecord>,
    /// Completed runs of each governed node in this thread's lineage.
    occurrences: BTreeMap<String, u32>,
    /// The request each governed node paused with, while it waits for a decision.
    held: BTreeMap<String, adk_core::ToolConfirmationRequest>,
    /// The id the next checkpoint is written under, known in advance so a governed
    /// node's effect-log entry can name the checkpoint that records its step.
    next_checkpoint_id: String,
}

/// Checkpoint metadata naming the nodes a dynamic pause stopped.
pub(crate) const PAUSED_NODES_METADATA_KEY: &str = "adk.graph.paused_nodes";
/// Checkpoint metadata counting each governed node's completed runs in the lineage.
pub(crate) const OCCURRENCES_METADATA_KEY: &str = "adk.graph.occurrences";
/// Checkpoint metadata holding the request each waiting governed node paused with.
const HELD_METADATA_KEY: &str = "adk.graph.held";
/// Every metadata key a checkpoint's revision by `update_state` carries over.
const CARRIED_METADATA_KEYS: [&str; 7] = [
    FAN_IN_METADATA_KEY,
    PAUSED_NODES_METADATA_KEY,
    RUN_METADATA_KEY,
    STEPS_METADATA_KEY,
    LINEAGE_METADATA_KEY,
    OCCURRENCES_METADATA_KEY,
    HELD_METADATA_KEY,
];

/// One node's dispatch within a super-step, as its future reports it.
struct NodeRun {
    name: String,
    /// `None` when the node did not start, because the step had already failed.
    output: Option<Result<crate::node::NodeOutput>>,
    started_at: chrono::DateTime<chrono::Utc>,
    duration_ms: u64,
    step: usize,
    attempts: u32,
    /// The governed bracket's outcome, for a governed node.
    governed: Option<GovernedRun>,
}

impl<'a> PregelExecutor<'a> {
    /// Create a new executor
    pub fn new(graph: &'a CompiledGraph, config: ExecutionConfig) -> Self {
        Self::new_with_run_config(graph, config, None)
    }

    pub(crate) fn new_with_run_config(
        graph: &'a CompiledGraph,
        config: ExecutionConfig,
        run_config: Option<adk_core::RunConfig>,
    ) -> Self {
        Self {
            graph,
            config,
            run_config,
            state: State::new(),
            step: 0,
            pending_nodes: vec![],
            goto_parent: None,
            pending_deferred: HashMap::new(),
            deferred_start_times: HashMap::new(),
            abandoned_arrivals: HashMap::new(),
            attempts: HashMap::new(),
            child_ledger: Arc::new(std::sync::Mutex::new(HashMap::new())),
            cleared_interrupt: None,
            restart_completed: false,
            run_start_step: 0,
            head_version: 0,
            pending_writes: BTreeMap::new(),
            inflight: None,
            unsaved: false,
            paused_nodes: BTreeSet::new(),
            run: None,
            lineage: None,
            unsaved_steps: Vec::new(),
            occurrences: BTreeMap::new(),
            held: BTreeMap::new(),
            next_checkpoint_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// Starts a thread whose last run finished again from its entry nodes.
    ///
    /// Used by [`GraphAgent`](crate::agent::GraphAgent), where every call is a
    /// new turn on the session's thread. The finished state carries over and the
    /// new input is merged on top; a paused thread still resumes where it paused.
    pub(crate) fn restarting_completed_threads(mut self) -> Self {
        self.restart_completed = true;
        self
    }

    /// Attempt to resume from an existing checkpoint.
    ///
    /// If a checkpoint is found (either by explicit `resume_from` ID or by latest
    /// checkpoint for the thread), restores state, pending_nodes, and step from it,
    /// then merges the provided input on top. Returns `true` if resumed.
    ///
    /// If no checkpoint is found, returns `false` so the caller can proceed with
    /// fresh-start logic.
    async fn try_resume_from_checkpoint(&mut self, input: &State) -> Result<bool> {
        let checkpoint = if let Some(checkpoint_id) = &self.config.resume_from {
            // Resume from a specific checkpoint by ID
            if let Some(cp) = self.graph.checkpointer.as_ref() {
                cp.load_by_id(checkpoint_id).await?
            } else {
                None
            }
        } else if let Some(cp) = self.graph.checkpointer.as_ref() {
            // Try to load the latest checkpoint for this thread
            cp.load(&self.config.thread_id).await?
        } else {
            None
        };

        if let Some(checkpoint) = checkpoint {
            // The lineage and each governed node's count belong to the thread, so a new
            // turn on a finished thread keeps them; the run and its waiting requests do not.
            self.lineage = checkpoint.lineage()?;
            self.occurrences =
                read_metadata(&checkpoint, OCCURRENCES_METADATA_KEY)?.unwrap_or_default();
            let run = checkpoint.run_record()?;
            let held = read_metadata(&checkpoint, HELD_METADATA_KEY)?.unwrap_or_default();
            // A fork from an earlier checkpoint continues after the thread's newest.
            self.head_version = checkpoint.version;
            if self.config.resume_from.is_some()
                && let Some(cp) = self.graph.checkpointer.as_ref()
                && let Some(newest) = cp.load(&checkpoint.thread_id).await?
            {
                self.head_version = self.head_version.max(newest.version);
            }
            let finished = checkpoint.pending_nodes.is_empty();
            self.state = checkpoint.state;
            self.step = checkpoint.step;

            if finished && self.restart_completed && self.config.resume_from.is_none() {
                // A new turn on a finished thread. The retry budget, child ledger,
                // gate marker, and fan-in arrivals belonged to the finished run,
                // so none of them carry over; only the state does.
                self.pending_nodes = self.entry_frontier()?;
                self.run_start_step = checkpoint.step;
            } else {
                self.pending_nodes = checkpoint.pending_nodes;
                self.cleared_interrupt = checkpoint.cleared_interrupt;
                self.attempts = checkpoint.attempts;
                *self.child_ledger.lock().expect("child ledger") = checkpoint.child_ledger;
                self.pending_writes = checkpoint.pending_writes;
                if let Some(snapshot) = FanInSnapshot::from_metadata(&checkpoint.metadata)? {
                    let restored = snapshot.restore(|node| self.graph.get_upstream_nodes(node));
                    self.pending_deferred = restored.trackers;
                    self.deferred_start_times = restored.started;
                    self.abandoned_arrivals = restored.abandoned;
                }
                if let Some(paused) = checkpoint.metadata.get(PAUSED_NODES_METADATA_KEY) {
                    self.paused_nodes = serde_json::from_value(paused.clone())?;
                }
                // A paused, failed or interrupted run continues; a checkpoint without a
                // run, such as a fork's first, opens one.
                self.run = run;
                self.held = held;
            }

            // Merge input on top of restored state
            for (key, value) in input {
                self.graph.schema.apply_update(&mut self.state, key, value.clone());
            }

            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Run the graph to completion
    pub async fn run(&mut self, input: State) -> Result<State> {
        // Check for existing checkpoint to resume from
        let resumed = self.try_resume_from_checkpoint(&input).await?;

        if !resumed {
            // No checkpoint found — fresh start
            self.state = self.initialize_state(input).await?;
            self.pending_nodes = self.entry_frontier()?;
        }
        self.open_run();

        // Main execution loop
        while !self.pending_nodes.is_empty() {
            // Check recursion limit
            let steps_taken = self.step.saturating_sub(self.run_start_step);
            if steps_taken >= self.recursion_limit() {
                return Err(GraphError::RecursionLimitExceeded(steps_taken));
            }

            self.flush_before_side_effects().await?;

            // Execute super-step
            let result = match self.execute_super_step().await {
                Ok(result) => result,
                Err(error) => {
                    self.checkpoint_failed_step().await;
                    return Err(error);
                }
            };

            // Handle interrupts
            if let Some((_, interrupt)) = result.interrupts.first().cloned() {
                self.pause_frontier(&result)?;
                let checkpoint_id = self.save_checkpoint().await?;
                return Err(GraphError::Interrupted(Box::new(
                    InterruptedExecution::new(
                        self.config.thread_id.clone(),
                        checkpoint_id,
                        interrupt,
                        self.state.clone(),
                        self.step,
                    )
                    .with_interrupts(
                        result.interrupts.into_iter().map(|(_, interrupt)| interrupt).collect(),
                    ),
                )));
            }

            // The gate re-arms once its node has run, so a cycle returning to
            // the same node asks again.
            if let Some(cleared) = &self.cleared_interrupt
                && result.executed_nodes.iter().any(|n| n == cleared)
            {
                self.cleared_interrupt = None;
            }

            // Advance the frontier *before* checkpointing. A checkpoint records
            // what still has to run, so saving while `pending_nodes` still holds
            // the nodes that just finished would re-execute them on resume.
            self.pending_nodes = self.advance_frontier(&result, &[])?;
            self.settle_child_ledger(&[]);
            self.paused_nodes.retain(|node| !result.executed_nodes.contains(node));
            self.step += 1;

            // An empty frontier is a terminal checkpoint: resuming it re-reads
            // the final state instead of restarting the graph.
            self.checkpoint_step().await?;

            if self.pending_nodes.is_empty() {
                break;
            }
        }

        self.finish_checkpoints().await?;
        Ok(self.state.clone())
    }

    /// Run with streaming
    pub fn run_stream(
        mut self,
        input: State,
        mode: StreamMode,
    ) -> impl futures::Stream<Item = Result<StreamEvent>> + 'a {
        async_stream::stream! {
            // Check for existing checkpoint to resume from
            let resumed = match self.try_resume_from_checkpoint(&input).await {
                Ok(r) => r,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            if resumed {
                // Emit a resumed event indicating execution was restored from checkpoint
                yield Ok(StreamEvent::resumed(self.step, self.pending_nodes.clone()));
            } else {
                // No checkpoint found — fresh start
                match self.initialize_state(input).await {
                    Ok(state) => self.state = state,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
                match self.entry_frontier() {
                    Ok(frontier) => self.pending_nodes = frontier,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
            self.open_run();

            // Stream initial state if requested
            if matches!(mode, StreamMode::Values) {
                yield Ok(StreamEvent::state(self.state.clone(), self.step));
            }

            // Main execution loop
            while !self.pending_nodes.is_empty() {
                // Check recursion limit
                let steps_taken = self.step.saturating_sub(self.run_start_step);
                if steps_taken >= self.recursion_limit() {
                    yield Err(GraphError::RecursionLimitExceeded(steps_taken));
                    return;
                }

                if let Err(error) = self.flush_before_side_effects().await {
                    yield Err(error);
                    return;
                }

                // Emit node_start events BEFORE execution (in Debug mode)
                if matches!(mode, StreamMode::Debug | StreamMode::Custom | StreamMode::Messages) {
                    for node_name in &self.pending_nodes {
                        yield Ok(StreamEvent::node_start(node_name, self.step));
                    }
                }

                // For Messages mode, stream from nodes directly
                if matches!(mode, StreamMode::Messages) {
                    let mut result = SuperStepResult::default();

                    // The same gate `execute_super_step` applies. This loop does
                    // not call it, so without this the mode ignored every gate.
                    let gated = self.gate_before(&self.pending_nodes);
                    // What became of each node, for the step's record.
                    let mut records: Vec<NodeRecord> = Vec::new();
                    if let Some(interrupt) = gated.clone() {
                        records.push(node_record(&interrupt.0, record_now(), 0, NodeStatus::Paused, None, 0));
                        result.interrupts.push(interrupt);
                    }

                    // Nodes run one after another here, each applying its updates, so
                    // a failure restores the state the step started from and keeps
                    // what finished as pending writes.
                    let step_start_state = self.state.clone();
                    let mut step_writes: BTreeMap<String, PendingWrite> = BTreeMap::new();

                    // A gate stops the step before any node runs. A node's own pause
                    // does not: the other nodes of the step still run, as they do
                    // when the step runs in parallel, and every pause is reported.
                    for node_name in self.pending_nodes.clone().iter() {
                        if gated.is_some() {
                            break;
                        }
                        if let Some(write) = self.pending_writes.get(node_name).cloned() {
                            records.push(node_record(
                                node_name,
                                record_now(),
                                0,
                                NodeStatus::Done,
                                Some(&write.updates),
                                1,
                            ));
                            result.executed_nodes.push(node_name.clone());
                            if let Some(targets) = write.goto.clone() {
                                result.goto.insert(node_name.clone(), targets);
                            }
                            if let Some(targets) = write.goto_parent.clone() {
                                self.goto_parent = Some(targets);
                            }
                            for (key, value) in &write.updates {
                                self.graph.schema.apply_update(&mut self.state, key, value.clone());
                            }
                            result.updates.insert(node_name.clone(), write.updates.clone());
                            step_writes.insert(node_name.clone(), write);
                            continue;
                        }
                        if let Some(node) = self.graph.nodes.get(node_name) {
                            let ctx = self.node_context(node_name);
                            let started_at = record_now();
                            let start = std::time::Instant::now();

                            // A governed node runs through its bracket, not as a stream.
                            if let Some(dispatch) = self.governed_dispatch(node_name, node) {
                                let run = crate::governed::dispatch(dispatch, &ctx).await;
                                let duration_ms = start.elapsed().as_millis() as u64;
                                let mut record =
                                    node_record(node_name, started_at, duration_ms, run.status, None, 1);
                                record.effect_key = run.effect_key.clone();
                                result.events.push(StreamEvent::node_end(node_name, self.step, duration_ms));
                                match run.output {
                                    Ok(output) => {
                                        if let Some(interrupt) = output.interrupt {
                                            records.push(record);
                                            if let Some(held) = run.held {
                                                self.held.insert(node_name.clone(), held);
                                            }
                                            result.interrupts.push((node_name.clone(), interrupt));
                                            continue;
                                        }
                                        record = node_record(
                                            node_name,
                                            started_at,
                                            duration_ms,
                                            run.status,
                                            Some(&output.updates),
                                            1,
                                        );
                                        record.effect_key = run.effect_key;
                                        records.push(record);
                                        self.held.remove(node_name);
                                        result.executed_nodes.push(node_name.clone());
                                        result.events.extend(output.events);
                                        if let Some(targets) = output.goto.clone() {
                                            result.goto.insert(node_name.clone(), targets);
                                        }
                                        if let Some(targets) = output.goto_parent.clone() {
                                            self.goto_parent = Some(targets);
                                        }
                                        let mut keys: Vec<String> = output.updates.keys().cloned().collect();
                                        keys.sort();
                                        self.ensure_channels_declared(node_name, keys.iter().map(String::as_str))?;
                                        for key in keys {
                                            if let Some(value) = output.updates.get(&key) {
                                                self.graph.schema.apply_update(&mut self.state, &key, value.clone());
                                            }
                                        }
                                        result.updates.insert(node_name.clone(), output.updates.clone());
                                        step_writes.insert(
                                            node_name.clone(),
                                            PendingWrite {
                                                updates: output.updates,
                                                goto: output.goto,
                                                goto_parent: output.goto_parent,
                                            },
                                        );
                                    }
                                    Err(e) => {
                                        record.error = Some(e.to_string());
                                        records.push(record);
                                        self.held.remove(node_name);
                                        self.state = step_start_state.clone();
                                        self.pending_writes = step_writes.clone();
                                        self.record_step(StepKind::Failed, std::mem::take(&mut records));
                                        self.checkpoint_failed_step().await;
                                        yield Err(e);
                                        return;
                                    }
                                }
                                continue;
                            }

                            let policy = self.graph.timeout_policy_for(node_name).cloned();

                            // The timeout policy now applies to the streamed
                            // execution itself. For a stream, "idle" means no
                            // event was produced within the idle timeout.
                            let max_attempts = match policy.as_ref().map(|p| &p.on_timeout) {
                                Some(OnTimeout::Retry { max_attempts }) => (*max_attempts).max(1),
                                _ => 1,
                            };
                            let mut collected_events = Vec::new();
                            let mut streamed_updates = Vec::new();
                            let mut streamed_goto: Option<(String, Vec<String>)> = None;
                            let mut streamed_interrupt: Option<Interrupt> = None;
                            let mut timed_out_after;
                            let mut attempt = 0;

                            loop {
                                attempt += 1;
                                collected_events.clear();
                                streamed_updates.clear();
                                timed_out_after = None;
                                begin_attempt(&ctx);
                                let attempt_start = std::time::Instant::now();
                                let mut node_stream = node.execute_stream(&ctx);
                                let mut failure = None;

                                loop {
                                    let budget = policy
                                        .as_ref()
                                        .and_then(|p| item_timeout_budget(p, attempt_start.elapsed()));
                                    let item = match budget {
                                        Some(budget) => {
                                            match tokio::time::timeout(budget, node_stream.next()).await {
                                                Ok(item) => item,
                                                Err(_) => {
                                                    timed_out_after = Some(attempt_start.elapsed());
                                                    break;
                                                }
                                            }
                                        }
                                        None => node_stream.next().await,
                                    };

                                    match item {
                                        Some(Ok(event)) => {
                                            // Yield Message events immediately
                                            if matches!(event, StreamEvent::Message { .. }) {
                                                yield Ok(event.clone());
                                            }
                                            // The node reports its state updates on the
                                            // stream, so they are taken from the single
                                            // execution that produced these events.
                                            if let StreamEvent::Updates { ref updates, .. } = event {
                                                streamed_updates.push(updates.clone());
                                            }
                                            // A node that routed itself reports it here.
                                            if let StreamEvent::RouteDispatched {
                                                ref source,
                                                ref targets,
                                            } = event
                                            {
                                                streamed_goto =
                                                    Some((source.clone(), targets.clone()));
                                            }
                                            // As does a node asking to pause.
                                            if let StreamEvent::NodeInterrupt {
                                                ref message,
                                                ref data,
                                                ..
                                            } = event
                                            {
                                                streamed_interrupt =
                                                    Some(Interrupt::Dynamic {
                                                        message: message.clone(),
                                                        data: data.clone(),
                                                    });
                                            }
                                            collected_events.push(event);
                                        }
                                        Some(Err(e)) => {
                                            failure = Some(e);
                                            break;
                                        }
                                        None => break,
                                    }
                                }
                                drop(node_stream);

                                if let Some(e) = failure {
                                    let mut record = node_record(
                                        node_name,
                                        started_at,
                                        start.elapsed().as_millis() as u64,
                                        NodeStatus::Failed,
                                        None,
                                        u32::try_from(attempt).unwrap_or(u32::MAX),
                                    );
                                    record.error = Some(e.to_string());
                                    records.push(record);
                                    self.state = step_start_state.clone();
                                    self.pending_writes = step_writes.clone();
                                    self.record_step(StepKind::Failed, std::mem::take(&mut records));
                                    self.checkpoint_failed_step().await;
                                    yield Err(e);
                                    return;
                                }
                                if timed_out_after.is_none() || attempt >= max_attempts {
                                    break;
                                }
                            }

                            if let Some(elapsed) = timed_out_after {
                                let on_timeout =
                                    policy.as_ref().map(|p| p.on_timeout.clone()).unwrap_or_default();
                                match on_timeout {
                                    OnTimeout::Skip => {
                                        tracing::warn!(
                                            node = %node_name,
                                            elapsed = ?elapsed,
                                            "node timed out while streaming, skipping"
                                        );
                                        streamed_updates.clear();
                                    }
                                    OnTimeout::Fail | OnTimeout::Retry { .. } => {
                                        let error = GraphError::NodeTimedOut {
                                            node: node_name.clone(),
                                            elapsed,
                                        };
                                        let mut record = node_record(
                                            node_name,
                                            started_at,
                                            start.elapsed().as_millis() as u64,
                                            NodeStatus::Failed,
                                            None,
                                            u32::try_from(attempt).unwrap_or(u32::MAX),
                                        );
                                        record.error = Some(error.to_string());
                                        records.push(record);
                                        self.state = step_start_state.clone();
                                        self.pending_writes = step_writes.clone();
                                        self.record_step(StepKind::Failed, std::mem::take(&mut records));
                                        self.checkpoint_failed_step().await;
                                        yield Err(error);
                                        return;
                                    }
                                }
                            }

                            let duration_ms = start.elapsed().as_millis() as u64;
                            result.events.push(StreamEvent::node_end(node_name, self.step, duration_ms));
                            result.events.extend(collected_events);

                            let node_interrupt = streamed_interrupt;
                            if let Some(interrupt) = node_interrupt {
                                records.push(node_record(
                                    node_name,
                                    started_at,
                                    duration_ms,
                                    NodeStatus::Paused,
                                    None,
                                    u32::try_from(attempt).unwrap_or(u32::MAX),
                                ));
                                result.interrupts.push((node_name.clone(), interrupt));
                            } else {
                                result.executed_nodes.push(node_name.clone());
                                let mut write = PendingWrite::default();
                                if let Some((source, targets)) = streamed_goto {
                                    write.goto = Some(targets.clone());
                                    result.goto.insert(source, targets);
                                }
                                for updates in &streamed_updates {
                                    write.updates.extend(updates.clone());
                                }
                                records.push(node_record(
                                    node_name,
                                    started_at,
                                    duration_ms,
                                    NodeStatus::Done,
                                    Some(&write.updates),
                                    u32::try_from(attempt).unwrap_or(u32::MAX),
                                ));
                                result.updates.insert(node_name.clone(), write.updates.clone());
                                step_writes.insert(node_name.clone(), write);
                                for updates in streamed_updates {
                                    self.ensure_channels_declared(
                                        node_name,
                                        updates.keys().map(String::as_str),
                                    )?;
                                    for (key, value) in updates {
                                        self.graph.schema.apply_update(&mut self.state, &key, value);
                                    }
                                }
                            }
                        }
                    }

                    // Yield node_end events
                    for event in &result.events {
                        if matches!(event, StreamEvent::NodeEnd { .. }) {
                            yield Ok(event.clone());
                        }
                    }

                    // Reported in node-name order, as the parallel path reports them.
                    result.interrupts.sort_by(|left, right| left.0.cmp(&right.0));

                    // A node that arms a gate on completion stops the run here, unless
                    // a node already asked to pause itself.
                    if result.interrupts.is_empty()
                        && let Some(interrupt) = self.gate_after(&result.executed_nodes)
                    {
                        result.interrupts.push(interrupt);
                    }
                    self.count_governed_runs(&result.executed_nodes);
                    let kind =
                        if result.interrupts.is_empty() { StepKind::Completed } else { StepKind::Paused };
                    self.record_step(kind, std::mem::take(&mut records));

                    // This branch returns rather than falling through to the shared
                    // handling below, so the pause is reported here.
                    if !result.interrupts.is_empty() {
                        if let Err(error) = self.pause_frontier(&result) {
                            yield Err(error);
                            return;
                        }
                        // Persist before reporting: without this the pause cannot be
                        // resumed and the work already done is lost.
                        self.pending_writes.clear();
                        let checkpoint_id = match self.save_checkpoint().await {
                            Ok(checkpoint_id) => checkpoint_id,
                            Err(error) => {
                                yield Err(error);
                                return;
                            }
                        };
                        for event in self.pause_events(&result, &checkpoint_id) {
                            yield Ok(event);
                        }
                        return;
                    }

                    // The gate re-arms once its node has run; see `run`.
                    if let Some(cleared) = &self.cleared_interrupt
                        && result.executed_nodes.iter().any(|n| n == cleared)
                    {
                        self.cleared_interrupt = None;
                    }

                    self.pending_nodes = match self.advance_frontier(&result, &[]) {
                        Ok(nodes) => nodes,
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    };
                    self.settle_child_ledger(&[]);
                    self.paused_nodes.retain(|node| !result.executed_nodes.contains(node));
                    self.step += 1;

                    self.pending_writes.clear();

                    // The other path checkpoints every super-step. This one did not,
                    // so a run in this mode left no state to resume from and
                    // `get_state` reported nothing.
                    if let Err(e) = self.checkpoint_step().await {
                        yield Err(e);
                        return;
                    }
                    continue;
                }

                // Execute super-step (non-streaming)
                let result = match self.execute_super_step().await {
                    Ok(r) => r,
                    Err(e) => {
                        self.checkpoint_failed_step().await;
                        yield Err(e);
                        return;
                    }
                };

                // Yield events based on mode (node_end and custom events)
                for event in &result.events {
                    match (&mode, &event) {
                        // Skip node_start since we already emitted it above
                        (StreamMode::Custom | StreamMode::Debug, StreamEvent::NodeStart { .. }) => {}
                        (StreamMode::Custom, _) => yield Ok(event.clone()),
                        (StreamMode::Debug, _) => yield Ok(event.clone()),
                        _ => {}
                    }
                }

                // Yield state/updates
                match mode {
                    StreamMode::Values => {
                        yield Ok(StreamEvent::state(self.state.clone(), self.step));
                    }
                    StreamMode::Updates => {
                        yield Ok(StreamEvent::step_complete(
                            self.step,
                            result.executed_nodes.clone(),
                        ));
                    }
                    _ => {}
                }

                // Handle interrupts
                if !result.interrupts.is_empty() {
                    if let Err(error) = self.pause_frontier(&result) {
                        yield Err(error);
                        return;
                    }
                    // Persist before reporting: without this the interrupt is
                    // unresumable, because resuming loads the checkpoint for the
                    // thread.
                    let checkpoint_id = match self.save_checkpoint().await {
                        Ok(checkpoint_id) => checkpoint_id,
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    };
                    for event in self.pause_events(&result, &checkpoint_id) {
                        yield Ok(event);
                    }
                    return;
                }

                // The gate re-arms once its node has run; see `run`.
                if let Some(cleared) = &self.cleared_interrupt
                    && result.executed_nodes.iter().any(|n| n == cleared)
                {
                    self.cleared_interrupt = None;
                }

                // Advance the frontier before checkpointing, so the checkpoint
                // records what still has to run rather than what just finished.
                //
                // Reported only on the debug stream, because building it evaluates
                // each router a second time.
                if matches!(mode, StreamMode::Debug) {
                    match self.graph.route_dispatches(&result.executed_nodes, &self.state) {
                        Ok(dispatches) => {
                            for (source, targets) in dispatches {
                                yield Ok(StreamEvent::route_dispatched(&source, targets));
                            }
                        }
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                }

                self.pending_nodes = match self.advance_frontier(&result, &[]) {
                    Ok(nodes) => nodes,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };
                self.settle_child_ledger(&[]);
                self.paused_nodes.retain(|node| !result.executed_nodes.contains(node));
                self.step += 1;

                if let Err(e) = self.checkpoint_step().await {
                    yield Err(e);
                    return;
                }
            }

            if let Err(e) = self.finish_checkpoints().await {
                yield Err(e);
                return;
            }
            yield Ok(StreamEvent::done(self.state.clone(), self.step + 1));
        }
    }

    /// Computes the next frontier of a finished super-step, holding fan-in nodes
    /// that still wait for a predecessor.
    ///
    /// `in_flight` names nodes that did not finish and run again, such as the
    /// nodes a dynamic pause re-queues; a waiting fan-in node counts them as
    /// predecessors that can still run.
    ///
    /// # Errors
    ///
    /// Returns the routing error from [`Self::next_frontier`] or the fan-in error
    /// from [`Self::filter_deferred_nodes`].
    fn advance_frontier(
        &mut self,
        result: &SuperStepResult,
        in_flight: &[String],
    ) -> Result<Vec<String>> {
        let (candidates, routes) = self.next_frontier(&result.executed_nodes, &result.goto)?;
        self.filter_deferred_nodes(candidates, &routes, &result.updates, in_flight)
    }

    /// The nodes a run starts with.
    ///
    /// An entry node that is also a fan-in node, reached from `START` and from
    /// other nodes, waits like any join: `START` has arrived, and the node runs
    /// once the predecessors that can still run in this pass have, so it runs
    /// once rather than at the start and again after each predecessor. A node
    /// whose other predecessors run only after it, such as a loop's head,
    /// starts at once.
    ///
    /// # Errors
    ///
    /// Returns the fan-in error from [`Self::filter_deferred_nodes`].
    fn entry_frontier(&mut self) -> Result<Vec<String>> {
        let graph = self.graph;
        let (joins, mut ready): (Vec<String>, Vec<String>) =
            graph.get_entry_nodes().into_iter().partition(|node| {
                graph.deferred_configs.contains_key(node)
                    && !graph.get_upstream_nodes(node).is_empty()
            });
        if joins.is_empty() {
            return Ok(ready);
        }
        for join in joins {
            let upstream = graph.get_upstream_nodes(&join);
            self.pending_deferred.insert(
                join.clone(),
                FanInTracker::new(upstream.iter().map(String::as_str).collect()),
            );
            self.deferred_start_times.insert(join, Instant::now());
        }
        // The other entry nodes run next, so they count as running when deciding
        // whether a join's predecessors can still arrive.
        let released = self.filter_deferred_nodes(Vec::new(), &[], &HashMap::new(), &ready)?;
        ready.extend(released);
        Ok(ready)
    }

    /// Filter deferred nodes from the next candidates.
    ///
    /// A deferred node records an arrival from each node that routed to it in
    /// this step: the updates that predecessor wrote, or the value it wrote to a
    /// channel named after itself. A predecessor that ran but routed elsewhere
    /// does not arrive.
    ///
    /// Every waiting node is then checked, whether or not anything routed to it
    /// in this step. It is released, with its arrivals merged into
    /// `{node}_fan_in`, once no predecessor that has not arrived can still run in
    /// the current pass. A predecessor can still run when it is reachable along
    /// declared edges from a node that runs next, a node that runs again, or
    /// another waiting node, without passing through this node or a deferred node
    /// that waits on it. A predecessor reached only through this node belongs to
    /// the next pass of a loop, so a node re-entered by a back edge, or a merge
    /// after a conditional branch, runs with what arrived.
    ///
    /// When nothing else can run, every waiting node is released, so a run never
    /// ends with a join that did not fire.
    ///
    /// If a deferred node has a `fan_in_timeout` configured and the timeout has
    /// elapsed while it still waits:
    /// - If at least `min_predecessors` (default one) arrived, proceed with partial results.
    /// - Otherwise, return `GraphError::FanInTimedOut`.
    fn filter_deferred_nodes(
        &mut self,
        candidates: Vec<String>,
        routes: &[Route],
        updates: &HashMap<String, HashMap<String, serde_json::Value>>,
        in_flight: &[String],
    ) -> Result<Vec<String>> {
        let mut ready_nodes = Vec::new();

        for candidate in candidates {
            if !self.graph.deferred_configs.contains_key(&candidate) {
                ready_nodes.push(candidate);
                continue;
            }
            // A branch a timeout released this node without arrives late: that
            // arrival is dropped once, rather than running the node again for it.
            let abandoned = self.abandoned_arrivals.entry(candidate.clone()).or_default();
            let arrivals: Vec<&String> = routes
                .iter()
                .filter(|(source, target)| target == &candidate && !abandoned.remove(source))
                .map(|(source, _)| source)
                .collect();
            if arrivals.is_empty() && !self.pending_deferred.contains_key(&candidate) {
                continue;
            }
            let upstream = self.graph.get_upstream_nodes(&candidate);
            let tracker = self.pending_deferred.entry(candidate.clone()).or_insert_with(|| {
                FanInTracker::new(upstream.iter().map(String::as_str).collect())
            });
            self.deferred_start_times.entry(candidate.clone()).or_insert_with(Instant::now);
            for source in arrivals {
                tracker.record(source, arrival_output(source, updates.get(source)));
            }
        }
        self.abandoned_arrivals.retain(|_, sources| !sources.is_empty());

        if self.pending_deferred.is_empty() {
            return Ok(ready_nodes);
        }

        let graph = self.graph;
        let mut waiting: Vec<String> = self.pending_deferred.keys().cloned().collect();
        waiting.sort();
        let successors = graph.successor_map();
        let deferred_upstream: HashMap<&str, Vec<String>> = graph
            .deferred_configs
            .keys()
            .map(|node| (node.as_str(), graph.get_upstream_nodes(node)))
            .collect();

        let mut released = Vec::new();
        for join in &waiting {
            // This node, and any deferred node waiting on it, cannot run before it.
            let blocked = |node: &str| {
                node == join.as_str()
                    || deferred_upstream.get(node).is_some_and(|upstream| upstream.contains(join))
            };
            let sources = ready_nodes
                .iter()
                .chain(in_flight)
                .chain(waiting.iter())
                .map(String::as_str)
                .filter(|node| !blocked(node));
            let reachable = reachable_from(sources, &successors, &blocked);

            let tracker = &self.pending_deferred[join];
            let still_expected =
                tracker.pending_sources().into_iter().any(|source| reachable.contains(source));
            if !still_expected {
                released.push(join.clone());
                continue;
            }

            let Some(config) = graph.deferred_configs.get(join) else { continue };
            let Some(timeout) = config.fan_in_timeout else { continue };
            if self.deferred_start_times[join].elapsed() < timeout {
                continue;
            }
            let received = tracker.received_count();
            let expected = tracker.expected_count();
            // `min_predecessors` decides how many arrivals are enough to release
            // the node once the timeout expires.
            let required = config.min_predecessors.unwrap_or(1).max(1);
            if received < required {
                self.pending_deferred.remove(join);
                self.deferred_start_times.remove(join);
                return Err(GraphError::FanInTimedOut { node: join.clone(), received, expected });
            }
            tracing::warn!(
                node = %join,
                received,
                expected,
                "fan-in timeout expired, proceeding with partial results"
            );
            let abandoned: std::collections::HashSet<String> = tracker
                .pending_sources()
                .into_iter()
                .filter(|source| reachable.contains(source))
                .map(str::to_string)
                .collect();
            self.release_join(join, false)?;
            if !abandoned.is_empty() {
                self.abandoned_arrivals.insert(join.clone(), abandoned);
            }
            ready_nodes.push(join.clone());
        }

        for join in released {
            self.release_join(&join, true)?;
            ready_nodes.push(join);
        }

        // Each waiting node waits on another, and nothing else runs: release
        // them together rather than end the run with joins that never fired.
        if ready_nodes.is_empty() && in_flight.is_empty() && !self.pending_deferred.is_empty() {
            let mut stalled: Vec<String> = self.pending_deferred.keys().cloned().collect();
            stalled.sort();
            tracing::debug!(nodes = ?stalled, "releasing fan-in nodes that wait only on each other");
            for join in stalled {
                self.release_join(&join, true)?;
                ready_nodes.push(join);
            }
        }

        Ok(ready_nodes)
    }

    /// Releases a waiting fan-in node, merging its arrivals into `{node}_fan_in`.
    ///
    /// `check_minimum` applies `min_predecessors` to a node released because its
    /// remaining predecessors can no longer run; the timeout path checks its own.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::FanInIncomplete`] when fewer predecessors arrived
    /// than `min_predecessors` requires.
    fn release_join(&mut self, join: &str, check_minimum: bool) -> Result<()> {
        let Some(tracker) = self.pending_deferred.remove(join) else { return Ok(()) };
        self.deferred_start_times.remove(join);
        // A branch abandoned on an earlier firing that never arrived belongs to
        // that firing, not to this one.
        self.abandoned_arrivals.remove(join);
        let Some(config) = self.graph.deferred_configs.get(join) else { return Ok(()) };
        if check_minimum
            && !tracker.is_ready()
            && let Some(required) = config.min_predecessors
            && tracker.received_count() < required
        {
            return Err(GraphError::FanInIncomplete {
                node: join.to_string(),
                received: tracker.received_count(),
                required,
            });
        }
        let merged = tracker.merge(&config.merge_strategy);
        self.graph.schema.apply_update(&mut self.state, &format!("{join}_fan_in"), merged);
        Ok(())
    }

    /// Keeps only the child-ledger entries of nodes that will run this step again.
    ///
    /// A ledger entry lets a re-executed parent skip a child that already
    /// finished. Once a parent's step completes it never re-executes that
    /// activation, so its entries are dropped; a parent that paused keeps them
    /// for its resume.
    fn settle_child_ledger(&self, rerun: &[String]) {
        let prefixes: Vec<String> = rerun
            .iter()
            .map(|node| crate::child::ChildInvoker::activation_prefix(node, self.step))
            .collect();
        self.child_ledger
            .lock()
            .expect("child ledger")
            .retain(|path, _| prefixes.iter().any(|prefix| path.starts_with(prefix)));
    }

    /// The context a node runs with in the current step.
    ///
    /// Carries the run configuration, the graph's schema for a node that projects
    /// state, an invoker for the children the node may run, and a progress handle
    /// when the node has an idle timeout.
    fn node_context(&self, node: &str) -> NodeContext {
        let mut ctx = NodeContext::new(self.state.clone(), self.config.clone(), self.step);
        if let Some(run_config) = self.run_config.clone() {
            ctx.set_run_config(run_config);
        }
        // A node body may invoke other nodes. The invoker carries the graph's
        // nodes and the shared ledger, so a resumed parent serves children that
        // already finished. These invocations are awaited inline by the parent and
        // are deliberately outside the concurrency budget: counting them could
        // deadlock, because the parent holds its own slot while waiting.
        ctx.set_parent_schema(Arc::new(self.graph.schema.clone()));
        // A governed node runs only through its bracket, so a node body cannot invoke it.
        let children = if self.graph.governed_nodes.is_empty() {
            self.graph.nodes.clone()
        } else {
            self.graph
                .nodes
                .iter()
                .filter(|(name, _)| !self.graph.is_governed(name))
                .map(|(name, node)| (name.clone(), Arc::clone(node)))
                .collect()
        };
        ctx.set_child_invoker(Arc::new(crate::child::ChildInvoker::new(
            children,
            self.graph.retry_policies.clone(),
            Arc::clone(&self.child_ledger),
            node,
            self.step,
        )));
        if self.graph.timeout_policy_for(node).is_some_and(|policy| policy.idle_timeout.is_some()) {
            ctx.set_progress_handle(ProgressHandle::new());
        }
        // Only a node resuming its own pause gets an answer, so a node that runs
        // again later pauses again rather than reading a stale one.
        if self.paused_nodes.contains(node)
            && let Some(answer) = self.config.resume_values.get(node)
        {
            ctx.set_resume_value(answer.clone());
        }
        ctx
    }

    /// Initialize state from input and/or checkpoint
    async fn initialize_state(&self, input: State) -> Result<State> {
        // Start with schema defaults
        let mut state = self.graph.schema.initialize_state();

        // If resuming from checkpoint, load it
        if let Some(checkpoint_id) = &self.config.resume_from {
            if let Some(cp) = self.graph.checkpointer.as_ref()
                && let Some(checkpoint) = cp.load_by_id(checkpoint_id).await?
            {
                state = checkpoint.state;
            }
        } else if let Some(cp) = self.graph.checkpointer.as_ref() {
            // Try to load latest checkpoint for thread
            if let Some(checkpoint) = cp.load(&self.config.thread_id).await? {
                state = checkpoint.state;
            }
        }

        // Merge input into state
        for (key, value) in input {
            self.graph.schema.apply_update(&mut state, &key, value);
        }

        Ok(state)
    }

    /// Execute one super-step (plan -> execute -> update)
    async fn execute_super_step(&mut self) -> Result<SuperStepResult> {
        let mut result = SuperStepResult::default();

        if let Some(gate) = self.gate_before(&self.pending_nodes) {
            let record = node_record(&gate.0, record_now(), 0, NodeStatus::Paused, None, 0);
            self.record_step(StepKind::Paused, vec![record]);
            return Ok(SuperStepResult { interrupts: vec![gate], ..Default::default() });
        }

        // Collect all updates and check for errors/interrupts
        let mut all_updates = Vec::new();
        // Each successful node's output, kept as a pending write if the step fails.
        let mut step_writes: BTreeMap<String, PendingWrite> = BTreeMap::new();
        // What became of each node, for the step's record.
        let mut records: Vec<NodeRecord> = Vec::new();

        // Nodes that finished before an earlier attempt at this step failed are
        // answered from their pending writes instead of running again.
        let mut to_run: Vec<String> = Vec::new();
        for node_name in &self.pending_nodes {
            match self.pending_writes.get(node_name) {
                Some(write) => {
                    tracing::debug!(node = %node_name, "applying pending write instead of re-running node");
                    records.push(node_record(
                        node_name,
                        record_now(),
                        0,
                        NodeStatus::Done,
                        Some(&write.updates),
                        1,
                    ));
                    result.executed_nodes.push(node_name.clone());
                    if let Some(targets) = &write.goto {
                        result.goto.insert(node_name.clone(), targets.clone());
                    }
                    if let Some(targets) = &write.goto_parent {
                        self.goto_parent = Some(targets.clone());
                    }
                    all_updates.push((node_name.clone(), write.updates.clone()));
                    result.updates.insert(node_name.clone(), write.updates.clone());
                    step_writes.insert(node_name.clone(), write.clone());
                }
                None => to_run.push(node_name.clone()),
            }
        }

        // Node cache lookups. Every key is computed from the pre-step state, the
        // state each node in this step reads, and a hit's updates join the same
        // batch as computed ones, so a hit changes nothing a sibling sees and the
        // key stored on a miss is the key the next identical lookup computes.
        #[cfg(feature = "node-cache")]
        let mut miss_keys: HashMap<String, String> = HashMap::new();
        #[cfg(feature = "node-cache")]
        let mut nodes_to_execute: Vec<String> = Vec::new();
        #[cfg(feature = "node-cache")]
        for node_name in &to_run {
            // A governed node's call goes through its bracket every time, never a cache.
            let Some(cache) = self
                .graph
                .node_caches
                .get(node_name)
                .filter(|_| !self.graph.is_governed(node_name))
            else {
                nodes_to_execute.push(node_name.clone());
                continue;
            };
            let cache_key = compute_cache_key(node_name, &self.state);
            let cached_value = cache.get(&cache_key).await;
            tracing::debug!(
                node = %node_name,
                cache_hit = cached_value.is_some(),
                cache_key = %cache_key,
                "node cache lookup"
            );
            match cached_value {
                Some(serde_json::Value::Object(updates)) => {
                    result.executed_nodes.push(node_name.clone());
                    result.events.push(StreamEvent::node_end(node_name, self.step, 0));
                    let updates: HashMap<String, serde_json::Value> = updates.into_iter().collect();
                    records.push(node_record(
                        node_name,
                        record_now(),
                        0,
                        NodeStatus::Done,
                        Some(&updates),
                        0,
                    ));
                    step_writes.insert(node_name.clone(), PendingWrite::new(updates.clone()));
                    result.updates.insert(node_name.clone(), updates.clone());
                    all_updates.push((node_name.clone(), updates));
                }
                // A cached value is always the map of a node's updates; anything
                // else is unusable, so the node runs.
                Some(_) | None => {
                    miss_keys.insert(node_name.clone(), cache_key);
                    nodes_to_execute.push(node_name.clone());
                }
            }
        }

        // Determine which nodes to execute (all if cache feature is disabled).
        //
        // Sorted so that a bounded dispatch admits nodes in a fixed order rather
        // than whatever order the frontier happened to be built in.
        #[cfg(feature = "node-cache")]
        let pending_for_execution = {
            nodes_to_execute.sort();
            &nodes_to_execute
        };
        #[cfg(not(feature = "node-cache"))]
        let pending_for_execution = {
            to_run.sort();
            &to_run
        };

        // Execute all pending nodes in parallel
        let nodes: Vec<_> = pending_for_execution
            .iter()
            .filter_map(|name| self.graph.nodes.get(name).map(|n| (name.clone(), n.clone())))
            .collect();

        // Look up timeout and retry policies for each node before spawning futures
        let timeout_policies: Vec<_> =
            nodes.iter().map(|(name, _)| self.graph.timeout_policy_for(name).cloned()).collect();
        let retry_policies: Vec<_> =
            nodes.iter().map(|(name, _)| self.graph.retry_policy_for(name).cloned()).collect();
        // Attempts already spent, so a resumed run continues its budget rather
        // than starting again. adk-python does not persist this.
        let prior_attempts: Vec<u32> =
            nodes.iter().map(|(name, _)| self.attempts.get(name).copied().unwrap_or(0)).collect();
        // A governed node runs through its bracket, once per dispatch.
        let dispatches: Vec<Option<GovernedDispatch>> =
            nodes.iter().map(|(name, node)| self.governed_dispatch(name, node)).collect();

        // Set by the first failure no error handler recovers. A node not yet
        // admitted then does not start, and a node waiting to retry stops waiting,
        // so the step ends with the work that finished instead of after the longest
        // retry chain. A node already running finishes and keeps its write.
        let (failed_tx, failed_rx) = tokio::sync::watch::channel(false);
        let failed_tx = Arc::new(failed_tx);

        let futures: Vec<_> = nodes
            .into_iter()
            .zip(timeout_policies)
            .zip(retry_policies)
            .zip(prior_attempts)
            .zip(dispatches)
            .map(|(((((name, node), policy), retry), spent), dispatch)| {
                let ctx = self.node_context(&name);
                let step = self.step;
                let recoverable = self.graph.error_handler_for(&name).is_some();
                let failed_tx = Arc::clone(&failed_tx);
                let mut failed = failed_rx.clone();
                async move {
                    let started_at = record_now();
                    if *failed.borrow() {
                        return NodeRun {
                            name,
                            output: None,
                            started_at,
                            duration_ms: 0,
                            step,
                            attempts: spent,
                            governed: None,
                        };
                    }
                    let start = Instant::now();
                    if let Some(dispatch) = dispatch {
                        let mut run = crate::governed::dispatch(dispatch, &ctx).await;
                        if run.output.is_err() && !recoverable {
                            failed_tx.send_replace(true);
                        }
                        let output = std::mem::replace(&mut run.output, Ok(Default::default()));
                        return NodeRun {
                            name,
                            output: Some(output),
                            started_at,
                            duration_ms: start.elapsed().as_millis() as u64,
                            step,
                            attempts: spent + 1,
                            governed: Some(run),
                        };
                    }
                    let mut attempts = spent;
                    let output = loop {
                        begin_attempt(&ctx);
                        let result = match policy {
                            Some(ref timeout_policy) => {
                                execute_with_timeout(node.as_ref(), &ctx, timeout_policy).await
                            }
                            None => node.execute(&ctx).await,
                        };
                        attempts += 1;

                        let Err(ref error) = result else { break result };
                        let Some(ref retry) = retry else { break result };
                        if !retry.allows_another_attempt(attempts)
                            || !retry.retry_on.should_retry(error)
                            || *failed.borrow()
                        {
                            break result;
                        }

                        let delay = retry.delay_for_attempt(attempts);
                        tracing::warn!(
                            node = %name,
                            attempt = attempts,
                            max_attempts = retry.max_attempts,
                            delay_ms = delay.as_millis(),
                            error = %error,
                            "node failed, retrying after backoff"
                        );
                        tokio::select! {
                            () = tokio::time::sleep(delay) => {}
                            _ = failed.wait_for(|failed| *failed) => {
                                tracing::debug!(
                                    node = %name,
                                    "a sibling failed the step, abandoning the retry"
                                );
                                break result;
                            }
                        }
                    };
                    if output.is_err() && !recoverable {
                        failed_tx.send_replace(true);
                    }
                    NodeRun {
                        name,
                        output: Some(output),
                        started_at,
                        duration_ms: start.elapsed().as_millis() as u64,
                        step,
                        attempts,
                        governed: None,
                    }
                }
            })
            .collect();

        // Bound the dispatch. `buffer_unordered` polls futures in the order they
        // are produced, and the frontier is sorted above, so admission order does
        // not depend on which node finished first.
        let concurrency = self
            .graph
            .max_concurrency
            .map_or(pending_for_execution.len(), |limit| limit.min(pending_for_execution.len()))
            .max(1);
        let mut outputs: Vec<NodeRun> =
            stream::iter(futures).buffer_unordered(concurrency).collect().await;
        // Completion order is intentionally nondeterministic under parallelism,
        // but selecting which of several simultaneous pauses to report cannot
        // be. Keep node event order stable and make the first node name win.
        outputs.sort_by(|left, right| left.name.cmp(&right.name));

        let mut interrupts = Vec::new();
        let mut failure = None;

        for run in outputs {
            let NodeRun {
                name: node_name,
                output,
                started_at,
                duration_ms,
                step,
                attempts,
                governed,
            } = run;
            // Not started, because the step had already failed. It stays in the
            // frontier and runs when the step resumes.
            let Some(output_result) = output else {
                records.push(node_record(
                    &node_name,
                    started_at,
                    0,
                    NodeStatus::NotRun,
                    None,
                    attempts,
                ));
                continue;
            };
            // Record the budget spent, so a resumed run does not restart it. A
            // node that finally succeeded keeps no entry: its budget is spent
            // only while it is failing.
            if output_result.is_err() {
                self.attempts.insert(node_name.clone(), attempts);
            } else {
                self.attempts.remove(&node_name);
            }
            result.events.push(StreamEvent::node_end(&node_name, step, duration_ms));
            let (governed_status, effect_key, held) = match governed {
                Some(run) => (Some(run.status), run.effect_key, run.held),
                None => (None, None, None),
            };
            let mut record = |status: NodeStatus,
                              writes: Option<&HashMap<String, serde_json::Value>>,
                              error: Option<String>| {
                let mut record =
                    node_record(&node_name, started_at, duration_ms, status, writes, attempts);
                record.effect_key = effect_key.clone();
                record.error = error;
                records.push(record);
            };

            match output_result {
                Ok(output) => {
                    // Check for dynamic interrupt. Every node that paused is
                    // reported, in name order.
                    if let Some(node_interrupt) = output.interrupt {
                        record(NodeStatus::Paused, None, None);
                        if let Some(held) = held {
                            self.held.insert(node_name.clone(), held);
                        }
                        interrupts.push((node_name, node_interrupt));
                        continue;
                    }
                    record(
                        governed_status.unwrap_or(NodeStatus::Done),
                        Some(&output.updates),
                        None,
                    );
                    self.held.remove(&node_name);
                    result.executed_nodes.push(node_name.clone());

                    // Collect custom events
                    result.events.extend(output.events);

                    // Store result in cache on miss, under the key the lookup used.
                    #[cfg(feature = "node-cache")]
                    if let Some(cache_key) = miss_keys.get(&node_name)
                        && let Some(cache) = self.graph.node_caches.get(&node_name)
                    {
                        let updates_value = serde_json::to_value(&output.updates)
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                        let ttl = self.graph.cache_policies.get(&node_name).and_then(|p| p.ttl);
                        cache.set(cache_key, updates_value, ttl).await;
                    }

                    step_writes.insert(
                        node_name.clone(),
                        PendingWrite {
                            updates: output.updates.clone(),
                            goto: output.goto.clone(),
                            goto_parent: output.goto_parent.clone(),
                        },
                    );
                    // A node that named its successors overrides its declared edges.
                    if let Some(targets) = output.goto {
                        result.goto.insert(node_name.clone(), targets);
                    }
                    // A node handing control to the graph that holds this one. The
                    // run finishes normally; the caller reads this from the outcome.
                    if let Some(targets) = output.goto_parent {
                        self.goto_parent = Some(targets);
                    }

                    // Collect updates with their node, so application order can be
                    // made independent of which future resolved first.
                    result.updates.insert(node_name.clone(), output.updates.clone());
                    all_updates.push((node_name.clone(), output.updates));
                }
                Err(e) => {
                    record(
                        governed_status.unwrap_or(NodeStatus::Failed),
                        None,
                        Some(e.to_string()),
                    );
                    // A refused or failed governed call asks again from the start.
                    self.held.remove(&node_name);
                    // The retry budget is spent. A handler may record what
                    // happened and name a recovery node instead of ending the run.
                    // An interrupt never reaches here as a failure.
                    match self.graph.error_handler_for(&node_name) {
                        Some(handler) if !matches!(e, GraphError::Interrupted(_)) => {
                            let recovery = handler(&node_name, &e, &self.state)?;
                            step_writes.insert(
                                node_name.clone(),
                                PendingWrite {
                                    updates: recovery.updates.clone(),
                                    goto: recovery.goto.clone(),
                                    goto_parent: None,
                                },
                            );
                            if let Some(targets) = recovery.goto {
                                result.goto.insert(node_name.clone(), targets);
                            }
                            result.executed_nodes.push(node_name.clone());
                            result.updates.insert(node_name.clone(), recovery.updates.clone());
                            all_updates.push((node_name, recovery.updates));
                        }
                        _ => {
                            // The first failure in name order is reported; the
                            // remaining outputs are still collected as pending writes.
                            // An error that already names this node is reported as it
                            // is, so a timeout stays a timeout. An agent's failure keeps
                            // the agent's error, prefixed with this node's name when it
                            // came from a node inside this one, such as a subgraph's.
                            if failure.is_none() {
                                failure = Some(match e {
                                    GraphError::AgentFailed { node: failed, source } => {
                                        let node = if failed == node_name {
                                            failed
                                        } else {
                                            format!("{node_name}.{failed}")
                                        };
                                        GraphError::AgentFailed { node, source }
                                    }
                                    GraphError::NodeExecutionFailed {
                                        node: ref failed, ..
                                    }
                                    | GraphError::NodeTimedOut { node: ref failed, .. }
                                        if *failed == node_name =>
                                    {
                                        e
                                    }
                                    other => GraphError::NodeExecutionFailed {
                                        node: node_name,
                                        message: other.to_string(),
                                    },
                                });
                            }
                        }
                    }
                }
            }
        }

        if let Some(error) = failure {
            self.pending_writes = step_writes;
            self.record_step(StepKind::Failed, records);
            return Err(error);
        }
        self.pending_writes.clear();

        // Apply all updates atomically using reducers.
        //
        // `buffer_unordered` yields futures as they resolve, so the collected
        // order follows timing. A non-commutative reducer — `Append` builds an
        // array, so order is the result — would then give a different state for
        // the same input depending on which node finished first. Sorting by
        // (node, channel) makes the order total and timing-independent: node
        // names are unique within a graph, and a node's own updates are held in a
        // map whose iteration order is itself unspecified.
        all_updates.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (node, updates) in all_updates {
            let mut keys: Vec<_> = updates.keys().cloned().collect();
            keys.sort();
            self.ensure_channels_declared(&node, keys.iter().map(String::as_str))?;
            for key in keys {
                if let Some(value) = updates.get(&key) {
                    self.graph.schema.apply_update(&mut self.state, &key, value.clone());
                }
            }
        }

        if interrupts.is_empty()
            && let Some(gate) = self.gate_after(&result.executed_nodes)
        {
            interrupts.push(gate);
        }
        self.count_governed_runs(&result.executed_nodes);
        let kind = if interrupts.is_empty() { StepKind::Completed } else { StepKind::Paused };
        self.record_step(kind, records);
        Ok(SuperStepResult { interrupts, ..result })
    }

    /// The bracket a governed node's dispatch runs through in this step, or `None` for a
    /// node that is not governed.
    fn governed_dispatch(&self, name: &str, node: &Arc<dyn Node>) -> Option<GovernedDispatch> {
        let governed = *self.graph.governed_nodes.get(name)?;
        Some(GovernedDispatch {
            node_name: name.to_string(),
            node: Arc::clone(node),
            governed,
            // A governed node that declares nothing is treated as having an effect.
            effect: self
                .graph
                .node_effects
                .get(name)
                .copied()
                .unwrap_or(adk_core::ToolEffect::NonIdempotent),
            declaration: self
                .graph
                .node_shape(name)
                .and_then(|shape| serde_json::to_value(shape).ok())
                .unwrap_or(serde_json::Value::Null),
            root: self.lineage_root(),
            run_id: self.run.as_ref().map(|run| run.run_id.clone()).unwrap_or_default(),
            occurrence: self.occurrences.get(name).copied().unwrap_or(0) + 1,
            // Only the dispatch that resumes the node's own pause binds to its request.
            held: if self.paused_nodes.contains(name) {
                self.held.get(name).cloned()
            } else {
                None
            },
            checkpointer: self.graph.checkpointer.clone(),
            thread_id: self.config.thread_id.clone(),
            checkpoint_id: self.next_checkpoint_id.clone(),
            run_timeout: self.graph.timeout_policy_for(name).and_then(|policy| policy.run_timeout),
        })
    }

    /// The thread this thread's lineage started at: itself, unless it was forked.
    fn lineage_root(&self) -> String {
        self.lineage.as_ref().map_or_else(|| self.config.thread_id.clone(), |l| l.root.clone())
    }

    /// Counts a completed run of each governed node in `executed`, so the next run of
    /// the node in this lineage is a new call.
    fn count_governed_runs(&mut self, executed: &[String]) {
        for node in executed.iter().filter(|node| self.graph.is_governed(node)) {
            *self.occurrences.entry(node.clone()).or_insert(0) += 1;
        }
    }

    /// Records the step that is ending, labelled with the step its completed checkpoint
    /// carries.
    fn record_step(&mut self, kind: StepKind, mut nodes: Vec<NodeRecord>) {
        nodes.sort_by(|left, right| left.node.cmp(&right.node));
        self.unsaved_steps.push(StepRecord { step: self.step + 1, kind, nodes });
    }

    /// Opens a run when none continues: a fresh start, a finished thread that starts
    /// again from its entry, or a checkpoint without a run.
    fn open_run(&mut self) {
        if self.run.is_none() {
            let run_id = self.config.parent_context.as_ref().map_or_else(
                || uuid::Uuid::new_v4().to_string(),
                |parent| parent.invocation_id().to_string(),
            );
            self.run = Some(RunRecord::new(run_id));
        }
    }

    /// Save a checkpoint
    /// Returns the gate a pending node arms, if any.
    ///
    /// A node whose gate this run has already answered runs instead of
    /// interrupting again; without that a resume reaches the same conclusion and
    /// the node never executes.
    ///
    /// Shared by both execution paths. `StreamMode::Messages` runs nodes in its
    /// own loop, and when this check lived only in `execute_super_step` that mode
    /// ignored every gate.
    fn gate_before(&self, pending: &[String]) -> Option<(String, Interrupt)> {
        pending
            .iter()
            .find(|node| {
                self.graph.interrupt_before.contains(*node)
                    && self.cleared_interrupt.as_deref() != Some(node.as_str())
            })
            .map(|node| (node.clone(), Interrupt::Before(node.clone())))
    }

    /// Returns the gate an executed node arms, if any.
    fn gate_after(&self, executed: &[String]) -> Option<(String, Interrupt)> {
        executed
            .iter()
            .find(|node| self.graph.interrupt_after.contains(*node))
            .map(|node| (node.clone(), Interrupt::After(node.clone())))
    }

    /// The stream events reporting a paused step: each tool-confirmation pause,
    /// then one `Interrupted` event per pause, naming the node it stopped.
    fn pause_events(&self, result: &SuperStepResult, checkpoint_id: &str) -> Vec<StreamEvent> {
        let mut events: Vec<StreamEvent> = result
            .interrupts
            .iter()
            .filter_map(|(_, interrupt)| {
                GraphToolConfirmationPause::from_interrupt(
                    interrupt,
                    &self.config.thread_id,
                    checkpoint_id,
                )
            })
            .map(|pause| {
                StreamEvent::custom(
                    &pause.node,
                    GraphToolConfirmationPause::KIND,
                    serde_json::to_value(&pause).unwrap_or(serde_json::Value::Null),
                )
            })
            .collect();
        events.extend(
            result
                .interrupts
                .iter()
                .map(|(node, interrupt)| StreamEvent::interrupted(node, &interrupt.to_string())),
        );
        events
    }

    /// Sets the frontier a paused super-step resumes from.
    ///
    /// | Interrupt | Saved frontier |
    /// |-----------|----------------|
    /// | `Before` | The frontier that was executing; the gated node produced no updates. |
    /// | `After` | The successors of the nodes that ran; the gated node already applied its updates. |
    /// | `Dynamic` (including tool confirmation) | The successors of every node that completed, plus each node that paused. |
    ///
    /// A completed node never re-runs on resume, and its successors and `goto`
    /// targets are not lost: they are computed now, while the step's results are
    /// still at hand, and arrivals at a fan-in node are recorded for the checkpoint.
    ///
    /// A node that paused keeps its child-ledger entries, so its children that
    /// finished are not run again when it resumes; the nodes that completed drop
    /// theirs.
    ///
    /// # Errors
    ///
    /// Returns the routing error from [`Self::next_frontier`] or the fan-in error
    /// from [`Self::filter_deferred_nodes`].
    fn pause_frontier(&mut self, result: &SuperStepResult) -> Result<()> {
        let executed = &result.executed_nodes;
        let Some((_, interrupt)) = result.interrupts.first() else { return Ok(()) };
        match interrupt {
            Interrupt::Before(node) => {
                // Record the gate being answered so the resumed run executes this
                // node rather than stopping at it again.
                self.cleared_interrupt = Some(node.clone());
                return Ok(());
            }
            Interrupt::After(_) => {
                self.pending_nodes = self.advance_frontier(result, &[])?;
                self.settle_child_ledger(&[]);
            }
            Interrupt::Dynamic { .. } => {
                let paused: Vec<String> = self
                    .pending_nodes
                    .iter()
                    .filter(|node| !executed.contains(node))
                    .cloned()
                    .collect();
                let mut frontier = self.advance_frontier(result, &paused)?;
                self.settle_child_ledger(&paused);
                // The nodes that paused; the step that resumes them hands each its
                // answer.
                self.paused_nodes = paused.iter().cloned().collect();
                for node in paused {
                    if !frontier.contains(&node) {
                        frontier.push(node);
                    }
                }
                self.pending_nodes = frontier;
            }
        }
        // A gate answered earlier re-arms once its node has run, as after an
        // uninterrupted step.
        if let Some(cleared) = &self.cleared_interrupt
            && executed.iter().any(|node| node == cleared)
        {
            self.cleared_interrupt = None;
        }
        Ok(())
    }

    /// Computes the next frontier, letting a node's `goto` stand in for its edges.
    ///
    /// A node that named successors has its declared edges skipped, so a `goto`
    /// replaces an edge rather than adding to one. `END` is accepted and
    /// contributes no successor, which is how a branch stops.
    ///
    /// Returns the frontier and each `(source, target)` route taken, the routes
    /// being what a fan-in node counts as arrivals.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::UnknownRouteTarget`] when a `goto` names a node the
    /// graph does not hold.
    fn next_frontier(
        &self,
        executed: &[String],
        goto: &HashMap<String, Vec<String>>,
    ) -> Result<(Vec<String>, Vec<Route>)> {
        // A node that routed itself does not also follow its declared edges.
        let followed_edges: Vec<String> =
            executed.iter().filter(|node| !goto.contains_key(*node)).cloned().collect();
        let mut routes = self.graph.routes_from(&followed_edges, &self.state)?;
        let mut next: Vec<String> = Vec::new();
        for (_, target) in &routes {
            if !next.contains(target) {
                next.push(target.clone());
            }
        }

        // Sorted, so a multi-target goto admits its nodes in a fixed order.
        let mut routed: Vec<(&String, &Vec<String>)> = goto.iter().collect();
        routed.sort_by_key(|(node, _)| node.as_str());

        for (node, targets) in routed {
            for target in targets {
                if target == crate::edge::END {
                    continue;
                }
                if self.graph.node(target).is_none() {
                    return Err(GraphError::UnknownRouteTarget(format!(
                        "node '{node}' routed to '{target}', which is not a node in this graph"
                    )));
                }
                routes.push((node.clone(), target.clone()));
                if !next.contains(target) {
                    next.push(target.clone());
                }
            }
        }
        Ok((next, routes))
    }

    /// The run's step budget: the config's override, else the graph's own limit.
    fn recursion_limit(&self) -> usize {
        self.config.recursion_limit.unwrap_or(self.graph.recursion_limit)
    }

    /// Rejects an update naming a channel the schema does not declare.
    ///
    /// Inert unless the graph asked for enforcement, and inert when the schema
    /// declares no channels, so an existing graph is unaffected either way.
    fn ensure_channels_declared<'k>(
        &self,
        node: &str,
        keys: impl IntoIterator<Item = &'k str>,
    ) -> Result<()> {
        if !self.graph.strict_channels {
            return Ok(());
        }
        match self.graph.schema.first_undeclared(keys) {
            Some(channel) => Err(GraphError::UndeclaredChannel {
                node: node.to_string(),
                channel: channel.to_string(),
            }),
            None => Ok(()),
        }
    }

    /// Builds the checkpoint of the run as it stands, one version past the head.
    fn build_checkpoint(&mut self) -> Checkpoint {
        self.head_version += 1;
        let mut checkpoint = Checkpoint::new(
            &self.config.thread_id,
            self.state.clone(),
            self.step,
            self.pending_nodes.clone(),
        )
        .with_version(self.head_version);
        checkpoint.checkpoint_id =
            std::mem::replace(&mut self.next_checkpoint_id, uuid::Uuid::new_v4().to_string());
        checkpoint.cleared_interrupt = self.cleared_interrupt.clone();
        checkpoint.attempts = self.attempts.clone();
        checkpoint.child_ledger = self.child_ledger.lock().expect("child ledger").clone();
        checkpoint.pending_writes = self.pending_writes.clone();
        // Carried in metadata rather than a field, so every backend persists
        // it without a schema change and older checkpoints load as before.
        if let Some(snapshot) = FanInSnapshot::capture(
            &self.pending_deferred,
            &self.deferred_start_times,
            &self.abandoned_arrivals,
        ) {
            checkpoint.metadata.insert(FAN_IN_METADATA_KEY.to_string(), snapshot.to_value());
        }
        if !self.paused_nodes.is_empty() {
            checkpoint.metadata.insert(
                PAUSED_NODES_METADATA_KEY.to_string(),
                serde_json::to_value(&self.paused_nodes).unwrap_or_default(),
            );
        }
        // Run, step and lineage records (spec 3.3 §12): every step since the last write,
        // so a run under `Durability::Exit` keeps the steps it did not checkpoint.
        let steps = std::mem::take(&mut self.unsaved_steps);
        let records = [
            (RUN_METADATA_KEY, self.run.as_ref().map(serde_json::to_value)),
            (STEPS_METADATA_KEY, (!steps.is_empty()).then(|| serde_json::to_value(&steps))),
            (LINEAGE_METADATA_KEY, self.lineage.as_ref().map(serde_json::to_value)),
            (
                OCCURRENCES_METADATA_KEY,
                (!self.occurrences.is_empty()).then(|| serde_json::to_value(&self.occurrences)),
            ),
            (HELD_METADATA_KEY, (!self.held.is_empty()).then(|| serde_json::to_value(&self.held))),
        ];
        for (key, value) in records {
            if let Some(Ok(value)) = value {
                checkpoint.metadata.insert(key.to_string(), value);
            }
        }
        checkpoint
    }

    /// Writes the run's checkpoint now, after any background write finishes.
    async fn save_checkpoint(&mut self) -> Result<String> {
        self.await_inflight().await?;
        let Some(cp) = self.graph.checkpointer.clone() else { return Ok(String::new()) };
        let checkpoint = self.build_checkpoint();
        self.unsaved = false;
        write_checkpoint(cp, self.graph.retention.clone(), checkpoint).await
    }

    /// Checkpoints a completed super-step as the run's durability mode asks.
    async fn checkpoint_step(&mut self) -> Result<()> {
        let Some(cp) = self.graph.checkpointer.clone() else { return Ok(()) };
        match self.config.durability {
            Durability::Sync => {
                self.save_checkpoint().await?;
            }
            Durability::Async => {
                // One write at a time, so versions reach the store in order.
                self.await_inflight().await?;
                let checkpoint = self.build_checkpoint();
                self.unsaved = false;
                self.inflight = Some(tokio::spawn(write_checkpoint(
                    cp,
                    self.graph.retention.clone(),
                    checkpoint,
                )));
            }
            Durability::Exit => self.unsaved = true,
        }
        Ok(())
    }

    /// Waits for a background checkpoint write and reports its failure.
    async fn await_inflight(&mut self) -> Result<()> {
        if let Some(handle) = self.inflight.take() {
            handle.await.map_err(|error| {
                GraphError::CheckpointError(format!("background checkpoint write failed: {error}"))
            })??;
        }
        Ok(())
    }

    /// Makes every completed super-step durable before the run returns.
    async fn finish_checkpoints(&mut self) -> Result<()> {
        self.await_inflight().await?;
        if self.unsaved {
            self.save_checkpoint().await?;
        }
        Ok(())
    }

    /// Writes every earlier checkpoint before a super-step that runs a
    /// non-idempotent node, so a crash during its side effect cannot resume from
    /// before an earlier step and repeat that step.
    async fn flush_before_side_effects(&mut self) -> Result<()> {
        let side_effecting = self.pending_nodes.iter().any(|node| {
            self.graph.node_effects.get(node) == Some(&adk_core::ToolEffect::NonIdempotent)
                || self.graph.is_governed(node)
        });
        if side_effecting {
            self.finish_checkpoints().await?;
        }
        Ok(())
    }

    /// Checkpoints a super-step that failed, when a resume would gain from it.
    ///
    /// The frontier still holds the failed node, so the run resumes at this step:
    /// the retry budget already spent is not handed out again, and the nodes that
    /// finished are answered from their pending writes instead of re-running. A
    /// failure to write is logged, not raised, so the node's own error is reported.
    async fn checkpoint_failed_step(&mut self) {
        let any_retryable =
            self.pending_nodes.iter().any(|node| self.graph.retry_policy_for(node).is_some());
        // A governed step is always recorded, so its refusal or failure can be seen.
        let any_governed = self.pending_nodes.iter().any(|node| self.graph.is_governed(node));
        let result =
            if any_retryable || any_governed || !self.pending_writes.is_empty() || self.unsaved {
                self.save_checkpoint().await.map(drop)
            } else {
                self.await_inflight().await
            };
        if let Err(error) = result {
            tracing::warn!(
                thread_id = %self.config.thread_id,
                step = self.step,
                error = %error,
                "failed to checkpoint a failing super-step; a resumed run repeats its work"
            );
        }
    }
}

/// What a predecessor hands a fan-in node: the value it wrote to a channel named
/// after itself, or else every update it wrote.
///
/// Never the whole state: in a loop the state holds the previous pass's
/// `{node}_fan_in`, so an arrival that embedded it would double each pass.
fn arrival_output(
    node: &str,
    updates: Option<&HashMap<String, serde_json::Value>>,
) -> serde_json::Value {
    let Some(updates) = updates else { return serde_json::Value::Object(serde_json::Map::new()) };
    if let Some(value) = updates.get(node) {
        return value.clone();
    }
    let mut keys: Vec<&String> = updates.keys().collect();
    keys.sort();
    serde_json::Value::Object(
        keys.into_iter().map(|key| (key.clone(), updates[key].clone())).collect(),
    )
}

/// A node's record in a step, with the keys of `writes` sorted.
fn node_record(
    node: &str,
    started_at: chrono::DateTime<chrono::Utc>,
    duration_ms: u64,
    status: NodeStatus,
    writes: Option<&HashMap<String, serde_json::Value>>,
    attempt: u32,
) -> NodeRecord {
    let mut writes: Vec<String> = writes.map(|w| w.keys().cloned().collect()).unwrap_or_default();
    writes.sort();
    NodeRecord {
        node: node.to_string(),
        started_at,
        duration_ms,
        status,
        writes,
        attempt,
        effect_key: None,
        error: None,
    }
}

/// Reads one metadata key of `checkpoint`, `None` when it is absent.
fn read_metadata<T: serde::de::DeserializeOwned>(
    checkpoint: &Checkpoint,
    key: &str,
) -> Result<Option<T>> {
    checkpoint
        .metadata
        .get(key)
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            GraphError::SerializationError(format!(
                "checkpoint metadata '{key}' is malformed: {error}"
            ))
        })
}

/// Every node reachable along `successors` from `sources`, the sources included,
/// never entering a node `blocked` rejects.
fn reachable_from<'g>(
    sources: impl Iterator<Item = &'g str>,
    successors: &HashMap<&'g str, Vec<&'g str>>,
    blocked: &impl Fn(&str) -> bool,
) -> std::collections::HashSet<&'g str> {
    let mut seen: std::collections::HashSet<&'g str> = std::collections::HashSet::new();
    let mut queue: Vec<&'g str> = sources.collect();
    while let Some(node) = queue.pop() {
        if !seen.insert(node) {
            continue;
        }
        for next in successors.get(node).into_iter().flatten() {
            if !blocked(next) && !seen.contains(next) {
                queue.push(next);
            }
        }
    }
    seen
}

/// Starts a new attempt of the node `ctx` belongs to.
///
/// Children are numbered per attempt, so a retried parent asks for the same
/// children under the same identities and is served those that finished.
pub(crate) fn begin_attempt(ctx: &NodeContext) {
    if let Some(invoker) = ctx.child_invoker() {
        invoker.begin_attempt();
    }
}

/// Saves a checkpoint, then trims the thread to its retention policy.
async fn write_checkpoint(
    cp: Arc<dyn crate::checkpoint::Checkpointer>,
    retention: Option<crate::checkpoint::RetentionPolicy>,
    checkpoint: Checkpoint,
) -> Result<String> {
    let id = cp.save(&checkpoint).await?;
    // Trimmed as the run proceeds, so the cost stays proportional to the run
    // and no external job is needed. After the save, so the newest counts.
    if let Some(policy) = &retention {
        let removed = cp.prune(&checkpoint.thread_id, policy).await?;
        if removed > 0 {
            tracing::debug!(thread_id = %checkpoint.thread_id, removed, "pruned old checkpoints");
        }
    }
    Ok(id)
}

/// Convenience methods for CompiledGraph
impl CompiledGraph {
    /// Execute the graph synchronously
    pub async fn invoke(&self, input: State, config: ExecutionConfig) -> Result<State> {
        self.invoke_detailed(input, config).await.map(|outcome| outcome.state)
    }

    /// Execute with ADK run configuration for agents inside a directly run graph.
    ///
    /// Use this to resume a tool-confirmation pause with decisions in
    /// [`adk_core::RunConfig`]. Existing [`Self::invoke`] callers remain
    /// standalone and use default ADK run configuration.
    pub async fn invoke_with_run_config(
        &self,
        input: State,
        config: ExecutionConfig,
        run_config: adk_core::RunConfig,
    ) -> Result<State> {
        let mut executor = PregelExecutor::new_with_run_config(self, config, Some(run_config));
        executor.run(input).await
    }

    /// Executes and reports what the run asked of its caller.
    ///
    /// Only a graph run as a [`SubgraphNode`](crate::subgraph::SubgraphNode) has
    /// anything to report beyond its state, so [`Self::invoke`] is the usual
    /// entry point.
    pub async fn invoke_detailed(
        &self,
        input: State,
        config: ExecutionConfig,
    ) -> Result<GraphOutcome> {
        let mut executor = PregelExecutor::new(self, config);
        let state = executor.run(input).await?;
        Ok(GraphOutcome { state, goto_parent: executor.goto_parent })
    }

    /// Execute with run configuration and retain a subgraph's parent routing outcome.
    pub async fn invoke_detailed_with_run_config(
        &self,
        input: State,
        config: ExecutionConfig,
        run_config: adk_core::RunConfig,
    ) -> Result<GraphOutcome> {
        let mut executor = PregelExecutor::new_with_run_config(self, config, Some(run_config));
        let state = executor.run(input).await?;
        Ok(GraphOutcome { state, goto_parent: executor.goto_parent })
    }

    /// Execute with streaming
    pub fn stream(
        &self,
        input: State,
        config: ExecutionConfig,
        mode: StreamMode,
    ) -> impl futures::Stream<Item = Result<StreamEvent>> + '_ {
        tracing::debug!("CompiledGraph::stream called with mode {:?}", mode);
        let executor = PregelExecutor::new(self, config);
        executor.run_stream(input, mode)
    }

    /// Stream with ADK run configuration for agents inside a directly run graph.
    ///
    /// A resumed tool-confirmation request can be approved or denied by passing
    /// [`adk_core::RunConfig::tool_confirmation_decisions`].
    pub fn stream_with_run_config(
        &self,
        input: State,
        config: ExecutionConfig,
        mode: StreamMode,
        run_config: adk_core::RunConfig,
    ) -> impl futures::Stream<Item = Result<StreamEvent>> + '_ {
        tracing::debug!("CompiledGraph::stream_with_run_config called with mode {:?}", mode);
        let executor = PregelExecutor::new_with_run_config(self, config, Some(run_config));
        executor.run_stream(input, mode)
    }

    /// Get current state for a thread
    pub async fn get_state(&self, thread_id: &str) -> Result<Option<State>> {
        if let Some(cp) = &self.checkpointer {
            Ok(cp.load(thread_id).await?.map(|c| c.state))
        } else {
            Ok(None)
        }
    }

    /// Update state for a thread (for human-in-the-loop)
    pub async fn update_state(
        &self,
        thread_id: &str,
        updates: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Result<()> {
        if let Some(cp) = &self.checkpointer
            && let Some(checkpoint) = cp.load(thread_id).await?
        {
            let mut state = checkpoint.state;
            for (key, value) in updates {
                self.schema.apply_update(&mut state, &key, value);
            }
            let mut new_checkpoint =
                Checkpoint::new(thread_id, state, checkpoint.step, checkpoint.pending_nodes)
                    .with_version(checkpoint.version + 1);
            // Everything a resume restores carries over, so editing state between
            // a pause and its resume keeps the answered gate, the retry budget,
            // finished children, and the arrivals a waiting join has recorded.
            new_checkpoint.cleared_interrupt = checkpoint.cleared_interrupt;
            new_checkpoint.attempts = checkpoint.attempts;
            new_checkpoint.child_ledger = checkpoint.child_ledger;
            new_checkpoint.pending_writes = checkpoint.pending_writes;
            for key in CARRIED_METADATA_KEYS {
                if let Some(value) = checkpoint.metadata.get(key) {
                    new_checkpoint.metadata.insert(key.to_string(), value.clone());
                }
            }
            cp.save(&new_checkpoint).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge::{END, START};
    use crate::graph::StateGraph;
    use crate::node::NodeOutput;
    use serde_json::json;

    #[tokio::test]
    async fn test_simple_execution() {
        let graph = StateGraph::with_channels(&["value"])
            .add_node_fn("set_value", |_ctx| async {
                Ok(NodeOutput::new().with_update("value", json!(42)))
            })
            .add_edge(START, "set_value")
            .add_edge("set_value", END)
            .compile()
            .unwrap();

        let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await.unwrap();

        assert_eq!(result.get("value"), Some(&json!(42)));
    }

    #[tokio::test]
    async fn test_sequential_execution() {
        let graph = StateGraph::with_channels(&["value"])
            .add_node_fn("step1", |_ctx| async {
                Ok(NodeOutput::new().with_update("value", json!(1)))
            })
            .add_node_fn("step2", |ctx| async move {
                let current = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("value", json!(current + 10)))
            })
            .add_edge(START, "step1")
            .add_edge("step1", "step2")
            .add_edge("step2", END)
            .compile()
            .unwrap();

        let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await.unwrap();

        assert_eq!(result.get("value"), Some(&json!(11)));
    }

    #[tokio::test]
    async fn test_conditional_routing() {
        let graph = StateGraph::with_channels(&["path", "result"])
            .add_node_fn("router", |ctx| async move {
                let path = ctx.get("path").and_then(|v| v.as_str()).unwrap_or("a");
                Ok(NodeOutput::new().with_update("route", json!(path)))
            })
            .add_node_fn("path_a", |_ctx| async {
                Ok(NodeOutput::new().with_update("result", json!("went to A")))
            })
            .add_node_fn("path_b", |_ctx| async {
                Ok(NodeOutput::new().with_update("result", json!("went to B")))
            })
            .add_edge(START, "router")
            .add_conditional_edges(
                "router",
                |state| state.get("route").and_then(|v| v.as_str()).unwrap_or(END).to_string(),
                [("a", "path_a"), ("b", "path_b"), (END, END)],
            )
            .add_edge("path_a", END)
            .add_edge("path_b", END)
            .compile()
            .unwrap();

        // Test path A
        let mut input = State::new();
        input.insert("path".to_string(), json!("a"));
        let result = graph.invoke(input, ExecutionConfig::new("test")).await.unwrap();
        assert_eq!(result.get("result"), Some(&json!("went to A")));

        // Test path B
        let mut input = State::new();
        input.insert("path".to_string(), json!("b"));
        let result = graph.invoke(input, ExecutionConfig::new("test")).await.unwrap();
        assert_eq!(result.get("result"), Some(&json!("went to B")));
    }

    #[tokio::test]
    async fn test_cycle_with_limit() {
        let graph = StateGraph::with_channels(&["count"])
            .add_node_fn("increment", |ctx| async move {
                let count = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("count", json!(count + 1)))
            })
            .add_edge(START, "increment")
            .add_conditional_edges(
                "increment",
                |state| {
                    let count = state.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                    if count < 5 { "increment".to_string() } else { END.to_string() }
                },
                [("increment", "increment"), (END, END)],
            )
            .compile()
            .unwrap();

        let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await.unwrap();

        assert_eq!(result.get("count"), Some(&json!(5)));
    }

    #[tokio::test]
    async fn test_recursion_limit() {
        let graph = StateGraph::with_channels(&["count"])
            .add_node_fn("loop", |ctx| async move {
                let count = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("count", json!(count + 1)))
            })
            .add_edge(START, "loop")
            .add_edge("loop", "loop") // Infinite loop
            .compile()
            .unwrap()
            .with_recursion_limit(10);

        let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await;

        // The recursion limit check happens when step >= limit, so it will exceed at step 10
        assert!(
            matches!(result, Err(GraphError::RecursionLimitExceeded(_))),
            "Expected RecursionLimitExceeded error, got: {:?}",
            result
        );
    }
}
