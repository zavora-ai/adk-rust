//! Timeout enforcement for graph node execution.
//!
//! Provides wall-clock and idle timeout enforcement for individual graph nodes,
//! with configurable recovery actions (fail, retry, skip).
//!
//! # Overview
//!
//! The [`TimeoutPolicy`] struct configures timeout behavior for a node:
//! - `run_timeout`: Hard wall-clock limit from when the node starts executing.
//! - `idle_timeout`: Resets each time `report_progress()` is called on the progress handle.
//! - `on_timeout`: What to do when a timeout fires ([`OnTimeout`]).
//!
//! # Example
//!
//! ```rust,ignore
//! use std::time::Duration;
//! use adk_graph::timeout::{TimeoutPolicy, OnTimeout};
//!
//! let policy = TimeoutPolicy {
//!     run_timeout: Some(Duration::from_secs(30)),
//!     idle_timeout: Some(Duration::from_secs(5)),
//!     on_timeout: OnTimeout::Retry { max_attempts: 3 },
//! };
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::error::{GraphError, Result};
use crate::node::{Node, NodeContext, NodeOutput};

/// Recovery action when a node times out.
#[derive(Debug, Clone, Default)]
pub enum OnTimeout {
    /// Fail the graph with `GraphError::NodeTimedOut`.
    #[default]
    Fail,
    /// Retry the node up to `max_attempts` times before failing.
    Retry { max_attempts: usize },
    /// Skip the node and proceed with an empty output.
    Skip,
}

/// Timeout configuration for a graph node.
///
/// # Example
///
/// ```rust,ignore
/// use std::time::Duration;
/// use adk_graph::timeout::{TimeoutPolicy, OnTimeout};
///
/// let policy = TimeoutPolicy {
///     run_timeout: Some(Duration::from_secs(10)),
///     idle_timeout: None,
///     on_timeout: OnTimeout::Fail,
/// };
/// ```
#[derive(Debug, Clone, Default)]
pub struct TimeoutPolicy {
    /// Hard wall-clock limit. Timer starts when node begins execution.
    pub run_timeout: Option<Duration>,
    /// Idle timeout: resets each time `report_progress()` is called.
    pub idle_timeout: Option<Duration>,
    /// Recovery action on timeout.
    pub on_timeout: OnTimeout,
}

/// A shared progress handle that nodes can use to report progress,
/// resetting the idle timeout counter.
///
/// The handle stores the last progress timestamp as milliseconds since
/// the UNIX epoch using an atomic u64 for lock-free updates.
#[derive(Debug, Clone)]
pub struct ProgressHandle {
    last_progress_ms: Arc<AtomicU64>,
}

impl ProgressHandle {
    /// Create a new progress handle initialized to the current time.
    pub fn new() -> Self {
        let now_ms = current_time_ms();
        Self { last_progress_ms: Arc::new(AtomicU64::new(now_ms)) }
    }

    /// Report progress, resetting the idle timeout counter.
    pub fn report_progress(&self) {
        let now_ms = current_time_ms();
        self.last_progress_ms.store(now_ms, Ordering::Release);
    }

    /// Get the last progress timestamp in milliseconds since epoch.
    pub(crate) fn last_progress_ms(&self) -> u64 {
        self.last_progress_ms.load(Ordering::Acquire)
    }
}

impl Default for ProgressHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Execute a node with timeout enforcement.
///
/// Uses `tokio::select!` to race node execution against configured timeouts.
/// When a timeout fires, the configured [`OnTimeout`] recovery action is applied:
///
/// - [`OnTimeout::Fail`]: Returns `GraphError::NodeTimedOut`.
/// - [`OnTimeout::Retry`]: Re-executes the node up to `max_attempts` times.
/// - [`OnTimeout::Skip`]: Returns an empty [`NodeOutput`].
///
/// A `tracing::warn!` is emitted whenever a timeout triggers a recovery action.
///
/// # Arguments
///
/// * `node` - The node to execute.
/// * `ctx` - The node execution context.
/// * `policy` - The timeout policy to enforce.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::timeout::{execute_with_timeout, TimeoutPolicy, OnTimeout};
/// use std::time::Duration;
///
/// let policy = TimeoutPolicy {
///     run_timeout: Some(Duration::from_secs(5)),
///     idle_timeout: None,
///     on_timeout: OnTimeout::Fail,
/// };
///
/// let result = execute_with_timeout(&my_node, &ctx, &policy).await;
/// ```
pub async fn execute_with_timeout(
    node: &dyn Node,
    ctx: &NodeContext,
    policy: &TimeoutPolicy,
) -> Result<NodeOutput> {
    // If no timeouts are configured, execute directly
    if policy.run_timeout.is_none() && policy.idle_timeout.is_none() {
        return node.execute(ctx).await;
    }

    let mut attempts = 0;

    loop {
        attempts += 1;
        let result = execute_once_with_timeout(node, ctx, policy).await;

        match result {
            Ok(output) => return Ok(output),
            Err(GraphError::NodeTimedOut { ref node, ref elapsed }) => {
                match &policy.on_timeout {
                    OnTimeout::Fail => {
                        tracing::warn!(
                            node = %node,
                            elapsed_ms = elapsed.as_millis(),
                            action = "fail",
                            "node timed out, failing execution"
                        );
                        return result;
                    }
                    OnTimeout::Retry { max_attempts } => {
                        if attempts >= *max_attempts {
                            tracing::warn!(
                                node = %node,
                                elapsed_ms = elapsed.as_millis(),
                                attempts = attempts,
                                action = "fail_after_retries",
                                "node timed out after all retry attempts exhausted"
                            );
                            return result;
                        }
                        tracing::warn!(
                            node = %node,
                            elapsed_ms = elapsed.as_millis(),
                            attempt = attempts,
                            max_attempts = *max_attempts,
                            action = "retry",
                            "node timed out, retrying"
                        );
                        // Continue loop to retry
                    }
                    OnTimeout::Skip => {
                        tracing::warn!(
                            node = %node,
                            elapsed_ms = elapsed.as_millis(),
                            action = "skip",
                            "node timed out, skipping with empty output"
                        );
                        return Ok(NodeOutput::new());
                    }
                }
            }
            Err(other) => return Err(other),
        }
    }
}

/// Execute a single attempt of a node with timeout enforcement.
async fn execute_once_with_timeout(
    node: &dyn Node,
    ctx: &NodeContext,
    policy: &TimeoutPolicy,
) -> Result<NodeOutput> {
    let node_name = node.name().to_string();
    let progress_handle = ProgressHandle::new();

    // Build a context with the progress handle attached so the node can
    // call `report_progress()` to reset the idle timeout.
    let mut timeout_ctx = NodeContext::new(ctx.state.clone(), ctx.config.clone(), ctx.step);
    timeout_ctx.set_progress_handle(progress_handle.clone());

    tokio::select! {
        result = node.execute(&timeout_ctx) => {
            result
        }
        elapsed = wait_for_run_timeout(policy.run_timeout) => {
            Err(GraphError::NodeTimedOut {
                node: node_name,
                elapsed,
            })
        }
        elapsed = wait_for_idle_timeout(policy.idle_timeout, &progress_handle) => {
            Err(GraphError::NodeTimedOut {
                node: node_name,
                elapsed,
            })
        }
    }
}

/// Wait for the run timeout to expire. If no run timeout is configured,
/// this future never completes (allowing the select to be driven by other branches).
async fn wait_for_run_timeout(run_timeout: Option<Duration>) -> Duration {
    match run_timeout {
        Some(duration) => {
            tokio::time::sleep(duration).await;
            duration
        }
        None => {
            // Never completes — effectively infinite timeout
            std::future::pending::<()>().await;
            unreachable!()
        }
    }
}

/// Poll for idle timeout expiry. Checks every 100ms whether the time since
/// last progress exceeds the idle timeout. If no idle timeout is configured,
/// this future never completes.
async fn wait_for_idle_timeout(
    idle_timeout: Option<Duration>,
    progress_handle: &ProgressHandle,
) -> Duration {
    match idle_timeout {
        Some(idle_duration) => {
            let start_ms = current_time_ms();
            let idle_ms = idle_duration.as_millis() as u64;
            let poll_interval = Duration::from_millis(100);

            loop {
                tokio::time::sleep(poll_interval).await;
                let now_ms = current_time_ms();
                let last_progress = progress_handle.last_progress_ms();
                let idle_elapsed = now_ms.saturating_sub(last_progress);

                if idle_elapsed >= idle_ms {
                    let total_elapsed_ms = now_ms.saturating_sub(start_ms);
                    return Duration::from_millis(total_elapsed_ms);
                }
            }
        }
        None => {
            // Never completes — effectively infinite timeout
            std::future::pending::<()>().await;
            unreachable!()
        }
    }
}

/// Get the current time in milliseconds since the UNIX epoch.
fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Returns how long a streaming node may take to produce its next event.
///
/// The budget is the smaller of the remaining run timeout and the idle timeout.
/// For a stream, "idle" means no event was produced within `idle_timeout`, which
/// is the streaming analogue of a node that never calls `report_progress()`.
/// Returns `None` when the policy sets no applicable limit.
pub fn item_timeout_budget(policy: &TimeoutPolicy, elapsed: Duration) -> Option<Duration> {
    let remaining_run = policy.run_timeout.map(|limit| limit.saturating_sub(elapsed));
    match (remaining_run, policy.idle_timeout) {
        (Some(run), Some(idle)) => Some(run.min(idle)),
        (Some(run), None) => Some(run),
        (None, Some(idle)) => Some(idle),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{ExecutionConfig, FunctionNode, NodeContext, NodeOutput};
    use crate::state::State;

    #[tokio::test]
    async fn test_no_timeout_executes_normally() {
        let node = FunctionNode::new("fast", |_ctx| async {
            Ok(NodeOutput::new().with_update("done", serde_json::json!(true)))
        });

        let ctx = NodeContext::new(State::new(), ExecutionConfig::default(), 0);
        let policy = TimeoutPolicy::default();

        let result = execute_with_timeout(&node, &ctx, &policy).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.updates.get("done"), Some(&serde_json::json!(true)));
    }

    #[tokio::test]
    async fn test_run_timeout_fires_on_slow_node() {
        let node = FunctionNode::new("slow", |_ctx| async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(NodeOutput::new())
        });

        let ctx = NodeContext::new(State::new(), ExecutionConfig::default(), 0);
        let policy = TimeoutPolicy {
            run_timeout: Some(Duration::from_millis(100)),
            idle_timeout: None,
            on_timeout: OnTimeout::Fail,
        };

        let result = execute_with_timeout(&node, &ctx, &policy).await;
        assert!(result.is_err());
        match result {
            Err(GraphError::NodeTimedOut { node, .. }) => {
                assert_eq!(node, "slow");
            }
            Err(other) => panic!("expected NodeTimedOut, got: {other:?}"),
            Ok(_) => panic!("expected error, got Ok"),
        }
    }

    #[tokio::test]
    async fn test_skip_returns_empty_output() {
        let node = FunctionNode::new("slow", |_ctx| async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(NodeOutput::new().with_update("should_not_appear", serde_json::json!(true)))
        });

        let ctx = NodeContext::new(State::new(), ExecutionConfig::default(), 0);
        let policy = TimeoutPolicy {
            run_timeout: Some(Duration::from_millis(50)),
            idle_timeout: None,
            on_timeout: OnTimeout::Skip,
        };

        let result = execute_with_timeout(&node, &ctx, &policy).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.updates.is_empty());
    }

    #[tokio::test]
    async fn test_retry_retries_up_to_max_attempts() {
        use std::sync::atomic::AtomicUsize;

        let attempt_count = Arc::new(AtomicUsize::new(0));
        let count_clone = attempt_count.clone();

        let node = FunctionNode::new("flaky", move |_ctx| {
            let count = count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(NodeOutput::new())
            }
        });

        let ctx = NodeContext::new(State::new(), ExecutionConfig::default(), 0);
        let policy = TimeoutPolicy {
            run_timeout: Some(Duration::from_millis(50)),
            idle_timeout: None,
            on_timeout: OnTimeout::Retry { max_attempts: 3 },
        };

        let result = execute_with_timeout(&node, &ctx, &policy).await;
        assert!(result.is_err());
        assert_eq!(attempt_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_fast_node_with_timeout_succeeds() {
        let node = FunctionNode::new("fast", |_ctx| async {
            Ok(NodeOutput::new().with_update("value", serde_json::json!(42)))
        });

        let ctx = NodeContext::new(State::new(), ExecutionConfig::default(), 0);
        let policy = TimeoutPolicy {
            run_timeout: Some(Duration::from_secs(5)),
            idle_timeout: None,
            on_timeout: OnTimeout::Fail,
        };

        let result = execute_with_timeout(&node, &ctx, &policy).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.updates.get("value"), Some(&serde_json::json!(42)));
    }

    #[test]
    fn test_progress_handle_updates_timestamp() {
        let handle = ProgressHandle::new();
        let initial = handle.last_progress_ms();

        // Small sleep to ensure time advances
        std::thread::sleep(Duration::from_millis(10));
        handle.report_progress();

        let updated = handle.last_progress_ms();
        assert!(updated >= initial);
    }
}
