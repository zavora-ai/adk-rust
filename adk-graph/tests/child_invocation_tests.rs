//! A node body can invoke other nodes, deciding at run time how many and in
//! which order.
//!
//! Declared edges fix the topology before the run. A supervisor often cannot know
//! it: how many workers to start depends on what the first one found. Both
//! adk-python and adk-go solve this without mutating the graph — a node calls
//! other nodes directly — and `adk-graph` had no equivalent.

use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::child::RunNodeOptions;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A supervisor decides at run time how many children to invoke.
#[tokio::test]
async fn a_node_invokes_children_it_chooses_at_run_time() {
    let worker_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&worker_calls);

    let graph = StateGraph::with_channels(&["batch", "results"])
        .add_node_fn("supervisor", |ctx| async move {
            // The batch size is state, so the count is not known when the graph
            // is built.
            let batch = ctx.get("batch").and_then(|v| v.as_u64()).unwrap_or(0);
            let mut results = Vec::new();
            for index in 0..batch {
                let output = ctx
                    .run_node_with(
                        "worker",
                        json!({ "item": index }),
                        RunNodeOptions::with_run_id(index.to_string()),
                    )
                    .await?;
                results.push(output);
            }
            Ok(NodeOutput::new().with_update("results", json!(results)))
        })
        .add_node_fn("worker", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let item = ctx.get("item").and_then(|v| v.as_u64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("doubled", json!(item * 2)))
            }
        })
        .add_edge(START, "supervisor")
        .add_edge("supervisor", END)
        .compile()
        .unwrap();

    let mut input = State::new();
    input.insert("batch".to_string(), json!(3));
    let state = graph.invoke(input, ExecutionConfig::new("children-1")).await.unwrap();

    assert_eq!(worker_calls.load(Ordering::SeqCst), 3, "one call per item in the batch");
    let results = state.get("results").and_then(|v| v.as_array()).expect("results");
    assert_eq!(results.len(), 3);
    assert_eq!(results[2].get("doubled"), Some(&json!(4)));
}

/// A child invoked only imperatively needs no edge.
///
/// `worker` above is wired to nothing. This test makes that explicit, because a
/// graph that rejected an unreferenced node would forbid the whole pattern.
#[tokio::test]
async fn an_imperatively_invoked_child_needs_no_edge() {
    let graph = StateGraph::with_channels(&["out"])
        .add_node_fn("only", |ctx| async move {
            let output = ctx.run_node("detached", json!({ "n": 5 })).await?;
            Ok(NodeOutput::new().with_update("out", output))
        })
        .add_node_fn("detached", |ctx| async move {
            let n = ctx.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("squared", json!(n * n)))
        })
        .add_edge(START, "only")
        .add_edge("only", END)
        .compile()
        .expect("a node reachable only through run_node must not fail validation");

    let state = graph.invoke(State::new(), ExecutionConfig::new("children-2")).await.unwrap();
    assert_eq!(state.get("out").and_then(|v| v.get("squared")), Some(&json!(25)));
}

/// A completed child is not executed again after a resume.
///
/// The parent re-runs from the top, so without the ledger every child would run
/// a second time — doubling any side effect they had.
#[tokio::test]
async fn a_completed_child_is_not_re_executed_after_a_resume() {
    let worker_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&worker_calls);
    let gate_open = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gate_for_node = Arc::clone(&gate_open);

    let graph = StateGraph::with_channels(&["first", "second"])
        .add_node_fn("supervisor", move |ctx| {
            let gate = Arc::clone(&gate_for_node);
            async move {
                // Runs before the pause and must not repeat after it.
                let first = ctx
                    .run_node_with("worker", json!({}), RunNodeOptions::with_run_id("first"))
                    .await?;

                if !gate.load(Ordering::SeqCst) {
                    return Ok(NodeOutput::interrupt("waiting for approval"));
                }

                // Runs only after the resume.
                let second = ctx
                    .run_node_with("worker", json!({}), RunNodeOptions::with_run_id("second"))
                    .await?;
                Ok(NodeOutput::new().with_update("first", first).with_update("second", second))
            }
        })
        .add_node_fn("worker", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                let call = counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("call", json!(call)))
            }
        })
        .add_edge(START, "supervisor")
        .add_edge("supervisor", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    // First run: the first child completes, then the node pauses.
    let outcome = graph.invoke(State::new(), ExecutionConfig::new("children-3")).await;
    assert!(matches!(outcome, Err(GraphError::Interrupted(_))));
    assert_eq!(worker_calls.load(Ordering::SeqCst), 1);

    // Resume: the parent re-runs from the top but the first child is served from
    // the ledger, so only the second one executes.
    gate_open.store(true, Ordering::SeqCst);
    let state = graph.invoke(State::new(), ExecutionConfig::new("children-3")).await.unwrap();

    assert_eq!(
        worker_calls.load(Ordering::SeqCst),
        2,
        "the resumed parent must not run the child that already completed"
    );
    assert_eq!(
        state.get("first").and_then(|v| v.get("call")),
        Some(&json!(0)),
        "the recorded output must be the one from the first run"
    );
    assert_eq!(state.get("second").and_then(|v| v.get("call")), Some(&json!(1)));
}

/// A failed child is not recorded, so it runs again.
#[tokio::test]
async fn a_failed_child_is_retried_on_the_next_attempt() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);

    let graph = StateGraph::with_channels(&["out"])
        .add_node_fn("parent", |ctx| async move {
            // Tolerate the child's failure so the run itself continues.
            let outcome = ctx.run_node("flaky", json!({})).await;
            Ok(NodeOutput::new().with_update("out", json!(outcome.is_err())))
        })
        .add_node_fn("flaky", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(GraphError::NodeExecutionFailed {
                    node: "flaky".to_string(),
                    message: "boom".to_string(),
                })
            }
        })
        .add_edge(START, "parent")
        .add_edge("parent", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("children-4")).await.unwrap();
    assert_eq!(state.get("out"), Some(&json!(true)), "the failure must reach the parent");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Invoking a name that is not a node is an error naming it.
#[tokio::test]
async fn invoking_an_unknown_child_is_an_error() {
    let graph = StateGraph::with_channels(&["out"])
        .add_node_fn("parent", |ctx| async move {
            let outcome = ctx.run_node("nope", json!({})).await;
            let message = match outcome {
                Err(error) => error.to_string(),
                Ok(_) => "unexpectedly succeeded".to_string(),
            };
            Ok(NodeOutput::new().with_update("out", json!(message)))
        })
        .add_edge(START, "parent")
        .add_edge("parent", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("children-5")).await.unwrap();
    let message = state.get("out").and_then(|v| v.as_str()).unwrap_or("");
    assert!(message.contains("nope"), "the error must name the child, got {message:?}");
}

/// A parent in a loop runs its child on every pass. The ledger path held no
/// trace of the pass, so every later pass received the first pass's output.
#[tokio::test]
async fn a_parent_in_a_loop_runs_its_child_on_every_pass() {
    let child_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&child_runs);
    let echoes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&echoes);

    let graph = StateGraph::with_channels(&["iter"])
        .add_node_fn("child", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("echo", ctx.get("i").cloned().unwrap_or_default()))
            }
        })
        .add_node_fn("parent", move |ctx| {
            let seen = Arc::clone(&seen);
            async move {
                let iter = ctx.get("iter").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
                let out = ctx.run_node("child", json!({ "i": iter })).await?;
                seen.lock().unwrap().push(out.get("echo").cloned());
                Ok(NodeOutput::new().with_update("iter", json!(iter)))
            }
        })
        .add_edge(START, "parent")
        .add_conditional_edges(
            "parent",
            |s| {
                if s.get("iter").and_then(|v| v.as_i64()).unwrap_or(0) < 3 {
                    "again"
                } else {
                    "done"
                }
                .to_string()
            },
            [("again", "parent"), ("done", END)],
        )
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    graph.invoke(State::new(), ExecutionConfig::new("children-loop")).await.unwrap();
    let last = graph.checkpointer().unwrap().load("children-loop").await.unwrap().unwrap();

    assert_eq!(
        (child_runs.load(Ordering::SeqCst), echoes.lock().unwrap().clone()),
        (3, vec![Some(json!(1)), Some(json!(2)), Some(json!(3))])
    );
    assert!(last.child_ledger.is_empty(), "a completed step drops its parents' entries");
}

/// A retried parent is served the children that finished on an earlier attempt,
/// so a charge is not made twice when the notification after it fails once. The
/// child counter carried across attempts and gave the charge a new identity.
#[tokio::test]
async fn a_retried_parent_is_served_the_children_that_finished() {
    use adk_graph::retry::RetryPolicy;
    use std::time::Duration;

    let charges = Arc::new(AtomicUsize::new(0));
    let notifications = Arc::new(AtomicUsize::new(0));
    let (charge, notify) = (Arc::clone(&charges), Arc::clone(&notifications));
    let graph = StateGraph::with_channels(&["out"])
        .add_node_fn("charge", move |_| {
            let charge = Arc::clone(&charge);
            async move {
                charge.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("charged", json!(true)))
            }
        })
        .add_node_fn("notify", move |_| {
            let notify = Arc::clone(&notify);
            async move {
                if notify.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(GraphError::NodeExecutionFailed {
                        node: "notify".into(),
                        message: "transient".into(),
                    });
                }
                Ok(NodeOutput::new().with_update("notified", json!(true)))
            }
        })
        .add_node_fn("parent", |ctx| async move {
            ctx.run_node("charge", json!({})).await?;
            ctx.run_node("notify", json!({})).await?;
            Ok(NodeOutput::new().with_update("out", json!("done")))
        })
        .add_edge(START, "parent")
        .add_edge("parent", END)
        .compile()
        .unwrap()
        .with_node_retry(
            "parent",
            RetryPolicy::new(3).with_initial_delay(Duration::from_millis(1)).with_jitter(0.0),
        );

    let state = graph.invoke(State::new(), ExecutionConfig::new("children-retry")).await.unwrap();

    assert_eq!(
        (state.get("out"), charges.load(Ordering::SeqCst), notifications.load(Ordering::SeqCst)),
        (Some(&json!("done")), 1, 2)
    );
}

/// A child is retried under the policy set for it with `with_node_retry`.
#[tokio::test]
async fn a_child_is_retried_under_its_own_policy() {
    use adk_graph::retry::RetryPolicy;
    use std::time::Duration;

    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let graph = StateGraph::with_channels(&["out"])
        .add_node_fn("flaky", move |_| {
            let counter = Arc::clone(&counter);
            async move {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(GraphError::NodeExecutionFailed {
                        node: "flaky".into(),
                        message: "transient".into(),
                    });
                }
                Ok(NodeOutput::new().with_update("out", json!("ok")))
            }
        })
        .add_node_fn("parent", |ctx| async move {
            let out = ctx.run_node("flaky", json!({})).await?;
            Ok(NodeOutput::new().with_update("out", out.get("out").cloned().unwrap_or_default()))
        })
        .add_edge(START, "parent")
        .add_edge("parent", END)
        .compile()
        .unwrap()
        .with_node_retry(
            "flaky",
            RetryPolicy::new(3).with_initial_delay(Duration::from_millis(1)).with_jitter(0.0),
        );

    let state = graph.invoke(State::new(), ExecutionConfig::new("child-policy")).await.unwrap();

    assert_eq!((state.get("out"), attempts.load(Ordering::SeqCst)), (Some(&json!("ok")), 2));
}
