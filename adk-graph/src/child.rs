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
//! Each invocation is recorded under a path, `<parent>/<child>@<run_id>`. When a
//! parent is re-executed after a resume it runs from the top, so without a record
//! every child would run again. A path already in the ledger returns its recorded
//! output instead.
//!
//! Only a successful invocation is recorded. A child that failed or interrupted
//! has to run again, because its work did not finish.
//!
//! The default `run_id` counts invocations of that child name within one
//! activation, so it is stable only while the parent runs once. **A parent that
//! may be resumed must supply its own `run_id`**, or the counter will hand the
//! same identity to a different unit of work. adk-go documents the same trap.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::node::{Node, NodeContext};

/// How one child invocation behaves.
#[derive(Debug, Clone, Default)]
pub struct RunNodeOptions {
    /// Stable identity for replay.
    ///
    /// Defaults to a counter over invocations of this child name within the
    /// current activation. Supply one when the parent may be resumed.
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
    /// Outputs already recorded, keyed by child path. Shared with the executor so
    /// the run's checkpoint carries them.
    ledger: Arc<Mutex<HashMap<String, Value>>>,
    /// The invoking node's path, which prefixes its children's.
    parent_path: String,
    /// Invocations so far per child name, for the default run id.
    counters: Mutex<HashMap<String, u32>>,
}

impl ChildInvoker {
    pub(crate) fn new(
        nodes: HashMap<String, Arc<dyn Node>>,
        ledger: Arc<Mutex<HashMap<String, Value>>>,
        parent_path: String,
    ) -> Self {
        Self { nodes, ledger, parent_path, counters: Mutex::new(HashMap::new()) }
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
        format!("{}/{}@{}", self.parent_path, child, run_id)
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
        let child_ctx = NodeContext::new(state, parent.config.clone(), parent.step);

        let output = node.execute(&child_ctx).await?;

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
