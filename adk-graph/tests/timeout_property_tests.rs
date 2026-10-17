//! Property tests for node timeout enforcement.
//!
//! **Feature: runtime-reliability-sprint, Property 2: Timeout Precision**
//! *For any* node with `run_timeout = D`, the node SHALL be cancelled when its timer for D
//! elapses on the runtime clock.
//! **Validates: Requirements 3.2, 4.2**
//!
//! Each case runs on a paused Tokio clock, so the timer fires at exactly D. How late a
//! loaded host schedules the timer after that is outside what the timeout controls.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_graph::error::GraphError;
use adk_graph::node::{ExecutionConfig, FunctionNode, NodeContext, NodeOutput};
use adk_graph::state::State;
use adk_graph::timeout::{OnTimeout, TimeoutPolicy, execute_with_timeout};
use proptest::prelude::*;
use tokio::time::Instant;

// ── Helpers ───────────────────────────────────────────────────────────

/// Create a `NodeContext` with default state and config.
fn make_ctx() -> NodeContext {
    NodeContext::new(State::new(), ExecutionConfig::default(), 0)
}

/// A runtime whose clock advances only to the next pending timer.
fn paused_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap()
}

// ── Property 2: Timeout Precision (Fail policy) ───────────────────────
//
// **Feature: runtime-reliability-sprint, Property 2: Timeout Precision**
// *For any* timeout duration D in [50ms, 300ms], a node that sleeps for D + 1000ms
// SHALL be cancelled with `GraphError::NodeTimedOut` exactly D after it starts.
// **Validates: Requirements 3.2, 4.2**

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_timeout_fail_triggers_within_precision(
        timeout_ms in 50u64..=300u64
    ) {
        paused_runtime().block_on(async {
            let d = Duration::from_millis(timeout_ms);

            // Node sleeps for D + 1000ms — guaranteed to exceed timeout
            let node = FunctionNode::new("sleepy", move |_ctx| async move {
                tokio::time::sleep(d + Duration::from_millis(1000)).await;
                Ok(NodeOutput::new())
            });

            let policy = TimeoutPolicy {
                run_timeout: Some(d),
                idle_timeout: None,
                on_timeout: OnTimeout::Fail,
            };

            let ctx = make_ctx();
            let start = Instant::now();
            let result = execute_with_timeout(&node, &ctx, &policy).await;
            let elapsed = start.elapsed();

            match &result {
                Err(GraphError::NodeTimedOut { node, elapsed: reported }) => {
                    prop_assert_eq!(node.as_str(), "sleepy");
                    prop_assert_eq!(*reported, d);
                }
                Err(other) => {
                    prop_assert!(false, "expected NodeTimedOut, got: {:?}", other);
                }
                Ok(_) => {
                    prop_assert!(false, "expected error, got Ok");
                }
            }

            prop_assert_eq!(elapsed, d);

            Ok(())
        })?;
    }
}

// ── Property 2b: Skip policy returns Ok with empty output exactly at D ──

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_timeout_skip_returns_empty_within_precision(
        timeout_ms in 50u64..=300u64
    ) {
        paused_runtime().block_on(async {
            let d = Duration::from_millis(timeout_ms);

            let node = FunctionNode::new("sleepy_skip", move |_ctx| async move {
                tokio::time::sleep(d + Duration::from_millis(1000)).await;
                Ok(NodeOutput::new().with_update("should_not_appear", serde_json::json!(true)))
            });

            let policy = TimeoutPolicy {
                run_timeout: Some(d),
                idle_timeout: None,
                on_timeout: OnTimeout::Skip,
            };

            let ctx = make_ctx();
            let start = Instant::now();
            let result = execute_with_timeout(&node, &ctx, &policy).await;
            let elapsed = start.elapsed();

            // Assert the result is Ok with empty output
            match &result {
                Ok(output) => {
                    prop_assert!(
                        output.updates.is_empty(),
                        "expected empty output, got {:?}",
                        output.updates
                    );
                }
                Err(e) => {
                    prop_assert!(false, "expected Ok, got error: {:?}", e);
                }
            }

            prop_assert_eq!(elapsed, d);

            Ok(())
        })?;
    }
}

// ── Property 2c: Retry policy retries the correct number of times ─────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_timeout_retry_executes_correct_attempts(
        timeout_ms in 50u64..=100u64,
        max_attempts in 2usize..=4usize
    ) {
        paused_runtime().block_on(async {
            let d = Duration::from_millis(timeout_ms);
            let attempt_count = Arc::new(AtomicUsize::new(0));
            let count_clone = attempt_count.clone();

            let node = FunctionNode::new("retry_node", move |_ctx| {
                let count = count_clone.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(d + Duration::from_millis(1000)).await;
                    Ok(NodeOutput::new())
                }
            });

            let policy = TimeoutPolicy {
                run_timeout: Some(d),
                idle_timeout: None,
                on_timeout: OnTimeout::Retry { max_attempts },
            };

            let ctx = make_ctx();
            let result = execute_with_timeout(&node, &ctx, &policy).await;

            // After exhausting retries, should return NodeTimedOut
            match &result {
                Err(GraphError::NodeTimedOut { node, .. }) => {
                    prop_assert_eq!(node.as_str(), "retry_node");
                }
                Err(other) => {
                    prop_assert!(false, "expected NodeTimedOut, got: {:?}", other);
                }
                Ok(_) => {
                    prop_assert!(false, "expected error after retries exhausted, got Ok");
                }
            }

            // Assert the node was attempted exactly max_attempts times
            let actual_attempts = attempt_count.load(Ordering::SeqCst);
            prop_assert_eq!(
                actual_attempts,
                max_attempts,
                "expected {} attempts, got {}",
                max_attempts,
                actual_attempts
            );

            Ok(())
        })?;
    }
}
