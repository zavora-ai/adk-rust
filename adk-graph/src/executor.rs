//! Pregel-based execution engine for graphs
//!
//! Executes graphs using the Pregel model with super-steps.

use crate::error::{GraphError, InterruptedExecution, Result};
use crate::graph::CompiledGraph;
use crate::interrupt::Interrupt;
use crate::node::{ExecutionConfig, NodeContext};
use crate::state::{Checkpoint, State};
use crate::stream::{StreamEvent, StreamMode};
use futures::stream::{self, StreamExt};
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
}

/// Pregel-based executor for graphs
pub struct PregelExecutor<'a> {
    graph: &'a CompiledGraph,
    config: ExecutionConfig,
    state: State,
    step: usize,
    pending_nodes: Vec<String>,
}

impl<'a> PregelExecutor<'a> {
    /// Create a new executor
    pub fn new(graph: &'a CompiledGraph, config: ExecutionConfig) -> Self {
        Self { graph, config, state: State::new(), step: 0, pending_nodes: vec![] }
    }

    /// Run the graph to completion
    pub async fn run(&mut self, input: State) -> Result<State> {
        // Initialize state
        self.state = self.initialize_state(input).await?;
        self.pending_nodes = self.graph.get_entry_nodes();

        // Main execution loop
        while !self.pending_nodes.is_empty() {
            // Check recursion limit
            if self.step >= self.config.recursion_limit {
                return Err(GraphError::RecursionLimitExceeded(self.step));
            }

            // Execute super-step
            let result = self.execute_super_step().await?;

            // Handle interrupts
            if let Some(interrupt) = result.interrupt {
                let checkpoint_id = self.save_checkpoint().await?;
                return Err(GraphError::Interrupted(Box::new(InterruptedExecution::new(
                    self.config.thread_id.clone(),
                    checkpoint_id,
                    interrupt,
                    self.state.clone(),
                    self.step,
                ))));
            }

            // Save checkpoint after each step
            self.save_checkpoint().await?;

            // Check if we're done (all paths led to END)
            if self.graph.leads_to_end(&result.executed_nodes, &self.state) {
                let next = self.graph.get_next_nodes(&result.executed_nodes, &self.state);
                if next.is_empty() {
                    break;
                }
            }

            // Determine next nodes
            self.pending_nodes = self.graph.get_next_nodes(&result.executed_nodes, &self.state);
            self.step += 1;
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
            // Initialize state
            match self.initialize_state(input).await {
                Ok(state) => self.state = state,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }
            self.pending_nodes = self.graph.get_entry_nodes();

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

                    for node_name in &self.pending_nodes {
                        if let Some(node) = self.graph.nodes.get(node_name) {
                            let ctx = NodeContext::new(self.state.clone(), self.config.clone(), self.step);
                            let start = std::time::Instant::now();

                            let mut node_stream = node.execute_stream(&ctx);
                            let mut collected_events = Vec::new();

                            while let Some(event_result) = node_stream.next().await {
                                match event_result {
                                    Ok(event) => {
                                        // Yield Message events immediately
                                        if matches!(event, StreamEvent::Message { .. }) {
                                            yield Ok(event.clone());
                                        }
                                        collected_events.push(event);
                                    }
                                    Err(e) => {
                                        yield Err(e);
                                        return;
                                    }
                                }
                            }

                            let duration_ms = start.elapsed().as_millis() as u64;
                            result.executed_nodes.push(node_name.clone());
                            result.events.push(StreamEvent::node_end(node_name, self.step, duration_ms));
                            result.events.extend(collected_events);

                            // Get output from execute for state updates
                            if let Ok(output) = node.execute(&ctx).await {
                                for (key, value) in output.updates {
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

                    self.pending_nodes = self.graph.get_next_nodes(&result.executed_nodes, &self.state);
                    self.step += 1;
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
                    yield Ok(StreamEvent::interrupted(
                        result.executed_nodes.first().map(|s| s.as_str()).unwrap_or("unknown"),
                        &interrupt.to_string(),
                    ));
                    return;
                }

                // Check if done
                if self.graph.leads_to_end(&result.executed_nodes, &self.state) {
                    let next = self.graph.get_next_nodes(&result.executed_nodes, &self.state);
                    if next.is_empty() {
                        break;
                    }
                }

                self.pending_nodes = self.graph.get_next_nodes(&result.executed_nodes, &self.state);
                self.step += 1;
            }

            yield Ok(StreamEvent::done(self.state.clone(), self.step + 1));
        }
    }

    /// Initialize state from input and/or checkpoint
    async fn initialize_state(&self, input: State) -> Result<State> {
        // Start with schema defaults
        let mut state = self.graph.schema.initialize_state();

        // If resuming from checkpoint, load it
        if let Some(checkpoint_id) = &self.config.resume_from {
            if let Some(cp) = self.graph.checkpointer.as_ref() {
                if let Some(checkpoint) = cp.load_by_id(checkpoint_id).await? {
                    state = checkpoint.state;
                }
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

        // Check for interrupt_before
        for node_name in &self.pending_nodes {
            if self.graph.interrupt_before.contains(node_name) {
                return Ok(SuperStepResult {
                    interrupt: Some(Interrupt::Before(node_name.clone())),
                    ..Default::default()
                });
            }
        }

        // Execute all pending nodes in parallel
        let nodes: Vec<_> = self
            .pending_nodes
            .iter()
            .filter_map(|name| self.graph.nodes.get(name).map(|n| (name.clone(), n.clone())))
            .collect();

        let futures: Vec<_> = nodes
            .into_iter()
            .map(|(name, node)| {
                let ctx = NodeContext::new(self.state.clone(), self.config.clone(), self.step);
                let step = self.step;
                async move {
                    let start = Instant::now();
                    let output = node.execute(&ctx).await;
                    let duration_ms = start.elapsed().as_millis() as u64;
                    (name, output, duration_ms, step)
                }
            })
            .collect();

        let outputs: Vec<_> =
            stream::iter(futures).buffer_unordered(self.pending_nodes.len()).collect().await;

        // Collect all updates and check for errors/interrupts
        let mut all_updates = Vec::new();

        for (node_name, output_result, duration_ms, step) in outputs {
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
                        });
                    }

                    // Collect custom events
                    result.events.extend(output.events);

                    // Collect updates
                    all_updates.push(output.updates);
                }
                Err(e) => {
                    return Err(GraphError::NodeExecutionFailed {
                        node: node_name,
                        message: e.to_string(),
                    });
                }
            }
        }

        // Apply all updates atomically using reducers
        for updates in all_updates {
            for (key, value) in updates {
                self.graph.schema.apply_update(&mut self.state, &key, value);
            }
        }

        // Check for interrupt_after
        for node_name in &result.executed_nodes {
            if self.graph.interrupt_after.contains(node_name) {
                return Ok(SuperStepResult {
                    interrupt: Some(Interrupt::After(node_name.clone())),
                    ..result
                });
            }
        }

        Ok(result)
    }

    /// Save a checkpoint
    async fn save_checkpoint(&self) -> Result<String> {
        if let Some(cp) = &self.graph.checkpointer {
            let checkpoint = Checkpoint::new(
                &self.config.thread_id,
                self.state.clone(),
                self.step,
                self.pending_nodes.clone(),
            );
            return cp.save(&checkpoint).await;
        }
        Ok(String::new())
    }
}

/// Convenience methods for CompiledGraph
impl CompiledGraph {
    /// Execute the graph synchronously
    pub async fn invoke(&self, input: State, config: ExecutionConfig) -> Result<State> {
        let mut executor = PregelExecutor::new(self, config);
        executor.run(input).await
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
        if let Some(cp) = &self.checkpointer {
            if let Some(checkpoint) = cp.load(thread_id).await? {
                let mut state = checkpoint.state;
                for (key, value) in updates {
                    self.schema.apply_update(&mut state, &key, value);
                }
                let new_checkpoint =
                    Checkpoint::new(thread_id, state, checkpoint.step, checkpoint.pending_nodes);
                cp.save(&new_checkpoint).await?;
            }
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
