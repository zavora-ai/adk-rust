//! A node that fails transiently should be retried, not end the run.
//!
//! Before this, an error from any node aborted the whole graph. The only retry
//! anywhere was inside the timeout path, which looped without a delay and passed
//! every other error straight through. adk-python and adk-go both support
//! attempts, initial delay, maximum delay, backoff factor and jitter.

use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::retry::{RetryOn, RetryPolicy};
use adk_graph::state::State;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A node that fails a set number of times, then succeeds.
fn flaky(fail_times: usize, calls: Arc<AtomicUsize>) -> impl Fn(usize) -> bool + Clone {
    let _ = calls;
    move |call| call < fail_times
}

/// A transient failure is retried and the run completes.
#[tokio::test]
async fn a_transient_failure_is_retried() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let should_fail = flaky(2, Arc::clone(&calls));

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("flaky", move |_ctx| {
            let counter = Arc::clone(&counter);
            let should_fail = should_fail.clone();
            async move {
                let call = counter.fetch_add(1, Ordering::SeqCst);
                if should_fail(call) {
                    return Err(GraphError::NodeExecutionFailed {
                        node: "flaky".to_string(),
                        message: format!("attempt {} failed", call + 1),
                    });
                }
                Ok(NodeOutput::new().with_update("value", json!("ok")))
            }
        })
        .add_edge(START, "flaky")
        .add_edge("flaky", END)
        .compile()
        .unwrap()
        .with_node_retry(
            "flaky",
            RetryPolicy::new(4).with_initial_delay(Duration::from_millis(5)).with_jitter(0.0),
        );

    let state = graph.invoke(State::new(), ExecutionConfig::new("retry-1")).await.unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 3, "two failures then a success");
    assert_eq!(state.get("value").and_then(|v| v.as_str()), Some("ok"));
}

/// Without a policy the first failure ends the run, which stays the default.
#[tokio::test]
async fn without_a_policy_the_first_failure_ends_the_run() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("always_fails", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(GraphError::NodeExecutionFailed {
                    node: "always_fails".to_string(),
                    message: "boom".to_string(),
                })
            }
        })
        .add_edge(START, "always_fails")
        .add_edge("always_fails", END)
        .compile()
        .unwrap();

    let outcome = graph.invoke(State::new(), ExecutionConfig::new("retry-2")).await;

    assert!(outcome.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one attempt when no policy is configured");
}

/// A node that never recovers exhausts its budget and then fails.
#[tokio::test]
async fn an_exhausted_budget_fails_the_run() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("always_fails", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(GraphError::NodeExecutionFailed {
                    node: "always_fails".to_string(),
                    message: "boom".to_string(),
                })
            }
        })
        .add_edge(START, "always_fails")
        .add_edge("always_fails", END)
        .compile()
        .unwrap()
        .with_node_retry(
            "always_fails",
            RetryPolicy::new(3).with_initial_delay(Duration::from_millis(1)).with_jitter(0.0),
        );

    let outcome = graph.invoke(State::new(), ExecutionConfig::new("retry-3")).await;

    assert!(outcome.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 3, "exactly the configured number of attempts");
}

/// Delays grow between attempts.
///
/// Fails if the retry loop is tight, which is what the timeout-only retry did.
#[tokio::test]
async fn delays_grow_between_attempts() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let started = Instant::now();

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("always_fails", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(GraphError::NodeExecutionFailed {
                    node: "always_fails".to_string(),
                    message: "boom".to_string(),
                })
            }
        })
        .add_edge(START, "always_fails")
        .add_edge("always_fails", END)
        .compile()
        .unwrap()
        .with_node_retry(
            "always_fails",
            RetryPolicy::new(4)
                .with_initial_delay(Duration::from_millis(30))
                .with_backoff_factor(2.0)
                .with_jitter(0.0),
        );

    let _ = graph.invoke(State::new(), ExecutionConfig::new("retry-4")).await;
    let elapsed = started.elapsed();

    // Three delays: 30ms, 60ms, 120ms. A tight loop would finish far sooner.
    assert!(
        elapsed >= Duration::from_millis(200),
        "elapsed {elapsed:?} is too short for 30ms + 60ms + 120ms of backoff"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

/// A `Timeout` policy leaves other errors alone.
#[tokio::test]
async fn a_timeout_policy_does_not_retry_other_errors() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("always_fails", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(GraphError::NodeExecutionFailed {
                    node: "always_fails".to_string(),
                    message: "not a timeout".to_string(),
                })
            }
        })
        .add_edge(START, "always_fails")
        .add_edge("always_fails", END)
        .compile()
        .unwrap()
        .with_node_retry(
            "always_fails",
            RetryPolicy::new(5)
                .with_initial_delay(Duration::from_millis(1))
                .with_retry_on(RetryOn::Timeout),
        );

    let _ = graph.invoke(State::new(), ExecutionConfig::new("retry-5")).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1, "a non-timeout error must not be retried");
}

/// An interrupt is never retried, whatever the policy allows.
///
/// A pause that retried would defeat the pause, so `RetryOn::Any` excludes it.
#[tokio::test]
async fn an_interrupt_is_not_retried() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("gate", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::interrupt("approve?"))
            }
        })
        .add_edge(START, "gate")
        .add_edge("gate", END)
        .compile()
        .unwrap()
        .with_checkpointer(adk_graph::checkpoint::MemoryCheckpointer::new())
        .with_node_retry(
            "gate",
            RetryPolicy::new(5)
                .with_initial_delay(Duration::from_millis(1))
                .with_retry_on(RetryOn::Custom(Arc::new(|_| true))),
        );

    let outcome = graph.invoke(State::new(), ExecutionConfig::new("retry-6")).await;

    assert!(matches!(outcome, Err(GraphError::Interrupted(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the gate must be asked once, not retried");
}

/// A retry budget is not restarted by a resume.
///
/// The attempt count lives in the checkpoint, so a node that has already burned
/// two of three attempts gets one more after a resume rather than three again.
/// adk-python does not persist this and documents the gap.
#[tokio::test]
async fn a_retry_budget_survives_a_resume() {
    use adk_graph::checkpoint::MemoryCheckpointer;

    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);

    // Fails on every attempt, and the gate before it forces a resume so the
    // budget has to cross a checkpoint.
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("gate", |_ctx| async move {
            Ok(NodeOutput::new().with_update("value", json!("gated")))
        })
        .add_node_fn("always_fails", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(GraphError::NodeExecutionFailed {
                    node: "always_fails".to_string(),
                    message: "boom".to_string(),
                })
            }
        })
        .add_edge(START, "gate")
        .add_edge("gate", "always_fails")
        .add_edge("always_fails", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
        .with_node_retry(
            "always_fails",
            RetryPolicy::new(3).with_initial_delay(Duration::from_millis(1)).with_jitter(0.0),
        );

    // First run exhausts the budget and fails.
    let first = graph.invoke(State::new(), ExecutionConfig::new("budget")).await;
    assert!(first.is_err());
    let after_first = calls.load(Ordering::SeqCst);
    assert_eq!(after_first, 3, "the first run spends the whole budget");

    // Resuming must not hand out a fresh budget.
    let second = graph.invoke(State::new(), ExecutionConfig::new("budget")).await;
    assert!(second.is_err());
    let after_second = calls.load(Ordering::SeqCst) - after_first;
    assert_eq!(
        after_second, 1,
        "a resumed run must attempt once more, not restart the budget: got {after_second}"
    );
}
