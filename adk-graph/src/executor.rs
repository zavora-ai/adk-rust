//! Pregel-based execution engine for graphs
//!
//! Executes graphs using the Pregel model with super-steps.

#[cfg(feature = "node-cache")]
use crate::cache::{NodeCache, compute_cache_key};
use crate::deferred::FanInTracker;
use crate::error::{GraphError, InterruptedExecution, Result};
use crate::graph::CompiledGraph;
use crate::interrupt::Interrupt;
use crate::node::{ExecutionConfig, NodeContext};
use crate::state::{Checkpoint, State};
use crate::stream::{StreamEvent, StreamMode};
use crate::timeout::{OnTimeout, ProgressHandle, execute_with_timeout, item_timeout_budget};
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Result of a super-step execution
#[derive(Default)]
pub struct SuperStepResult {
    /// Nodes that were executed
    pub executed_nodes: Vec<String>,
    /// Interrupt if one occurred
    pub interrupt: Option<Interrupt>,
    /// Stream events generated
    pub events: Vec<StreamEvent>,
    /// Nodes that named their own successors, keyed by node name.
    pub goto: HashMap<String, Vec<String>>,
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
    state: State,
    step: usize,
    pending_nodes: Vec<String>,
    /// Parent nodes a node asked to run next; see `NodeOutput::with_goto_parent`.
    goto_parent: Option<Vec<String>>,
    /// Tracks deferred nodes waiting for all upstream paths to complete.
    pending_deferred: HashMap<String, FanInTracker>,
    /// Tracks when each deferred node first entered the pending state (for fan-in timeout).
    deferred_start_times: HashMap<String, Instant>,
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
    /// Per-node caches initialized from `CompiledGraph::cache_policies`.
    #[cfg(feature = "node-cache")]
    node_caches: HashMap<String, NodeCache>,
}

impl<'a> PregelExecutor<'a> {
    /// Create a new executor
    pub fn new(graph: &'a CompiledGraph, config: ExecutionConfig) -> Self {
        #[cfg(feature = "node-cache")]
        let node_caches = graph
            .cache_policies
            .iter()
            .map(|(name, policy)| (name.clone(), NodeCache::from_policy(policy)))
            .collect();

        Self {
            graph,
            config,
            state: State::new(),
            step: 0,
            pending_nodes: vec![],
            goto_parent: None,
            pending_deferred: HashMap::new(),
            deferred_start_times: HashMap::new(),
            attempts: HashMap::new(),
            child_ledger: Arc::new(std::sync::Mutex::new(HashMap::new())),
            cleared_interrupt: None,
            #[cfg(feature = "node-cache")]
            node_caches,
        }
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
            // Restore state from checkpoint
            self.state = checkpoint.state;
            self.pending_nodes = checkpoint.pending_nodes;
            self.step = checkpoint.step;
            self.cleared_interrupt = checkpoint.cleared_interrupt;
            self.attempts = checkpoint.attempts;
            *self.child_ledger.lock().expect("child ledger") = checkpoint.child_ledger;

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
            self.pending_nodes = self.graph.get_entry_nodes();
        }

        // Main execution loop
        while !self.pending_nodes.is_empty() {
            // Check recursion limit
            if self.step >= self.config.recursion_limit {
                return Err(GraphError::RecursionLimitExceeded(self.step));
            }

            // Execute super-step
            let result = match self.execute_super_step().await {
                Ok(result) => result,
                Err(error) => {
                    // Checkpoint before propagating, so a retry budget already
                    // spent is not handed out again by the next invocation. The
                    // frontier still holds the failed node, which is what makes
                    // the run resumable at all.
                    let any_retryable = self
                        .pending_nodes
                        .iter()
                        .any(|node| self.graph.retry_policy_for(node).is_some());
                    if any_retryable {
                        let _ = self.save_checkpoint().await;
                    }
                    return Err(error);
                }
            };

            // Handle interrupts
            if let Some(interrupt) = result.interrupt {
                // Record the gate being answered so the resumed run executes
                // this node rather than stopping at it again.
                if let Interrupt::Before(node) = &interrupt {
                    self.cleared_interrupt = Some(node.clone());
                }
                // `After` has the opposite timing: the node ran and its updates
                // are applied, so the resume point is its successors. Saving the
                // executing frontier would re-run it and re-raise the gate.
                if matches!(interrupt, Interrupt::After(_)) {
                    let next = self.next_frontier(&result.executed_nodes, &result.goto)?;
                    self.pending_nodes =
                        self.filter_deferred_nodes(next, &result.executed_nodes)?;
                }
                // For `Before`, the frontier saved is deliberately the one that
                // was executing: the node produced no updates, so resuming must
                // run it, which the marker above now permits.
                let checkpoint_id = self.save_checkpoint().await?;
                return Err(GraphError::Interrupted(Box::new(InterruptedExecution::new(
                    self.config.thread_id.clone(),
                    checkpoint_id,
                    interrupt,
                    self.state.clone(),
                    self.step,
                ))));
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
            let next_candidates = self.next_frontier(&result.executed_nodes, &result.goto)?;
            self.pending_nodes =
                self.filter_deferred_nodes(next_candidates, &result.executed_nodes)?;
            self.step += 1;

            // An empty frontier is a terminal checkpoint: resuming it re-reads
            // the final state instead of restarting the graph.
            self.save_checkpoint().await?;

            if self.pending_nodes.is_empty() {
                break;
            }
        }

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
                self.pending_nodes = self.graph.get_entry_nodes();
            }

            // Stream initial state if requested
            if matches!(mode, StreamMode::Values) {
                yield Ok(StreamEvent::state(self.state.clone(), self.step));
            }

            // Main execution loop
            while !self.pending_nodes.is_empty() {
                // Check recursion limit
                if self.step >= self.config.recursion_limit {
                    yield Err(GraphError::RecursionLimitExceeded(self.step));
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
                    if let Some(interrupt) = self.gate_before(&self.pending_nodes) {
                        result.interrupt = Some(interrupt);
                    }

                    for node_name in &self.pending_nodes {
                        if result.interrupt.is_some() {
                            break;
                        }
                        if let Some(node) = self.graph.nodes.get(node_name) {
                            let mut ctx = NodeContext::new(self.state.clone(), self.config.clone(), self.step);
                            ctx.set_parent_schema(Arc::new(self.graph.schema.clone()));
                            ctx.set_child_invoker(Arc::new(crate::child::ChildInvoker::new(
                                self.graph.nodes.clone(),
                                Arc::clone(&self.child_ledger),
                                node_name.clone(),
                            )));

                            // Attach progress handle if idle timeout is configured
                            let policy = self.graph.timeout_policy_for(node_name).cloned();
                            if let Some(ref p) = policy
                                && p.idle_timeout.is_some() {
                                    ctx.set_progress_handle(ProgressHandle::new());
                                }

                            let start = std::time::Instant::now();

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
                                        yield Err(GraphError::NodeTimedOut {
                                            node: node_name.clone(),
                                            elapsed,
                                        });
                                        return;
                                    }
                                }
                            }

                            let duration_ms = start.elapsed().as_millis() as u64;
                            result.executed_nodes.push(node_name.clone());
                            result.events.push(StreamEvent::node_end(node_name, self.step, duration_ms));
                            result.events.extend(collected_events);

                            if let Some((source, targets)) = streamed_goto {
                                result.goto.insert(source, targets);
                            }
                            if let Some(interrupt) = streamed_interrupt {
                                result.interrupt = Some(interrupt);
                            }

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

                    // Yield node_end events
                    for event in &result.events {
                        if matches!(event, StreamEvent::NodeEnd { .. }) {
                            yield Ok(event.clone());
                        }
                    }

                    // A node that arms a gate on completion stops the run here, unless
                    // it already asked to pause itself.
                    if result.interrupt.is_none()
                        && let Some(interrupt) = self.gate_after(&result.executed_nodes)
                    {
                        result.interrupt = Some(interrupt);
                    }

                    // This branch returns rather than falling through to the shared
                    // handling below, so the pause is reported here.
                    if let Some(interrupt) = result.interrupt {
                        if let Interrupt::Before(node) = &interrupt {
                            self.cleared_interrupt = Some(node.clone());
                        }
                        // `After` resumes at the successors, because that node has
                        // already applied its updates; see `run`.
                        if matches!(interrupt, Interrupt::After(_)) {
                            let next =
                                self.next_frontier(&result.executed_nodes, &result.goto)?;
                            match self.filter_deferred_nodes(next, &result.executed_nodes) {
                                Ok(frontier) => self.pending_nodes = frontier,
                                Err(error) => {
                                    yield Err(error);
                                    return;
                                }
                            }
                        }
                        // Persist before reporting: without this the pause cannot be
                        // resumed and the work already done is lost.
                        if let Err(error) = self.save_checkpoint().await {
                            yield Err(error);
                            return;
                        }
                        yield Ok(StreamEvent::interrupted(
                            result.executed_nodes.first().map(|s| s.as_str()).unwrap_or("unknown"),
                            &interrupt.to_string(),
                        ));
                        return;
                    }

                    // The gate re-arms once its node has run; see `run`.
                    if let Some(cleared) = &self.cleared_interrupt
                        && result.executed_nodes.iter().any(|n| n == cleared)
                    {
                        self.cleared_interrupt = None;
                    }

                    self.pending_nodes = {
                        let next_candidates = self.next_frontier(&result.executed_nodes, &result.goto)?;
                        match self.filter_deferred_nodes(next_candidates, &result.executed_nodes) {
                            Ok(nodes) => nodes,
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                        }
                    };
                    self.step += 1;

                    // The other path checkpoints every super-step. This one did not,
                    // so a run in this mode left no state to resume from and
                    // `get_state` reported nothing.
                    if let Err(e) = self.save_checkpoint().await {
                        yield Err(e);
                        return;
                    }
                    continue;
                }

                // Execute super-step (non-streaming)
                let result = match self.execute_super_step().await {
                    Ok(r) => r,
                    Err(e) => {
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
                if let Some(interrupt) = result.interrupt {
                    // Record the gate being answered; see `run`.
                    if let Interrupt::Before(node) = &interrupt {
                        self.cleared_interrupt = Some(node.clone());
                    }
                    // `After` resumes at the successors; see `run`.
                    if matches!(interrupt, Interrupt::After(_)) {
                        let next =
                            self.next_frontier(&result.executed_nodes, &result.goto)?;
                        match self.filter_deferred_nodes(next, &result.executed_nodes) {
                            Ok(frontier) => self.pending_nodes = frontier,
                            Err(error) => {
                                yield Err(error);
                                return;
                            }
                        }
                    }
                    // Persist before reporting: without this the interrupt is
                    // unresumable, because resuming loads the checkpoint for the
                    // thread. The frontier saved is the one that was executing,
                    // since an interrupted node still owes its updates.
                    if let Err(e) = self.save_checkpoint().await {
                        yield Err(e);
                        return;
                    }
                    yield Ok(StreamEvent::interrupted(
                        result.executed_nodes.first().map(|s| s.as_str()).unwrap_or("unknown"),
                        &interrupt.to_string(),
                    ));
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

                self.pending_nodes = {
                    let next_candidates = self.next_frontier(&result.executed_nodes, &result.goto)?;
                    match self.filter_deferred_nodes(next_candidates, &result.executed_nodes) {
                        Ok(nodes) => nodes,
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    }
                };
                self.step += 1;

                if let Err(e) = self.save_checkpoint().await {
                    yield Err(e);
                    return;
                }
            }

            yield Ok(StreamEvent::done(self.state.clone(), self.step + 1));
        }
    }

    /// Filter deferred nodes from the next candidates.
    ///
    /// For each candidate node that is configured as deferred, check whether all
    /// upstream paths have completed. If not, hold the node in `pending_deferred`
    /// and record the outputs from the just-executed nodes. If all upstream paths
    /// have completed, inject the merged output into state and allow the node to
    /// proceed.
    ///
    /// If a deferred node has a `fan_in_timeout` configured and the timeout has
    /// elapsed:
    /// - If at least one upstream path has completed, proceed with partial results.
    /// - If zero upstream paths have completed, return `GraphError::FanInTimedOut`.
    fn filter_deferred_nodes(
        &mut self,
        candidates: Vec<String>,
        executed_nodes: &[String],
    ) -> Result<Vec<String>> {
        let mut ready_nodes = Vec::new();

        for candidate in candidates {
            if let Some(config) = self.graph.deferred_configs.get(&candidate) {
                // This is a deferred node — check if all upstream paths are done
                let upstream = self.graph.get_upstream_nodes(&candidate);

                // Get or create the tracker for this deferred node
                let tracker = self.pending_deferred.entry(candidate.clone()).or_insert_with(|| {
                    let sources: Vec<&str> = upstream.iter().map(|s| s.as_str()).collect();
                    FanInTracker::new(sources)
                });

                // Record the start time if this is the first time we see this deferred node
                self.deferred_start_times.entry(candidate.clone()).or_insert_with(Instant::now);

                // Record outputs from the just-executed nodes that are upstream of this deferred node
                for executed in executed_nodes {
                    if upstream.contains(executed) {
                        // Use the current state as the output representation for this upstream node.
                        // We capture a snapshot of the state that this upstream node contributed to.
                        let output = self.state.get(executed).cloned().unwrap_or_else(|| {
                            // If no state key matches the node name, capture the full state
                            serde_json::Value::Object(
                                self.state.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                            )
                        });
                        tracker.record(executed, output);
                    }
                }

                if tracker.is_ready() {
                    // All upstream paths have completed — merge and inject into state
                    let merged = tracker.merge(&config.merge_strategy);
                    let fan_in_key = format!("{candidate}_fan_in");
                    self.graph.schema.apply_update(&mut self.state, &fan_in_key, merged);

                    // Remove from pending_deferred and start times since it's now ready
                    self.pending_deferred.remove(&candidate);
                    self.deferred_start_times.remove(&candidate);
                    ready_nodes.push(candidate);
                } else if let Some(timeout_duration) = config.fan_in_timeout {
                    // Check if the fan-in timeout has elapsed
                    let start_time = self.deferred_start_times[&candidate];
                    if start_time.elapsed() >= timeout_duration {
                        let received = tracker.received_count();
                        let expected = tracker.expected_count();

                        // `min_predecessors` decides how many arrivals are enough
                        // to release the node once the timeout expires.
                        let required = config.min_predecessors.unwrap_or(1).max(1);
                        if received >= required {
                            // Proceed with partial results
                            tracing::warn!(
                                node = %candidate,
                                received,
                                expected,
                                "fan-in timeout expired, proceeding with partial results"
                            );
                            let merged = tracker.merge(&config.merge_strategy);
                            let fan_in_key = format!("{candidate}_fan_in");
                            self.graph.schema.apply_update(&mut self.state, &fan_in_key, merged);

                            // Clean up tracking state
                            self.pending_deferred.remove(&candidate);
                            self.deferred_start_times.remove(&candidate);
                            ready_nodes.push(candidate);
                        } else {
                            // Too few arrived to release the node.
                            self.pending_deferred.remove(&candidate);
                            self.deferred_start_times.remove(&candidate);
                            return Err(GraphError::FanInTimedOut {
                                node: candidate,
                                received,
                                expected,
                            });
                        }
                    }
                }
                // If not ready and no timeout (or timeout not yet elapsed), the node stays
                // in pending_deferred and is NOT added to ready_nodes
            } else {
                // Not a deferred node — schedule normally
                ready_nodes.push(candidate);
            }
        }

        Ok(ready_nodes)
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

        if let Some(interrupt) = self.gate_before(&self.pending_nodes) {
            return Ok(SuperStepResult { interrupt: Some(interrupt), ..Default::default() });
        }

        // --- Node cache: check for cache hits before executing ---
        #[cfg(feature = "node-cache")]
        let mut cached_results: HashMap<String, serde_json::Value> = HashMap::new();
        #[cfg(feature = "node-cache")]
        let mut nodes_to_execute: Vec<String> = Vec::new();

        #[cfg(feature = "node-cache")]
        {
            for node_name in &self.pending_nodes {
                if let Some(cache) = self.node_caches.get(node_name) {
                    let cache_key = compute_cache_key(node_name, &self.state);
                    let cached_value = cache.get(&cache_key).await;
                    tracing::debug!(
                        node = %node_name,
                        cache_hit = cached_value.is_some(),
                        cache_key = %cache_key,
                        "node cache lookup"
                    );
                    if let Some(value) = cached_value {
                        // Cache hit — store the cached result for later application
                        cached_results.insert(node_name.clone(), value);
                    } else {
                        // Cache miss — node needs execution
                        nodes_to_execute.push(node_name.clone());
                    }
                } else {
                    // No cache configured — node needs execution
                    nodes_to_execute.push(node_name.clone());
                }
            }
        }

        // Apply cached results immediately
        #[cfg(feature = "node-cache")]
        {
            for (node_name, cached_value) in &cached_results {
                result.executed_nodes.push(node_name.clone());
                result.events.push(StreamEvent::node_end(node_name, self.step, 0));

                // Reconstruct updates from the cached JSON value (a map of key -> value)
                if let Some(updates_map) = cached_value.as_object() {
                    self.ensure_channels_declared(
                        node_name,
                        updates_map.keys().map(String::as_str),
                    )?;
                    for (key, value) in updates_map {
                        self.graph.schema.apply_update(&mut self.state, key, value.clone());
                    }
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
            self.pending_nodes.sort();
            &self.pending_nodes
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

        let futures: Vec<_> = nodes
            .into_iter()
            .zip(timeout_policies)
            .zip(retry_policies)
            .zip(prior_attempts)
            .map(|((((name, node), policy), retry), spent)| {
                let mut ctx = NodeContext::new(self.state.clone(), self.config.clone(), self.step);
                // A node body may invoke other nodes. The invoker carries the
                // graph's nodes and the shared ledger, so a resumed parent serves
                // children that already finished. These invocations are awaited
                // inline by the parent and are deliberately outside the
                // concurrency budget: counting them could deadlock, because the
                // parent holds its own slot while waiting.
                ctx.set_parent_schema(Arc::new(self.graph.schema.clone()));
                ctx.set_child_invoker(Arc::new(crate::child::ChildInvoker::new(
                    self.graph.nodes.clone(),
                    Arc::clone(&self.child_ledger),
                    name.clone(),
                )));

                // Attach a ProgressHandle when idle timeout is configured
                if let Some(ref p) = policy
                    && p.idle_timeout.is_some()
                {
                    ctx.set_progress_handle(ProgressHandle::new());
                }

                let step = self.step;
                async move {
                    let start = Instant::now();
                    let mut attempts = spent;
                    let output = loop {
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
                        tokio::time::sleep(delay).await;
                    };
                    let duration_ms = start.elapsed().as_millis() as u64;
                    (name, output, duration_ms, step, attempts)
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
        let outputs: Vec<_> = stream::iter(futures).buffer_unordered(concurrency).collect().await;

        // Collect all updates and check for errors/interrupts
        let mut all_updates = Vec::new();

        for (node_name, output_result, duration_ms, step, attempts) in outputs {
            // Record the budget spent, so a resumed run does not restart it. A
            // node that finally succeeded keeps no entry: its budget is spent
            // only while it is failing.
            if output_result.is_err() {
                self.attempts.insert(node_name.clone(), attempts);
            } else {
                self.attempts.remove(&node_name);
            }
            result.executed_nodes.push(node_name.clone());
            result.events.push(StreamEvent::node_end(&node_name, step, duration_ms));

            match output_result {
                Ok(output) => {
                    // Check for dynamic interrupt
                    if let Some(interrupt) = output.interrupt {
                        return Ok(SuperStepResult {
                            interrupt: Some(interrupt),
                            executed_nodes: result.executed_nodes,
                            events: result.events,
                            goto: result.goto,
                        });
                    }

                    // Collect custom events
                    result.events.extend(output.events);

                    // Store result in cache on miss
                    #[cfg(feature = "node-cache")]
                    {
                        if let Some(cache) = self.node_caches.get(&node_name) {
                            let cache_key = compute_cache_key(&node_name, &self.state);
                            let updates_value = serde_json::to_value(&output.updates)
                                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                            let ttl = self.graph.cache_policies.get(&node_name).and_then(|p| p.ttl);
                            cache.set(&cache_key, updates_value, ttl).await;
                        }
                    }

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
                    all_updates.push((node_name.clone(), output.updates));
                }
                Err(e) => {
                    // The retry budget is spent. A handler may record what
                    // happened and name a recovery node instead of ending the run.
                    // An interrupt never reaches here as a failure.
                    match self.graph.error_handler_for(&node_name) {
                        Some(handler) if !matches!(e, GraphError::Interrupted(_)) => {
                            let recovery = handler(&node_name, &e, &self.state)?;
                            if let Some(targets) = recovery.goto {
                                result.goto.insert(node_name.clone(), targets);
                            }
                            result.executed_nodes.push(node_name.clone());
                            all_updates.push((node_name, recovery.updates));
                        }
                        _ => {
                            return Err(GraphError::NodeExecutionFailed {
                                node: node_name,
                                message: e.to_string(),
                            });
                        }
                    }
                }
            }
        }

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

        if let Some(interrupt) = self.gate_after(&result.executed_nodes) {
            return Ok(SuperStepResult { interrupt: Some(interrupt), ..result });
        }

        Ok(result)
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
    fn gate_before(&self, pending: &[String]) -> Option<Interrupt> {
        pending
            .iter()
            .find(|node| {
                self.graph.interrupt_before.contains(*node)
                    && self.cleared_interrupt.as_deref() != Some(node.as_str())
            })
            .map(|node| Interrupt::Before(node.clone()))
    }

    /// Returns the gate an executed node arms, if any.
    fn gate_after(&self, executed: &[String]) -> Option<Interrupt> {
        executed
            .iter()
            .find(|node| self.graph.interrupt_after.contains(*node))
            .map(|node| Interrupt::After(node.clone()))
    }

    /// Computes the next frontier, letting a node's `goto` stand in for its edges.
    ///
    /// A node that named successors has its declared edges skipped, so a `goto`
    /// replaces an edge rather than adding to one. `END` is accepted and
    /// contributes no successor, which is how a branch stops.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::UnknownRouteTarget`] when a `goto` names a node the
    /// graph does not hold.
    fn next_frontier(
        &self,
        executed: &[String],
        goto: &HashMap<String, Vec<String>>,
    ) -> Result<Vec<String>> {
        // A node that routed itself does not also follow its declared edges.
        let followed_edges: Vec<String> =
            executed.iter().filter(|node| !goto.contains_key(*node)).cloned().collect();
        let mut next = self.graph.get_next_nodes(&followed_edges, &self.state)?;

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
                if !next.contains(target) {
                    next.push(target.clone());
                }
            }
        }
        Ok(next)
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

    async fn save_checkpoint(&self) -> Result<String> {
        if let Some(cp) = &self.graph.checkpointer {
            let mut checkpoint = Checkpoint::new(
                &self.config.thread_id,
                self.state.clone(),
                self.step,
                self.pending_nodes.clone(),
            );
            checkpoint.cleared_interrupt = self.cleared_interrupt.clone();
            checkpoint.attempts = self.attempts.clone();
            checkpoint.child_ledger = self.child_ledger.lock().expect("child ledger").clone();
            let id = cp.save(&checkpoint).await?;

            // Trimmed as the run proceeds, so the cost stays proportional to the run
            // and no external job is needed. After the save, so the newest counts.
            if let Some(policy) = &self.graph.retention {
                let removed = cp.prune(&self.config.thread_id, policy).await?;
                if removed > 0 {
                    tracing::debug!(
                        thread_id = %self.config.thread_id,
                        removed,
                        "pruned old checkpoints"
                    );
                }
            }
            return Ok(id);
        }
        Ok(String::new())
    }
}

/// Convenience methods for CompiledGraph
impl CompiledGraph {
    /// Execute the graph synchronously
    pub async fn invoke(&self, input: State, config: ExecutionConfig) -> Result<State> {
        self.invoke_detailed(input, config).await.map(|outcome| outcome.state)
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
            let new_checkpoint =
                Checkpoint::new(thread_id, state, checkpoint.step, checkpoint.pending_nodes);
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
