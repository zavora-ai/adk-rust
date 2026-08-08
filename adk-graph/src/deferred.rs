//! Deferred node (fan-in barrier) support for graph workflows.
//!
//! Provides fan-in barrier semantics for nodes that wait on multiple upstream
//! parallel paths before executing. This enables scatter-gather patterns where
//! work is distributed across parallel branches and then collected at a single
//! synchronization point.
//!
//! # Overview
//!
//! A deferred node is declared with a [`DeferredNodeConfig`] that specifies:
//! - [`MergeStrategy`]: How upstream outputs are combined (collect, merge maps, first, or custom).
//! - `fan_in_timeout`: Optional maximum wait duration for all upstream paths.
//!
//! The [`FanInTracker`] tracks which upstream paths have completed and merges
//! their outputs according to the configured strategy.
//!
//! # Example
//!
//! ```rust
//! use std::time::Duration;
//! use adk_graph::deferred::{DeferredNodeConfig, FanInTracker, MergeStrategy};
//! use serde_json::json;
//!
//! // Configure a deferred node that collects all upstream outputs
//! let config = DeferredNodeConfig {
//!     merge_strategy: MergeStrategy::Collect,
//!     fan_in_timeout: Some(Duration::from_secs(30)),
//!     ..Default::default()
//! };
//!
//! // Track upstream completions
//! let mut tracker = FanInTracker::new(vec!["branch_a", "branch_b", "branch_c"]);
//!
//! tracker.record("branch_a", json!({"result": 1}));
//! tracker.record("branch_b", json!({"result": 2}));
//! assert!(!tracker.is_ready());
//!
//! tracker.record("branch_c", json!({"result": 3}));
//! assert!(tracker.is_ready());
//!
//! // Merge outputs using the configured strategy
//! let merged = tracker.merge(&config.merge_strategy);
//! assert_eq!(merged, json!([{"result": 1}, {"result": 2}, {"result": 3}]));
//! ```

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

/// How to combine outputs from multiple upstream parallel paths.
///
/// The merge strategy determines how the collected outputs from all upstream
/// branches are combined into a single value for the deferred node's input.
///
/// # Example
///
/// ```rust
/// use adk_graph::deferred::MergeStrategy;
///
/// // Default strategy collects all outputs into a Vec
/// let strategy = MergeStrategy::default();
/// assert!(matches!(strategy, MergeStrategy::Collect));
///
/// // MergeMap combines all output maps with last-write-wins
/// let strategy = MergeStrategy::MergeMap;
/// ```
#[derive(Clone, Default)]
pub enum MergeStrategy {
    /// Collect all outputs into a `Vec<Value>`.
    ///
    /// Outputs are ordered by the insertion order of source nodes
    /// (the order in which they were recorded).
    #[default]
    Collect,

    /// Merge all output maps into a single map (last-write-wins on key conflict).
    ///
    /// Each upstream output is expected to be a JSON object. Non-object outputs
    /// are skipped. When multiple outputs contain the same key, the value from
    /// the later-recorded source wins.
    MergeMap,

    /// Use only the first completed output.
    ///
    /// Returns the output from whichever upstream path completed first
    /// (i.e., was recorded first).
    First,

    /// Custom merge function.
    ///
    /// Accepts a closure that takes all collected outputs and produces a
    /// single merged value.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_graph::deferred::MergeStrategy;
    /// use serde_json::{json, Value};
    ///
    /// let strategy = MergeStrategy::Custom(Arc::new(|outputs: Vec<Value>| {
    ///     json!({ "count": outputs.len() })
    /// }));
    /// ```
    Custom(Arc<dyn Fn(Vec<Value>) -> Value + Send + Sync>),
}

impl fmt::Debug for MergeStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collect => write!(f, "Collect"),
            Self::MergeMap => write!(f, "MergeMap"),
            Self::First => write!(f, "First"),
            Self::Custom(_) => write!(f, "Custom(<fn>)"),
        }
    }
}

/// Configuration for a deferred (fan-in) node.
///
/// A deferred node waits for all upstream parallel paths to complete before
/// executing. The configuration controls how outputs are merged and how long
/// the node waits.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_graph::deferred::{DeferredNodeConfig, MergeStrategy};
///
/// let config = DeferredNodeConfig {
///     merge_strategy: MergeStrategy::MergeMap,
///     fan_in_timeout: Some(Duration::from_secs(60)),
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone, Default)]
pub struct DeferredNodeConfig {
    /// Strategy for combining upstream outputs.
    pub merge_strategy: MergeStrategy,

    /// Maximum time to wait for all upstream paths to complete.
    ///
    /// - `None`: Wait indefinitely for all upstream paths.
    /// - `Some(duration)`: If the timeout expires and some paths have completed,
    ///   proceed with partial results. If zero paths have completed, return
    ///   `GraphError::FanInTimedOut`.
    pub fan_in_timeout: Option<Duration>,
    /// How many predecessors must have arrived for `fan_in_timeout` to release
    /// the node with partial results.
    ///
    /// `None` means one, which is the behaviour this field replaced. Set it to
    /// the full predecessor count to require all of them even after a timeout, or
    /// to any value between for an n-of-m join: release once that many branches
    /// have answered and abandon the rest.
    pub min_predecessors: Option<usize>,
}

/// Tracks which upstream paths have completed for a deferred node.
///
/// The tracker maintains a set of expected source nodes and records their
/// outputs as they arrive. Once all expected sources have reported, the
/// tracker is ready and outputs can be merged.
///
/// # Example
///
/// ```rust
/// use adk_graph::deferred::{FanInTracker, MergeStrategy};
/// use serde_json::json;
///
/// let mut tracker = FanInTracker::new(vec!["node_a", "node_b"]);
///
/// assert!(!tracker.is_ready());
/// assert_eq!(tracker.received_count(), 0);
/// assert_eq!(tracker.expected_count(), 2);
///
/// tracker.record("node_a", json!("output_a"));
/// assert!(!tracker.is_ready());
///
/// tracker.record("node_b", json!("output_b"));
/// assert!(tracker.is_ready());
///
/// let merged = tracker.merge(&MergeStrategy::Collect);
/// assert_eq!(merged, json!(["output_a", "output_b"]));
/// ```
pub struct FanInTracker {
    /// The set of source node names we expect to receive output from.
    expected: HashSet<String>,
    /// Outputs received so far, keyed by source node name.
    received: HashMap<String, Value>,
    /// Insertion order of received outputs (for deterministic merge ordering).
    insertion_order: Vec<String>,
}

impl FanInTracker {
    /// Create a new tracker expecting outputs from the given source nodes.
    ///
    /// # Arguments
    ///
    /// * `expected_sources` - Names of upstream nodes that must complete
    ///   before this deferred node can execute.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::deferred::FanInTracker;
    ///
    /// let tracker = FanInTracker::new(vec!["branch_1", "branch_2", "branch_3"]);
    /// assert_eq!(tracker.expected_count(), 3);
    /// assert!(!tracker.is_ready());
    /// ```
    pub fn new(expected_sources: Vec<&str>) -> Self {
        Self {
            expected: expected_sources.iter().map(|s| (*s).to_string()).collect(),
            received: HashMap::new(),
            insertion_order: Vec::new(),
        }
    }

    /// Returns `true` when all expected sources have reported their output.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::deferred::FanInTracker;
    /// use serde_json::json;
    ///
    /// let mut tracker = FanInTracker::new(vec!["a"]);
    /// assert!(!tracker.is_ready());
    ///
    /// tracker.record("a", json!(42));
    /// assert!(tracker.is_ready());
    /// ```
    pub fn is_ready(&self) -> bool {
        self.expected.iter().all(|s| self.received.contains_key(s))
    }

    /// Record the output from a source node.
    ///
    /// If the source has already been recorded, the previous value is
    /// overwritten (last-write-wins). Recording a source that is not in
    /// the expected set is a no-op for readiness but the value is still stored.
    ///
    /// # Arguments
    ///
    /// * `source_node` - The name of the upstream node that produced the output.
    /// * `output` - The output value from the source node.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::deferred::FanInTracker;
    /// use serde_json::json;
    ///
    /// let mut tracker = FanInTracker::new(vec!["worker_1", "worker_2"]);
    /// tracker.record("worker_1", json!({"status": "done"}));
    /// assert_eq!(tracker.received_count(), 1);
    /// ```
    pub fn record(&mut self, source_node: &str, output: Value) {
        let key = source_node.to_string();
        if !self.received.contains_key(&key) {
            self.insertion_order.push(key.clone());
        }
        self.received.insert(key, output);
    }

    /// Merge all received outputs according to the given strategy.
    ///
    /// The merge operation combines all recorded outputs into a single
    /// [`Value`] based on the [`MergeStrategy`]:
    ///
    /// - [`MergeStrategy::Collect`]: Returns a JSON array of all outputs in
    ///   insertion order.
    /// - [`MergeStrategy::MergeMap`]: Merges all JSON object outputs into a
    ///   single object (last-write-wins). Non-object outputs are skipped.
    /// - [`MergeStrategy::First`]: Returns the first recorded output.
    /// - [`MergeStrategy::Custom`]: Invokes the custom function with all outputs.
    ///
    /// # Arguments
    ///
    /// * `strategy` - The merge strategy to apply.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::deferred::{FanInTracker, MergeStrategy};
    /// use serde_json::json;
    ///
    /// let mut tracker = FanInTracker::new(vec!["a", "b"]);
    /// tracker.record("a", json!({"x": 1}));
    /// tracker.record("b", json!({"y": 2}));
    ///
    /// // Collect strategy
    /// let result = tracker.merge(&MergeStrategy::Collect);
    /// assert_eq!(result, json!([{"x": 1}, {"y": 2}]));
    ///
    /// // MergeMap strategy
    /// let result = tracker.merge(&MergeStrategy::MergeMap);
    /// assert_eq!(result, json!({"x": 1, "y": 2}));
    /// ```
    pub fn merge(&self, strategy: &MergeStrategy) -> Value {
        match strategy {
            MergeStrategy::Collect => {
                let outputs: Vec<Value> = self
                    .insertion_order
                    .iter()
                    .filter_map(|key| self.received.get(key).cloned())
                    .collect();
                Value::Array(outputs)
            }
            MergeStrategy::MergeMap => {
                let mut merged = serde_json::Map::new();
                for key in &self.insertion_order {
                    if let Some(Value::Object(map)) = self.received.get(key) {
                        for (k, v) in map {
                            merged.insert(k.clone(), v.clone());
                        }
                    }
                }
                Value::Object(merged)
            }
            MergeStrategy::First => self
                .insertion_order
                .first()
                .and_then(|key| self.received.get(key).cloned())
                .unwrap_or(Value::Null),
            MergeStrategy::Custom(f) => {
                let outputs: Vec<Value> = self
                    .insertion_order
                    .iter()
                    .filter_map(|key| self.received.get(key).cloned())
                    .collect();
                f(outputs)
            }
        }
    }

    /// Returns the number of outputs received so far.
    pub fn received_count(&self) -> usize {
        self.received.len()
    }

    /// Returns the number of expected source nodes.
    pub fn expected_count(&self) -> usize {
        self.expected.len()
    }

    /// Returns the names of sources that have not yet reported.
    pub fn pending_sources(&self) -> Vec<&str> {
        self.expected
            .iter()
            .filter(|s| !self.received.contains_key(*s))
            .map(|s| s.as_str())
            .collect()
    }

    /// Returns the names of sources that have reported.
    pub fn completed_sources(&self) -> Vec<&str> {
        self.insertion_order.iter().map(|s| s.as_str()).collect()
    }
}

impl fmt::Debug for FanInTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FanInTracker")
            .field("expected", &self.expected)
            .field("received_keys", &self.insertion_order)
            .field("is_ready", &self.is_ready())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_tracker_new_empty_not_ready() {
        let tracker = FanInTracker::new(vec!["a", "b", "c"]);
        assert!(!tracker.is_ready());
        assert_eq!(tracker.expected_count(), 3);
        assert_eq!(tracker.received_count(), 0);
    }

    #[test]
    fn test_tracker_ready_when_all_received() {
        let mut tracker = FanInTracker::new(vec!["a", "b"]);
        tracker.record("a", json!(1));
        assert!(!tracker.is_ready());
        tracker.record("b", json!(2));
        assert!(tracker.is_ready());
    }

    #[test]
    fn test_merge_collect() {
        let mut tracker = FanInTracker::new(vec!["x", "y", "z"]);
        tracker.record("x", json!("first"));
        tracker.record("y", json!("second"));
        tracker.record("z", json!("third"));

        let result = tracker.merge(&MergeStrategy::Collect);
        assert_eq!(result, json!(["first", "second", "third"]));
    }

    #[test]
    fn test_merge_map_combines_objects() {
        let mut tracker = FanInTracker::new(vec!["a", "b"]);
        tracker.record("a", json!({"key1": "val1", "shared": "from_a"}));
        tracker.record("b", json!({"key2": "val2", "shared": "from_b"}));

        let result = tracker.merge(&MergeStrategy::MergeMap);
        assert_eq!(result, json!({"key1": "val1", "key2": "val2", "shared": "from_b"}));
    }

    #[test]
    fn test_merge_map_skips_non_objects() {
        let mut tracker = FanInTracker::new(vec!["a", "b"]);
        tracker.record("a", json!(42)); // Not an object, skipped
        tracker.record("b", json!({"key": "value"}));

        let result = tracker.merge(&MergeStrategy::MergeMap);
        assert_eq!(result, json!({"key": "value"}));
    }

    #[test]
    fn test_merge_first() {
        let mut tracker = FanInTracker::new(vec!["a", "b", "c"]);
        tracker.record("b", json!("first_to_arrive"));
        tracker.record("a", json!("second_to_arrive"));
        tracker.record("c", json!("third_to_arrive"));

        let result = tracker.merge(&MergeStrategy::First);
        assert_eq!(result, json!("first_to_arrive"));
    }

    #[test]
    fn test_merge_first_empty() {
        let tracker = FanInTracker::new(vec!["a"]);
        let result = tracker.merge(&MergeStrategy::First);
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn test_merge_custom() {
        let mut tracker = FanInTracker::new(vec!["a", "b"]);
        tracker.record("a", json!(10));
        tracker.record("b", json!(20));

        let strategy = MergeStrategy::Custom(Arc::new(|outputs| {
            let sum: i64 = outputs.iter().filter_map(|v| v.as_i64()).sum();
            json!(sum)
        }));

        let result = tracker.merge(&strategy);
        assert_eq!(result, json!(30));
    }

    #[test]
    fn test_record_overwrites_previous() {
        let mut tracker = FanInTracker::new(vec!["a"]);
        tracker.record("a", json!("first"));
        tracker.record("a", json!("second"));

        assert!(tracker.is_ready());
        assert_eq!(tracker.received_count(), 1);

        let result = tracker.merge(&MergeStrategy::First);
        assert_eq!(result, json!("second"));
    }

    #[test]
    fn test_pending_and_completed_sources() {
        let mut tracker = FanInTracker::new(vec!["a", "b", "c"]);
        tracker.record("b", json!(1));

        let mut pending = tracker.pending_sources();
        pending.sort();
        assert_eq!(pending, vec!["a", "c"]);
        assert_eq!(tracker.completed_sources(), vec!["b"]);
    }

    #[test]
    fn test_default_config() {
        let config = DeferredNodeConfig::default();
        assert!(matches!(config.merge_strategy, MergeStrategy::Collect));
        assert!(config.fan_in_timeout.is_none());
    }

    #[test]
    fn test_merge_strategy_debug() {
        assert_eq!(format!("{:?}", MergeStrategy::Collect), "Collect");
        assert_eq!(format!("{:?}", MergeStrategy::MergeMap), "MergeMap");
        assert_eq!(format!("{:?}", MergeStrategy::First), "First");
        let custom = MergeStrategy::Custom(Arc::new(Value::Array));
        assert_eq!(format!("{:?}", custom), "Custom(<fn>)");
    }
}
