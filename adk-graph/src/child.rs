//! Invoking a node from inside another node's body.
//!
//! Declared edges express a topology decided before the run. A supervisor often
//! cannot: how many workers to start, and in what order, depends on what the
//! first one found. Both adk-python and adk-go answer this the same way, and
//! neither mutates the graph — a node body calls other nodes directly and awaits
//! their output.
//!
//! # Identity and replay
//!
//! Each invocation is recorded under a path, `<parent>@<step>/<child>@<run_id>`.
//! When a parent is re-executed it runs from the top, so without a record every
//! child would run again. A path already in the ledger returns its recorded output
//! instead. The parent re-executes the same activation when it is retried, when it
//! resumes after a pause, or when a failed step resumes.
//!
//! Only a successful invocation is recorded. A child that failed or interrupted
//! has to run again, because its work did not finish.
//!
//! The parent's step is part of the path, and a parent's entries are dropped once
//! its step completes, so a parent that runs again in a later pass of a loop runs
//! its children again rather than receiving the earlier pass's output.
//!
//! The default `run_id` counts invocations of that child name within one attempt
//! of the parent, so a retried or resumed parent that asks for its children in
//! the same order gets the same identities. **A parent whose order of calls can
//! change between attempts must supply its own `run_id`**, or the counter will
//! hand the same identity to a different unit of work. adk-go documents the same
//! trap.
//!
//! # Retry
//!
//! A child with a policy set by
//! [`with_node_retry`](crate::graph::CompiledGraph::with_node_retry) is retried
//! under it, inside its parent's attempt. The graph's default policy is not
//! applied to children, because the parent's own policy already retries the
//! parent, and a retried parent is served the children that finished.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::node::{Node, NodeContext};
use crate::retry::RetryPolicy;

/// How one child invocation behaves.
#[derive(Debug, Clone, Default)]
pub struct RunNodeOptions {
    /// Stable identity for replay.
    ///
    /// Defaults to a counter over invocations of this child name within the
    /// current attempt of the parent. Supply one when the parent's order of calls
    /// can change between attempts.
    pub run_id: Option<String>,
}

impl RunNodeOptions {
    /// Options with an explicit run id.
    pub fn with_run_id(run_id: impl Into<String>) -> Self {
        Self { run_id: Some(run_id.into()) }
    }
}

/// The machinery a [`NodeContext`] needs to invoke another node.
///
/// Built by the executor for each node it runs, so a node body reaches only the
/// graph it belongs to.
pub(crate) struct ChildInvoker {
    /// Every node in the graph, including any reachable by no edge.
    nodes: HashMap<String, Arc<dyn Node>>,
    /// Retry policies set per node, applied when that node runs as a child.
    retry_policies: HashMap<String, RetryPolicy>,
    /// Outputs already recorded, keyed by child path. Shared with the executor so
    /// the run's checkpoint carries them.
    ledger: Arc<Mutex<HashMap<String, Value>>>,
    /// The invoking node's activation, which prefixes its children's paths.
    activation: String,
    /// Invocations so far per child name in this attempt, for the default run id.
    counters: Mutex<HashMap<String, u32>>,
}

impl ChildInvoker {
    pub(crate) fn new(
        nodes: HashMap<String, Arc<dyn Node>>,
        retry_policies: HashMap<String, RetryPolicy>,
        ledger: Arc<Mutex<HashMap<String, Value>>>,
        parent: &str,
        step: usize,
    ) -> Self {
        Self {
            nodes,
            retry_policies,
            ledger,
            activation: Self::activation_prefix(parent, step),
            counters: Mutex::new(HashMap::new()),
        }
    }

    /// The prefix of every ledger path the activation of `parent` at `step` records.
    pub(crate) fn activation_prefix(parent: &str, step: usize) -> String {
        format!("{parent}@{step}/")
    }

    /// Starts a new attempt of the parent, so its children are numbered from one
    /// again and those that finished are served from the ledger.
    pub(crate) fn begin_attempt(&self) {
        self.counters.lock().expect("child counters").clear();
    }

    /// The path this invocation is recorded under.
    fn path_for(&self, child: &str, options: &RunNodeOptions) -> String {
        let run_id = match &options.run_id {
            Some(id) => id.clone(),
            None => {
                let mut counters = self.counters.lock().expect("child counters");
                let count = counters.entry(child.to_string()).or_insert(0);
                *count += 1;
                count.to_string()
            }
        };
        format!("{}{}@{}", self.activation, child, run_id)
    }

    /// Invoke a child and return its updates as one value.
    pub(crate) async fn run(
        &self,
        child: &str,
        input: Value,
        options: RunNodeOptions,
        parent: &NodeContext,
    ) -> Result<Value> {
        let path = self.path_for(child, &options);

        // A child that already completed under this identity is not run again.
        if let Some(recorded) = self.ledger.lock().expect("child ledger").get(&path) {
            tracing::debug!(path = %path, "child already completed, serving its recorded output");
            return Ok(recorded.clone());
        }

        let node = self
            .nodes
            .get(child)
            .ok_or_else(|| GraphError::NodeNotFound(child.to_string()))?
            .clone();

        // The child sees the parent's state, with its input merged over it.
        let mut state = parent.state.clone();
        if let Value::Object(map) = input {
            for (key, value) in map {
                state.insert(key, value);
            }
        }
        let mut child_ctx = NodeContext::new(state, parent.config.clone(), parent.step);
        if let Some(run_config) = parent.run_config() {
            child_ctx.set_run_config(run_config);
        }
        if let Some(schema) = parent.parent_schema() {
            child_ctx.set_parent_schema(schema);
        }

        let policy = self.retry_policies.get(child);
        let mut attempts = 0;
        let output = loop {
            let result = node.execute(&child_ctx).await;
            attempts += 1;
            let Err(error) = &result else { break result };
            let Some(policy) = policy else { break result };
            if !policy.allows_another_attempt(attempts) || !policy.retry_on.should_retry(error) {
                break result;
            }
            let delay = policy.delay_for_attempt(attempts);
            tracing::warn!(
                path = %path,
                attempt = attempts,
                max_attempts = policy.max_attempts,
                delay_ms = delay.as_millis(),
                error = %error,
                "child failed, retrying after backoff"
            );
            tokio::time::sleep(delay).await;
        }?;

        // An interrupt is not a result: the child has not finished, so nothing is
        // recorded and it runs again on resume.
        if let Some(interrupt) = output.interrupt {
            return Err(GraphError::Interrupted(Box::new(
                crate::error::InterruptedExecution::new(
                    parent.config.thread_id.clone(),
                    String::new(),
                    interrupt,
                    child_ctx.state.clone(),
                    parent.step,
                ),
            )));
        }

        let value = Value::Object(output.updates.into_iter().collect());
        self.ledger.lock().expect("child ledger").insert(path.clone(), value.clone());
        tracing::debug!(path = %path, "child completed");
        Ok(value)
    }
}
