//! Forks with edits and lineage (spec 3.3 §12, developer track).

#![cfg(feature = "time-travel")]

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// `a` reaches `join` in one step; `b1 → b2` reaches it in two, so after the first
/// step `join` waits with `a`'s arrival recorded.
fn join_graph(
    checkpointer: Arc<dyn Checkpointer>,
    runs: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<serde_json::Value>>>,
) -> CompiledGraph {
    StateGraph::with_channels(&["x", "y", "z"])
        .add_node_fn("a", |_ctx| async { Ok(NodeOutput::new().with_update("x", json!(1))) })
        .add_node_fn("b1", |_ctx| async { Ok(NodeOutput::new().with_update("z", json!(true))) })
        .add_node_fn("b2", |_ctx| async { Ok(NodeOutput::new().with_update("y", json!(2))) })
        .add_node_fn("join", move |ctx| {
            let runs = Arc::clone(&runs);
            let seen = Arc::clone(&seen);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                seen.lock().unwrap().push(ctx.get("join_fan_in").cloned().unwrap_or_default());
                Ok(NodeOutput::new())
            }
        })
        .add_edge(START, "a")
        .add_edge(START, "b1")
        .add_edge("b1", "b2")
        .add_edge("a", "join")
        .add_edge("b2", "join")
        .add_edge("join", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer)
}

#[tokio::test]
async fn a_fork_at_a_waiting_join_fires_the_join_once_with_every_arrival() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let graph = join_graph(checkpointer.clone(), Arc::clone(&runs), Arc::clone(&seen));

    graph.invoke(State::new(), ExecutionConfig::new("t1")).await.unwrap();
    assert_eq!(runs.swap(0, Ordering::SeqCst), 1);
    seen.lock().unwrap().clear();
    // After step 1 the join waits for b2, with a's arrival recorded.
    graph.time_travel("t1").unwrap().fork_at(1, "t1-fork").await.unwrap();

    graph.invoke(State::new(), ExecutionConfig::new("t1-fork")).await.unwrap();
    assert_eq!(
        (runs.load(Ordering::SeqCst), seen.lock().unwrap().clone()),
        (1, vec![json!([{ "x": 1 }, { "y": 2 }])]),
        "the join fires once, with the arrival recorded before the fork"
    );
}

use adk_graph::record::Lineage;
use adk_graph::state::Checkpoint;
use adk_graph::time_travel::{ForkError, ForkSpec, fork_thread};

/// draft → pay → receipt; pay can be gated with `interrupt_before`.
fn invoice_graph(checkpointer: Arc<dyn Checkpointer>) -> CompiledGraph {
    StateGraph::with_channels(&["invoice", "note", "charge", "receipt"])
        .add_node_fn("draft", |_ctx| async {
            Ok(NodeOutput::new().with_update("invoice", json!({ "amountCents": 4200 })))
        })
        .add_node_fn("pay", |ctx| async move {
            let amount = ctx.get("invoice").cloned().unwrap_or_default()["amountCents"].clone();
            Ok(NodeOutput::new().with_update("charge", json!({ "amountCents": amount })))
        })
        .add_node_fn("receipt", |_ctx| async {
            Ok(NodeOutput::new().with_update("receipt", json!("sent")))
        })
        .add_edge(START, "draft")
        .add_edge("draft", "pay")
        .add_edge("pay", "receipt")
        .add_edge("receipt", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer)
}

/// Runs the invoice graph on `t1` and returns its checkpoints: after draft (c1), after
/// pay (c2), and the terminal one (c3).
async fn ran(checkpointer: &Arc<MemoryCheckpointer>) -> (CompiledGraph, Vec<Checkpoint>) {
    let graph = invoice_graph(checkpointer.clone());
    graph.invoke(State::new(), ExecutionConfig::new("t1")).await.unwrap();
    (graph, checkpointer.list("t1").await.unwrap())
}

#[tokio::test]
async fn a_fork_is_the_source_plus_its_edits_and_records_its_lineage() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let (graph, source) = ran(&checkpointer).await;
    let c1 = &source[0];

    let fork = graph
        .time_travel("t1")
        .unwrap()
        .fork(ForkSpec::new(&c1.checkpoint_id, "t2").set("note", json!("re-run for audit")))
        .await
        .unwrap();

    let mut expected = c1.state.clone();
    expected.insert("note".to_string(), json!("re-run for audit"));
    assert_eq!(
        (fork.thread_id.as_str(), &fork.state, fork.step, &fork.pending_nodes, fork.version),
        ("t2", &expected, c1.step, &c1.pending_nodes, 1)
    );
    assert_eq!(
        fork.lineage().unwrap(),
        Some(Lineage {
            root: "t1".to_string(),
            parent: "t1".to_string(),
            parent_checkpoint: c1.checkpoint_id.clone(),
            parent_step: 1,
            edited: vec!["note".to_string()],
            checked: true,
        })
    );
    // The fork opens its own run and has no steps until it runs.
    assert_eq!((fork.run_record().unwrap(), fork.step_records().unwrap()), (None, Vec::new()));
    assert_eq!(checkpointer.list("t1").await.unwrap(), source, "the source is unchanged");

    // Running the fork carries its lineage on every checkpoint, under a run of its own.
    graph.invoke(State::new(), ExecutionConfig::new("t2")).await.unwrap();
    let forked = checkpointer.list("t2").await.unwrap();
    assert!(forked.iter().all(|c| c.lineage().unwrap() == fork.lineage().unwrap()));
    assert_ne!(
        forked.last().unwrap().run_record().unwrap(),
        source.last().unwrap().run_record().unwrap()
    );
    assert_eq!(forked.last().unwrap().state["note"], json!("re-run for audit"));
}

#[tokio::test]
async fn an_edit_overwrites_a_whole_channel_without_its_reducer() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let schema = adk_graph::state::StateSchema::builder().list_channel("items").build();
    let source = Checkpoint::new(
        "t1",
        State::from([("items".to_string(), json!(["a", "b"]))]),
        1,
        vec!["next".to_string()],
    );
    checkpointer.save(&source).await.unwrap();

    let fork = fork_thread(
        checkpointer.as_ref(),
        ForkSpec::new(&source.checkpoint_id, "t2").set("items", json!(["c"])),
        Some(&schema),
    )
    .await
    .unwrap();
    assert_eq!(fork.state["items"], json!(["c"]));
}

#[tokio::test]
async fn edits_are_checked_against_the_channels_or_else_the_existing_keys() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let (graph, source) = ran(&checkpointer).await;
    let c1 = source[0].checkpoint_id.clone();
    let handle = graph.time_travel("t1").unwrap();

    let undeclared = handle.fork(ForkSpec::new(&c1, "t2").set("amount", json!(1))).await;
    assert!(matches!(undeclared, Err(ForkError::UndeclaredChannel { ref key }) if key == "amount"));

    // Without a schema only keys the checkpoint holds may change; `note` is declared
    // but was never written, so it is refused too.
    for key in ["amount", "note"] {
        let unchecked =
            fork_thread(checkpointer.as_ref(), ForkSpec::new(&c1, "t3").set(key, json!(1)), None)
                .await;
        assert!(matches!(unchecked, Err(ForkError::UnknownKey { key: ref k }) if k == key));
    }
    let accepted = fork_thread(
        checkpointer.as_ref(),
        ForkSpec::new(&c1, "t3").set("invoice", json!({ "amountCents": 5000 })),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        accepted.lineage().unwrap().map(|l| (l.edited, l.checked)),
        Some((vec!["invoice".to_string()], false))
    );
    // Refused forks wrote nothing.
    assert!(checkpointer.list("t2").await.unwrap().is_empty());
}

#[tokio::test]
async fn unknown_paused_terminal_and_foreign_checkpoints_are_refused() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let (graph, source) = ran(&checkpointer).await;
    let handle = graph.time_travel("t1").unwrap();

    let unknown = handle.fork(ForkSpec::new("nope", "t2")).await.unwrap_err();
    let terminal = handle.fork(ForkSpec::new(&source[2].checkpoint_id, "t2")).await.unwrap_err();

    let gated = invoice_graph(checkpointer.clone()).with_interrupt_before(&["pay"]);
    assert!(gated.invoke(State::new(), ExecutionConfig::new("g1")).await.is_err());
    let pause = checkpointer.load("g1").await.unwrap().unwrap();
    assert!(pause.is_paused());
    let paused = gated
        .time_travel("g1")
        .unwrap()
        .fork(ForkSpec::new(&pause.checkpoint_id, "t2"))
        .await
        .unwrap_err();
    let foreign = handle.fork(ForkSpec::new(&pause.checkpoint_id, "t2")).await.unwrap_err();
    let existing = handle.fork(ForkSpec::new(&source[0].checkpoint_id, "g1")).await.unwrap_err();

    assert_eq!(
        [&unknown, &terminal, &paused, &foreign, &existing].map(ForkError::code),
        [
            "fork.unknown_checkpoint",
            "fork.nothing_to_run",
            "fork.paused",
            "fork.other_thread",
            "fork.thread_exists",
        ]
    );
    assert!(terminal.to_string().contains("nothing runs after the last step"));
    assert_eq!(
        paused.to_string(),
        format!(
            "fork.paused: checkpoint '{}' is a pause, where a node waits mid-step; fork from \
             the completed checkpoint of its step",
            pause.checkpoint_id
        )
    );
    assert_eq!(
        ForkError::UnknownKey { key: "amount".to_string() }.to_string(),
        "fork.unknown_key: 'amount' is not a key of this checkpoint; without the graph's \
         channels only existing keys can be edited"
    );
    assert!(checkpointer.list("t2").await.unwrap().is_empty());
}

#[tokio::test]
async fn a_fork_of_a_fork_keeps_the_original_root() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let (graph, source) = ran(&checkpointer).await;
    graph
        .time_travel("t1")
        .unwrap()
        .fork(ForkSpec::new(&source[0].checkpoint_id, "t2"))
        .await
        .unwrap();
    graph.invoke(State::new(), ExecutionConfig::new("t2")).await.unwrap();
    let t2_after_pay = checkpointer.list("t2").await.unwrap()[1].clone();

    let grandchild = graph
        .time_travel("t2")
        .unwrap()
        .fork(ForkSpec::new(&t2_after_pay.checkpoint_id, "t3"))
        .await
        .unwrap();
    assert_eq!(
        grandchild.lineage().unwrap(),
        Some(Lineage {
            root: "t1".to_string(),
            parent: "t2".to_string(),
            parent_checkpoint: t2_after_pay.checkpoint_id.clone(),
            parent_step: t2_after_pay.step,
            edited: Vec::new(),
            checked: true,
        })
    );
}

#[tokio::test]
async fn fork_at_records_lineage_too() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let (graph, source) = ran(&checkpointer).await;
    graph.time_travel("t1").unwrap().fork_at(1, "t2").await.unwrap();

    let fork = checkpointer.load("t2").await.unwrap().unwrap();
    assert_eq!(
        fork.lineage().unwrap(),
        Some(Lineage {
            root: "t1".to_string(),
            parent: "t1".to_string(),
            parent_checkpoint: source[0].checkpoint_id.clone(),
            parent_step: 1,
            edited: Vec::new(),
            checked: true,
        })
    );
}
