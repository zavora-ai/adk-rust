//! Work a graph re-enters runs again: a subgraph in a later pass of a loop or a
//! later turn, a child invoked by a node in a loop, and a second call of a graph
//! tool. Work the graph resumes is not repeated.
//!
//! Every case runs on each checkpointer, because each depends on what a
//! checkpoint stores: subgraph threads, the child ledger, and tool-call threads.
//! The in-memory checkpointer always runs, SQLite with `--features sqlite`, and
//! Redis with `--features redis` and a server at `REDIS_URL`:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-graph --features sqlite,redis --run-ignored all \
//!   --test integration -E 'test(/^rerun_semantics_tests::/)'
//! ```

use crate::support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::{Agent, Tool, ToolContext};
use adk_graph::agent::{GraphAgent, tool_call_thread_id};
use adk_graph::checkpoint::Checkpointer;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::subgraph::SubgraphNode;
use adk_graph::tool::NodeTool;
use adk_tool::SimpleToolContext;
use futures::StreamExt;
use serde_json::{Value, json};

/// A checkpointed subgraph that doubles `n`, pausing for approval when
/// `pause_at` equals `n` and `approved` is unset.
fn doubling_subgraph(
    runs: Arc<AtomicUsize>,
    pause_at: Option<i64>,
    checkpointer: Arc<dyn Checkpointer>,
) -> Arc<CompiledGraph> {
    Arc::new(
        StateGraph::with_channels(&["n", "doubled", "approved"])
            .add_node_fn("double", move |ctx| {
                let runs = Arc::clone(&runs);
                async move {
                    let n = ctx.get("n").and_then(Value::as_i64).unwrap_or(0);
                    if Some(n) == pause_at && ctx.get("approved").is_none() {
                        return Ok(NodeOutput::interrupt("approve the doubling"));
                    }
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutput::new().with_update("doubled", json!(n * 2)))
                }
            })
            .add_edge(START, "double")
            .add_edge("double", END)
            .compile()
            .expect("compile the subgraph")
            .with_checkpointer_arc(checkpointer),
    )
}

/// A parent that bumps `n`, runs the subgraph, and records what came back, for
/// three passes.
fn looping_parent(
    subgraph: Arc<CompiledGraph>,
    seen: Arc<Mutex<Vec<(i64, i64)>>>,
    checkpointer: Arc<dyn Checkpointer>,
) -> CompiledGraph {
    StateGraph::with_channels(&["n", "doubled", "approved"])
        .add_node_fn("bump", |ctx| async move {
            let n = ctx.get("n").and_then(Value::as_i64).unwrap_or(0) + 1;
            Ok(NodeOutput::new().with_update("n", json!(n)))
        })
        .add_node(SubgraphNode::new("sub", subgraph))
        .add_node_fn("record", move |ctx| {
            let seen = Arc::clone(&seen);
            async move {
                let n = ctx.get("n").and_then(Value::as_i64).unwrap_or(0);
                let doubled = ctx.get("doubled").and_then(Value::as_i64).unwrap_or(0);
                seen.lock().unwrap().push((n, doubled));
                Ok(NodeOutput::new())
            }
        })
        .add_edge(START, "bump")
        .add_edge("bump", "sub")
        .add_edge("sub", "record")
        .add_conditional_edges(
            "record",
            |s| {
                if s.get("n").and_then(Value::as_i64).unwrap_or(0) < 3 { "again" } else { "done" }
                    .to_string()
            },
            [("again", "bump"), ("done", END)],
        )
        .compile()
        .expect("compile the parent")
        .with_checkpointer_arc(checkpointer)
}

/// A checkpointed subgraph runs on every pass of its parent's loop. It ran on one
/// thread, so every later pass read the first pass's finished state.
async fn a_subgraph_runs_on_every_pass(checkpointer: Arc<dyn Checkpointer>) {
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let subgraph = doubling_subgraph(Arc::clone(&runs), None, Arc::clone(&checkpointer));
    let parent = looping_parent(subgraph, Arc::clone(&seen), checkpointer);

    let thread = format!("sub-loop-{}", uuid::Uuid::new_v4());
    parent.invoke(State::new(), ExecutionConfig::new(&thread)).await.expect("run");

    assert_eq!(
        (runs.load(Ordering::SeqCst), seen.lock().unwrap().clone()),
        (3, vec![(1, 2), (2, 4), (3, 6)])
    );
}

/// A pause inside the subgraph on the second pass resumes that pass's run of the
/// subgraph, and the third pass still runs it afresh.
async fn a_subgraph_paused_in_a_loop_resumes_its_own_pass(checkpointer: Arc<dyn Checkpointer>) {
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let subgraph = doubling_subgraph(Arc::clone(&runs), Some(2), Arc::clone(&checkpointer));
    let parent = looping_parent(subgraph, Arc::clone(&seen), checkpointer);

    let thread = format!("sub-pause-{}", uuid::Uuid::new_v4());
    let first = parent.invoke(State::new(), ExecutionConfig::new(&thread)).await;
    assert!(matches!(first, Err(GraphError::Interrupted(_))), "got {first:?}");

    let mut answer = State::new();
    answer.insert("approved".to_string(), json!(true));
    parent.invoke(answer, ExecutionConfig::new(&thread)).await.expect("resume");

    assert_eq!(
        (runs.load(Ordering::SeqCst), seen.lock().unwrap().clone()),
        (3, vec![(1, 2), (2, 4), (3, 6)])
    );
}

/// A later `GraphAgent` turn runs a checkpointed subgraph again.
async fn a_subgraph_runs_again_on_a_later_turn(checkpointer: Arc<dyn Checkpointer>) {
    let runs = Arc::new(AtomicUsize::new(0));
    let subgraph = doubling_subgraph(Arc::clone(&runs), None, Arc::clone(&checkpointer));
    let graph = StateGraph::with_channels(&["input", "n", "doubled", "output"])
        .add_node_fn("read", |ctx| async move {
            let n = ctx.get("input").and_then(Value::as_str).and_then(|t| t.parse::<i64>().ok());
            Ok(NodeOutput::new().with_update("n", json!(n.unwrap_or(0))))
        })
        .add_node(SubgraphNode::new("sub", subgraph))
        .add_node_fn("answer", |ctx| async move {
            let doubled = ctx.get("doubled").and_then(Value::as_i64).unwrap_or(0);
            Ok(NodeOutput::new().with_update("output", json!(doubled.to_string())))
        })
        .add_edge(START, "read")
        .add_edge("read", "sub")
        .add_edge("sub", "answer")
        .add_edge("answer", END)
        .compile()
        .expect("compile")
        .with_checkpointer_arc(checkpointer);
    let agent = GraphAgent::from_graph(&format!("turns-{}", uuid::Uuid::new_v4()), graph);
    let user = format!("user-{}", uuid::Uuid::new_v4());

    let mut answers = Vec::new();
    for text in ["4", "5"] {
        let events: Vec<_> = agent
            .run(support::test_context_for(&user, "session", text))
            .await
            .expect("run")
            .map(|event| event.expect("event"))
            .collect()
            .await;
        answers.extend(events.iter().filter_map(|event| {
            event
                .llm_response
                .content
                .as_ref()?
                .parts
                .iter()
                .find_map(|p| p.text().map(String::from))
        }));
    }

    assert_eq!((answers, runs.load(Ordering::SeqCst)), (vec!["8".into(), "10".into()], 2));
}

/// A node in a loop runs its child on every pass, and a pause in the second pass
/// does not repeat the child that pass already ran.
async fn a_child_runs_on_every_pass_and_once_across_a_pause(checkpointer: Arc<dyn Checkpointer>) {
    let child_runs = Arc::new(AtomicUsize::new(0));
    let approved = Arc::new(AtomicBool::new(false));
    let counter = Arc::clone(&child_runs);
    let gate = Arc::clone(&approved);
    let graph = StateGraph::with_channels(&["iter", "echoes"])
        .add_node_fn("child", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("echo", ctx.get("i").cloned().unwrap_or_default()))
            }
        })
        .add_node_fn("parent", move |ctx| {
            let gate = Arc::clone(&gate);
            async move {
                let iter = ctx.get("iter").and_then(Value::as_i64).unwrap_or(0) + 1;
                let out = ctx.run_node("child", json!({ "i": iter })).await?;
                if iter == 2 && !gate.load(Ordering::SeqCst) {
                    return Ok(NodeOutput::interrupt("approve the second pass"));
                }
                let mut echoes = ctx.get("echoes").cloned().unwrap_or_else(|| json!([]));
                if let Some(list) = echoes.as_array_mut() {
                    list.push(out.get("echo").cloned().unwrap_or_default());
                }
                Ok(NodeOutput::new().with_update("iter", json!(iter)).with_update("echoes", echoes))
            }
        })
        .add_edge(START, "parent")
        .add_conditional_edges(
            "parent",
            |s| {
                if s.get("iter").and_then(Value::as_i64).unwrap_or(0) < 3 {
                    "again"
                } else {
                    "done"
                }
                .to_string()
            },
            [("again", "parent"), ("done", END)],
        )
        .compile()
        .expect("compile")
        .with_checkpointer_arc(checkpointer);

    let thread = format!("children-{}", uuid::Uuid::new_v4());
    let first = graph.invoke(State::new(), ExecutionConfig::new(&thread)).await;
    assert!(matches!(first, Err(GraphError::Interrupted(_))), "got {first:?}");
    approved.store(true, Ordering::SeqCst);
    let state = graph.invoke(State::new(), ExecutionConfig::new(&thread)).await.expect("resume");

    assert_eq!(
        (state.get("echoes"), child_runs.load(Ordering::SeqCst)),
        (Some(&json!([1, 2, 3])), 3),
        "one child run per pass, the second served from the ledger after the pause"
    );
}

/// Each call of a checkpointed graph tool runs on its own thread, so a second call
/// in the session runs the graph, and the same call dispatched again resumes it.
async fn tool_calls_run_on_their_own_threads(checkpointer: Arc<dyn Checkpointer>) {
    let runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&runs);
    let graph = Arc::new(
        StateGraph::with_channels(&["input", "doubled", "approved"])
            .add_node_fn("double", move |ctx| {
                let counter = Arc::clone(&counter);
                async move {
                    let input = ctx.get("input").and_then(Value::as_i64).unwrap_or(0);
                    if input > 100 && ctx.get("approved").is_none() {
                        return Ok(NodeOutput::interrupt("approve a large input"));
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutput::new().with_update("doubled", json!(input * 2)))
                }
            })
            .add_edge(START, "double")
            .add_edge("double", END)
            .compile()
            .expect("compile")
            .with_checkpointer_arc(checkpointer),
    );
    let tool = NodeTool::for_graph(graph);
    let session = format!("session-{}", uuid::Uuid::new_v4());
    let call = |id: &str| {
        Arc::new(
            SimpleToolContext::new("tools")
                .with_session_id(session.clone())
                .with_function_call_id(id),
        ) as Arc<dyn ToolContext>
    };

    let first = tool.execute(call("call-1"), json!({ "input": 21 })).await.expect("first call");
    let second = tool.execute(call("call-2"), json!({ "input": 5 })).await.expect("second call");
    assert_eq!(
        (first.get("doubled"), second.get("doubled"), runs.load(Ordering::SeqCst)),
        (Some(&json!(42)), Some(&json!(10)), 2)
    );

    let paused = tool.execute(call("call-3"), json!({ "input": 200 })).await.expect("pause");
    let thread = paused.pointer("/interrupt/thread_id").and_then(Value::as_str);
    let expected = tool_call_thread_id("tools", "anonymous", &session, "call-3");
    let resumed = tool.execute(call("call-3"), json!({ "approved": true })).await.expect("resume");
    assert_eq!(
        (
            paused.get("status"),
            thread == Some(expected.as_str()),
            resumed.get("doubled"),
            runs.load(Ordering::SeqCst)
        ),
        (Some(&json!("interrupted")), true, Some(&json!(400)), 3),
        "paused: {paused}"
    );
}

#[cfg(feature = "sqlite")]
async fn sqlite() -> Arc<dyn Checkpointer> {
    let path = std::env::temp_dir().join(format!("adk-graph-rerun-{}.db", uuid::Uuid::new_v4()));
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(30));
    let pool = sqlx::SqlitePool::connect_with(options).await.expect("open sqlite");
    Arc::new(adk_graph::checkpoint::SqliteCheckpointer::from_pool(pool).await.expect("schema"))
}

#[cfg(feature = "redis")]
async fn redis() -> Arc<dyn Checkpointer> {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
    let checkpointer = adk_graph::RedisCheckpointer::new(&url).await.expect("connect to redis");
    checkpointer.migrate().await.expect("load scripts");
    Arc::new(checkpointer)
}

macro_rules! rerun_cases {
    ($backend:ident, $open:expr $(, #[$attr:meta])*) => {
        mod $backend {
            #[allow(unused_imports)]
            use super::*;

            $(#[$attr])*
            #[tokio::test]
            async fn a_subgraph_runs_on_every_pass() {
                super::a_subgraph_runs_on_every_pass($open).await;
            }

            $(#[$attr])*
            #[tokio::test]
            async fn a_subgraph_paused_in_a_loop_resumes_its_own_pass() {
                super::a_subgraph_paused_in_a_loop_resumes_its_own_pass($open).await;
            }

            $(#[$attr])*
            #[tokio::test]
            async fn a_subgraph_runs_again_on_a_later_turn() {
                super::a_subgraph_runs_again_on_a_later_turn($open).await;
            }

            $(#[$attr])*
            #[tokio::test]
            async fn a_child_runs_on_every_pass_and_once_across_a_pause() {
                super::a_child_runs_on_every_pass_and_once_across_a_pause($open).await;
            }

            $(#[$attr])*
            #[tokio::test]
            async fn tool_calls_run_on_their_own_threads() {
                super::tool_calls_run_on_their_own_threads($open).await;
            }
        }
    };
}

rerun_cases!(memory, Arc::new(adk_graph::checkpoint::MemoryCheckpointer::new()));

#[cfg(feature = "sqlite")]
rerun_cases!(sqlite, super::sqlite().await);

#[cfg(feature = "redis")]
rerun_cases!(redis, super::redis().await, #[ignore = "requires a Redis server at REDIS_URL"]);
