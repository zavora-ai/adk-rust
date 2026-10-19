//! Per-invocation and global retry state tracking.

use std::collections::{HashMap, VecDeque};

/// How many invocations a [`RetryTracker`] keeps counts for by default.
pub const DEFAULT_TRACKED_INVOCATIONS: usize = 1024;

/// The retry counts of one invocation.
#[derive(Debug, Default)]
struct InvocationRetries {
    /// Failure counts by tool name, then by call key.
    counts: HashMap<String, HashMap<String, u32>>,
    /// Retries suggested in this invocation, for the global limit.
    total: u32,
}

/// Retry counts for each invocation, so one invocation's failures never use up another's
/// budget.
///
/// Counts are kept by invocation ID, tool name and call key. A tool call that succeeds
/// clears its tool's counts in that invocation. The tracker keeps at most `capacity`
/// invocations and forgets the oldest first, so a long-running process holds bounded
/// state. Thread safety is provided by wrapping it in `Arc<Mutex<...>>` at the plugin level.
///
/// # Example
///
/// ```rust
/// use adk_retry_reflect::RetryTracker;
///
/// let mut tracker = RetryTracker::new();
/// assert_eq!(tracker.increment("inv-1", "search", "q=rust"), 1);
/// assert_eq!(tracker.get("inv-2", "search", "q=rust"), 0);
/// tracker.reset_tool("inv-1", "search");
/// assert_eq!(tracker.get("inv-1", "search", "q=rust"), 0);
/// ```
#[derive(Debug)]
pub struct RetryTracker {
    invocations: HashMap<String, InvocationRetries>,
    /// Invocation IDs, oldest first.
    order: VecDeque<String>,
    capacity: usize,
}

impl Default for RetryTracker {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_TRACKED_INVOCATIONS)
    }
}

impl RetryTracker {
    /// Create an empty tracker that keeps [`DEFAULT_TRACKED_INVOCATIONS`] invocations.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an empty tracker that keeps at most `capacity` invocations (at least one).
    pub fn with_capacity(capacity: usize) -> Self {
        Self { invocations: HashMap::new(), order: VecDeque::new(), capacity: capacity.max(1) }
    }

    /// Increment the failure count of `call_key` for `tool` in `invocation` and return it.
    pub fn increment(&mut self, invocation: &str, tool: &str, call_key: &str) -> u32 {
        if !self.invocations.contains_key(invocation) {
            while self.order.len() >= self.capacity {
                if let Some(oldest) = self.order.pop_front() {
                    self.invocations.remove(&oldest);
                }
            }
            self.order.push_back(invocation.to_string());
        }
        let retries = self.invocations.entry(invocation.to_string()).or_default();
        retries.total += 1;
        let count = retries
            .counts
            .entry(tool.to_string())
            .or_default()
            .entry(call_key.to_string())
            .or_insert(0);
        *count += 1;
        *count
    }

    /// The failure count of `call_key` for `tool` in `invocation`.
    pub fn get(&self, invocation: &str, tool: &str, call_key: &str) -> u32 {
        self.invocations
            .get(invocation)
            .and_then(|retries| retries.counts.get(tool))
            .and_then(|calls| calls.get(call_key))
            .copied()
            .unwrap_or(0)
    }

    /// The retries suggested in `invocation`, across all tools.
    pub fn total(&self, invocation: &str) -> u32 {
        self.invocations.get(invocation).map_or(0, |retries| retries.total)
    }

    /// Clears `tool`'s failure counts in `invocation`, after one of its calls succeeded.
    pub fn reset_tool(&mut self, invocation: &str, tool: &str) {
        if let Some(retries) = self.invocations.get_mut(invocation) {
            retries.counts.remove(tool);
        }
    }

    /// The number of invocations the tracker holds counts for.
    pub fn tracked_invocations(&self) -> usize {
        self.invocations.len()
    }

    /// Reset all failure counts.
    pub fn reset(&mut self) {
        self.invocations.clear();
        self.order.clear();
    }
}

/// Cross-invocation state for circuit-breaker patterns.
///
/// Counts each tool's failures since its last success across all invocations, to detect
/// chronically failing tools. A success closes the tool's circuit again.
#[derive(Debug, Default)]
pub struct GlobalRetryTracker {
    /// Failures per tool name since that tool's last success.
    tool_failures: HashMap<String, u32>,
}

impl GlobalRetryTracker {
    /// Create a new empty global tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a failure for the given tool and return its failures since its last success.
    pub fn record_failure(&mut self, tool_name: &str) -> u32 {
        let count = self.tool_failures.entry(tool_name.to_string()).or_insert(0);
        *count += 1;
        *count
    }

    /// Record a success for the given tool, which clears its failure count.
    pub fn record_success(&mut self, tool_name: &str) {
        self.tool_failures.remove(tool_name);
    }

    /// Check if a tool has exceeded the circuit-breaker threshold.
    pub fn is_circuit_broken(&self, tool_name: &str, threshold: u32) -> bool {
        self.tool_failures.get(tool_name).copied().unwrap_or(0) >= threshold
    }

    /// Reset all global failure counts.
    pub fn reset(&mut self) {
        self.tool_failures.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocations_count_separately() {
        let mut tracker = RetryTracker::new();
        assert_eq!(tracker.increment("inv-1", "search", "k"), 1);
        assert_eq!(tracker.increment("inv-1", "search", "k"), 2);
        assert_eq!(tracker.get("inv-2", "search", "k"), 0);
        assert_eq!(tracker.increment("inv-2", "search", "k"), 1);
        assert_eq!((tracker.total("inv-1"), tracker.total("inv-2")), (2, 1));
    }

    #[test]
    fn a_success_clears_only_that_tools_counts() {
        let mut tracker = RetryTracker::new();
        tracker.increment("inv", "search", "a");
        tracker.increment("inv", "search", "b");
        tracker.increment("inv", "fetch", "a");
        tracker.reset_tool("inv", "search");
        assert_eq!((tracker.get("inv", "search", "a"), tracker.get("inv", "search", "b")), (0, 0));
        assert_eq!(tracker.get("inv", "fetch", "a"), 1);
    }

    #[test]
    fn the_oldest_invocation_is_forgotten_at_capacity() {
        let mut tracker = RetryTracker::with_capacity(2);
        for invocation in ["inv-1", "inv-2", "inv-3"] {
            tracker.increment(invocation, "search", "k");
        }
        assert_eq!(tracker.tracked_invocations(), 2);
        assert_eq!(tracker.get("inv-1", "search", "k"), 0);
        assert_eq!(tracker.get("inv-3", "search", "k"), 1);
        // An invocation already tracked does not evict another.
        tracker.increment("inv-3", "search", "k");
        assert_eq!(tracker.get("inv-2", "search", "k"), 1);
    }

    #[test]
    fn a_success_closes_the_circuit() {
        let mut tracker = GlobalRetryTracker::new();
        tracker.record_failure("search");
        tracker.record_failure("search");
        assert!(tracker.is_circuit_broken("search", 2));
        tracker.record_success("search");
        assert!(!tracker.is_circuit_broken("search", 2));
    }
}
