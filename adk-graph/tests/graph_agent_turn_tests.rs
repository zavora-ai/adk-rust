//! Every `GraphAgent::run` is a new turn on the session's thread.
//!
//! The thread id is derived from the app name, user id, and session id, and a
//! finished run leaves a checkpoint with an empty frontier. With a checkpointer,
//! every later turn loaded that checkpoint, skipped the loop, and answered with
//! the first turn's output.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::{Agent, Event};
use adk_graph::agent::{GraphAgent, session_thread_id};
use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::node::NodeOutput;
use adk_graph::state::State;
use futures::StreamExt;
use serde_json::json;

async fn turn(agent: &GraphAgent, text: &str) -> Vec<Event> {
    agent
        .run(support::test_context_with_text(text))
        .await
        .expect("run")
        .map(|event| event.expect("event"))
        .collect()
        .await
}

fn text_of(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter().filter_map(|part| part.text()))
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn a_second_turn_runs_the_graph_again() {
    let runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&runs);
    let checkpointer = Arc::new(MemoryCheckpointer::new());

    let agent = GraphAgent::builder("echo")
        .channels(&["input", "output", "turns", "messages", "session_id"])
        .node_fn("echo", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let input = ctx.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let turns = ctx.get("turns").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new()
                    .with_update("output", json!(format!("echo: {input}")))
                    .with_update("turns", json!(turns + 1)))
            }
        })
        .edge(START, "echo")
        .edge("echo", END)
        .checkpointer_arc(Arc::clone(&checkpointer) as Arc<dyn Checkpointer>)
        .build()
        .expect("build");

    let first = turn(&agent, "hello").await;
    let second = turn(&agent, "world").await;

    assert_eq!(
        (text_of(&first), text_of(&second)),
        (vec!["echo: hello".to_string()], vec!["echo: world".to_string()])
    );
    assert_eq!(runs.load(Ordering::SeqCst), 2, "each turn must execute the graph");

    // State carries across turns: the second turn started from the first's.
    let thread_id = session_thread_id("caller-app", "caller-user", "caller-session");
    let state = checkpointer.load(&thread_id).await.unwrap().unwrap().state;
    assert_eq!(state.get("turns"), Some(&json!(2)));
}

/// A paused thread still resumes on the next turn rather than starting over.
#[tokio::test]
async fn a_turn_after_a_pause_resumes_the_paused_run() {
    let prepares = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&prepares);

    let agent = GraphAgent::builder("approval")
        .channels(&["approved", "output"])
        .input_mapper(|ctx| {
            let text: String = ctx.user_content().parts.iter().filter_map(|p| p.text()).collect();
            State::from([("approved".to_string(), json!(text == "approve"))])
        })
        .node_fn("prepare", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new())
            }
        })
        .node_fn("gate", |ctx| async move {
            if ctx.get("approved") != Some(&json!(true)) {
                return Ok(NodeOutput::interrupt("needs approval"));
            }
            Ok(NodeOutput::new().with_update("output", json!("approved")))
        })
        .edge(START, "prepare")
        .edge("prepare", "gate")
        .edge("gate", END)
        .checkpointer(MemoryCheckpointer::new())
        .build()
        .expect("build");

    let paused = turn(&agent, "start").await;
    let resumed = turn(&agent, "approve").await;

    assert_eq!(
        (
            paused.iter().map(|event| event.invocation_id.as_str()).collect::<Vec<_>>(),
            text_of(&resumed)
        ),
        (vec!["graph_interrupted"], vec!["approved".to_string()])
    );
    assert_eq!(prepares.load(Ordering::SeqCst), 1, "the resumed turn must not start over");
}

/// A session id is chosen by the caller, so it cannot alone identify a thread.
///
/// The thread was keyed by the bare session id, so a second user who named the
/// first user's session resumed the first user's paused run, with its state.
#[tokio::test]
async fn two_users_with_the_same_session_id_do_not_share_a_thread() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());

    let agent = GraphAgent::builder("vault")
        .channels(&["secret", "approved", "output"])
        .input_mapper(|ctx| {
            let text: String = ctx.user_content().parts.iter().filter_map(|p| p.text()).collect();
            match text.strip_prefix("secret:") {
                Some(secret) => State::from([("secret".to_string(), json!(secret))]),
                None => State::from([("approved".to_string(), json!(text == "approve"))]),
            }
        })
        .node_fn("gate", |ctx| async move {
            if ctx.get("approved") != Some(&json!(true)) {
                return Ok(NodeOutput::interrupt("needs approval"));
            }
            let secret = ctx.get("secret").cloned().unwrap_or(json!("none"));
            Ok(NodeOutput::new().with_update("output", secret))
        })
        .edge(START, "gate")
        .edge("gate", END)
        .checkpointer_arc(Arc::clone(&checkpointer) as Arc<dyn Checkpointer>)
        .build()
        .expect("build");

    let run = |user: &'static str, text: &'static str| {
        let agent = &agent;
        async move {
            agent
                .run(support::test_context_for(user, "shared-session", text))
                .await
                .expect("run")
                .map(|event| event.expect("event"))
                .collect::<Vec<_>>()
                .await
        }
    };

    let alice = run("alice", "secret:alice-card").await;
    let bob = run("bob", "approve").await;

    assert_eq!(
        (alice.iter().map(|event| event.invocation_id.as_str()).collect::<Vec<_>>(), text_of(&bob)),
        (vec!["graph_interrupted"], vec!["none".to_string()]),
        "bob must run his own thread, not resume alice's paused one"
    );

    let alice_thread = session_thread_id("caller-app", "alice", "shared-session");
    let alice_latest = checkpointer.load(&alice_thread).await.unwrap().expect("alice's checkpoint");
    assert_eq!(
        (alice_latest.pending_nodes, alice_latest.state.get("secret")),
        (vec!["gate".to_string()], Some(&json!("alice-card"))),
        "alice's run must still be paused with her state"
    );
}
