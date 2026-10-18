//! How a super-step dispatches its nodes.
//!
//! A step that failed still admitted its queued nodes, so their side effects ran,
//! and still waited for every sibling's retry backoff. A node reached from `START`
//! and from another node ran twice, once without its predecessor's output. Only
//! the first of several pauses in a step reached the caller. Two concurrent child
//! invocations under one run id both ran the child.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use adk_core::Agent;
use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::child::RunNodeOptions;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::interrupt::GraphInterruptPayload;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::retry::RetryPolicy;
use adk_graph::state::State;
use adk_graph::stream::{StreamEvent, StreamMode};
use futures::StreamExt;
use serde_json::json;

fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

/// `a_slow` takes 200 ms and succeeds, `b_fails` fails after 20 ms, `c_flaky`
/// fails at once and retries after 2 s, and `d_ship` is queued behind them.
fn failing_step(ships: Arc<AtomicUsize>, flaky: Arc<AtomicUsize>) -> CompiledGraph {
    StateGraph::with_channels(&["slow", "shipped", "flaky"])
        .add_node_fn("a_slow", |_| async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(NodeOutput::new().with_update("slow", json!("done")))
        })
        .add_node_fn("b_fails", |_| async {
            // After c_flaky has started, so the failure meets it in its backoff.
            tokio::time::sleep(Duration::from_millis(20)).await;
            Err(GraphError::NodeExecutionFailed {
                node: "b_fails".to_string(),
                message: "card declined".to_string(),
            })
        })
        .add_node_fn("c_flaky", move |_| {
            let flaky = Arc::clone(&flaky);
            async move {
                flaky.fetch_add(1, Ordering::SeqCst);
                Err(GraphError::NodeExecutionFailed {
                    node: "c_flaky".to_string(),
                    message: "upstream overloaded".to_string(),
                })
            }
        })
        .add_node_fn("d_ship", move |_| {
            let ships = Arc::clone(&ships);
            async move {
                ships.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("shipped", json!(true)))
            }
        })
        .add_edge(START, "a_slow")
        .add_edge(START, "b_fails")
        .add_edge(START, "c_flaky")
        .add_edge(START, "d_ship")
        .add_edge("a_slow", END)
        .add_edge("b_fails", END)
        .add_edge("c_flaky", END)
        .add_edge("d_ship", END)
        .compile()
        .unwrap()
        .with_max_concurrency(3)
        .with_node_retry(
            "c_flaky",
            RetryPolicy::new(3).with_initial_delay(Duration::from_secs(2)).with_jitter(0.0),
        )
}

#[tokio::test]
async fn a_failed_step_admits_no_queued_node_and_skips_retry_backoff() {
    let (ships, flaky) = (counter(), counter());
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = failing_step(Arc::clone(&ships), Arc::clone(&flaky))
        .with_checkpointer_arc(Arc::clone(&checkpointer) as Arc<dyn Checkpointer>);

    let started = Instant::now();
    let outcome = graph.invoke(State::new(), ExecutionConfig::new("failed-step")).await;
    let elapsed = started.elapsed();

    assert!(
        matches!(&outcome, Err(GraphError::NodeExecutionFailed { node, .. }) if node == "b_fails"),
        "{outcome:?}"
    );
    assert_eq!(ships.load(Ordering::SeqCst), 0, "a node queued behind the failure must not run");
    assert_eq!(flaky.load(Ordering::SeqCst), 1, "the sibling must not wait out its backoff");
    assert!(elapsed < Duration::from_secs(1), "the step ended after {elapsed:?}");

    // The node that was running finished, and its write is kept for the resume.
    let latest = checkpointer.load("failed-step").await.unwrap().expect("a checkpoint");
    assert_eq!(latest.pending_writes.keys().collect::<Vec<_>>(), ["a_slow"]);
    assert_eq!(latest.attempts.get("c_flaky"), Some(&1));
}

#[tokio::test]
async fn a_failure_an_error_handler_recovers_does_not_stop_the_step() {
    let (ships, flaky) = (counter(), counter());
    let graph = failing_step(Arc::clone(&ships), Arc::clone(&flaky))
        .with_node_error_handler("b_fails", |_, _, _| {
            Ok(NodeOutput::new().with_update("slow", json!("recovered")))
        })
        .with_node_retry(
            "c_flaky",
            RetryPolicy::new(2).with_initial_delay(Duration::from_millis(10)).with_jitter(0.0),
        );

    let outcome = graph.invoke(State::new(), ExecutionConfig::new("recovered-step")).await;

    assert!(
        matches!(&outcome, Err(GraphError::NodeExecutionFailed { node, .. }) if node == "c_flaky"),
        "{outcome:?}"
    );
    assert_eq!(flaky.load(Ordering::SeqCst), 2, "the recovered failure left c_flaky's retry");
}

#[tokio::test]
async fn a_node_reached_from_start_and_a_predecessor_runs_once_after_it() {
    let composes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&composes);
    let graph = StateGraph::with_channels(&["status", "reply"])
        .add_node_fn("lookup", |_| async {
            Ok(NodeOutput::new().with_update("status", json!("shipped")))
        })
        .add_node_fn("compose", move |ctx| {
            let seen = Arc::clone(&seen);
            async move {
                let status = ctx.get("status").cloned();
                seen.lock().unwrap().push(status.clone());
                Ok(NodeOutput::new().with_update("reply", json!(format!("status: {status:?}"))))
            }
        })
        .add_edge(START, "lookup")
        .add_edge(START, "compose")
        .add_edge("lookup", "compose")
        .add_edge("compose", END)
        .compile()
        .unwrap();

    graph.invoke(State::new(), ExecutionConfig::new("entry-join")).await.unwrap();

    assert_eq!(composes.lock().unwrap().clone(), [Some(json!("shipped"))]);
}

#[tokio::test]
async fn a_loop_head_reached_from_start_starts_at_once() {
    let passes = counter();
    let runs = Arc::clone(&passes);
    let graph = StateGraph::with_channels(&["count"])
        .add_node_fn("head", move |ctx| {
            let runs = Arc::clone(&runs);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let count = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("count", json!(count + 1)))
            }
        })
        .add_node_fn("body", |_| async { Ok(NodeOutput::new()) })
        .add_edge(START, "head")
        .add_edge("head", "body")
        .add_conditional_edges(
            "body",
            |state| {
                let count = state.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                if count < 3 { "again".to_string() } else { END.to_string() }
            },
            [("again", "head"), (END, END)],
        )
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("loop-head")).await.unwrap();

    assert_eq!((passes.load(Ordering::SeqCst), state.get("count")), (3, Some(&json!(3))));
}

/// Two nodes of one step that both pause.
fn two_pauses() -> CompiledGraph {
    StateGraph::with_channels(&["legal", "finance"])
        .add_node_fn("legal", |_| async { Ok(NodeOutput::interrupt("legal review")) })
        .add_node_fn("finance", |_| async { Ok(NodeOutput::interrupt("finance review")) })
        .add_edge(START, "legal")
        .add_edge(START, "finance")
        .add_edge("legal", END)
        .add_edge("finance", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
}

fn messages(interrupted: &adk_graph::error::InterruptedExecution) -> Vec<String> {
    interrupted.interrupts.iter().map(ToString::to_string).collect()
}

#[tokio::test]
async fn every_pause_of_a_step_is_reported() {
    let graph = two_pauses();

    let Err(GraphError::Interrupted(interrupted)) =
        graph.invoke(State::new(), ExecutionConfig::new("two-pauses")).await
    else {
        panic!("the step must pause");
    };

    assert_eq!(
        messages(&interrupted),
        ["Dynamic interrupt: finance review", "Dynamic interrupt: legal review"]
    );
    assert_eq!(interrupted.interrupt.to_string(), "Dynamic interrupt: finance review");
}

#[tokio::test]
async fn every_pause_of_a_step_reaches_a_stream_in_each_mode() {
    for mode in [StreamMode::Updates, StreamMode::Messages] {
        let graph = two_pauses();
        let events: Vec<StreamEvent> = graph
            .stream(State::new(), ExecutionConfig::new("two-pauses-stream"), mode)
            .map(|event| event.expect("event"))
            .collect()
            .await;
        let pauses: Vec<(String, String)> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::Interrupted { node, message } => Some((node.clone(), message.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            pauses,
            [
                ("finance".to_string(), "Dynamic interrupt: finance review".to_string()),
                ("legal".to_string(), "Dynamic interrupt: legal review".to_string()),
            ],
            "{mode:?}"
        );
    }
}

#[tokio::test]
async fn a_graph_agent_yields_one_event_per_pause() {
    let agent = adk_graph::agent::GraphAgent::from_graph("reviews", two_pauses());

    let events: Vec<_> = agent
        .run(support::test_context_with_text("start"))
        .await
        .unwrap()
        .map(|event| event.expect("event"))
        .collect()
        .await;

    let pauses: Vec<Option<String>> = events
        .iter()
        .filter_map(GraphInterruptPayload::from_event)
        .map(|pause| pause.message)
        .collect();
    assert_eq!(pauses, [Some("finance review".to_string()), Some("legal review".to_string())]);
}

#[tokio::test]
async fn concurrent_children_under_one_run_id_run_once() {
    let charges = counter();
    let runs = Arc::clone(&charges);
    let graph = StateGraph::with_channels(&["first", "second", "receipt"])
        .add_node_fn("charge", move |_| {
            let runs = Arc::clone(&runs);
            async move {
                let charge = runs.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(NodeOutput::new().with_update("receipt", json!(format!("R-{charge}"))))
            }
        })
        .add_node_fn("checkout", |ctx| async move {
            let order = json!({"order": "A-7"});
            let (first, second) = futures::join!(
                ctx.run_node_with("charge", order.clone(), RunNodeOptions::with_run_id("A-7")),
                ctx.run_node_with("charge", order, RunNodeOptions::with_run_id("A-7")),
            );
            Ok(NodeOutput::new().with_update("first", first?).with_update("second", second?))
        })
        .add_edge(START, "checkout")
        .add_edge("checkout", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("single-flight")).await.unwrap();

    assert_eq!(charges.load(Ordering::SeqCst), 1);
    assert_eq!(state.get("first"), state.get("second"));
}
