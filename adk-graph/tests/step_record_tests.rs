//! Run, step and lineage records in checkpoint metadata (spec 3.3 §12, developer track).

use adk_core::{Agent, Content, InvocationContext, Part, RunConfig, Session};
use adk_graph::agent::{GraphAgent, session_thread_id};
use adk_graph::checkpoint::{Checkpointer, Durability, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::record::{
    LINEAGE_METADATA_KEY, NodeStatus, RUN_METADATA_KEY, STEPS_METADATA_KEY, StepKind, StepRecord,
};
use adk_graph::state::{Checkpoint, State};
use futures::StreamExt;
use proptest::prelude::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The parts of a step record a test can predict: no clock readings.
type Outline = Vec<(usize, StepKind, Vec<(String, NodeStatus, Vec<String>, u32)>)>;

fn outline(records: &[StepRecord]) -> Outline {
    records
        .iter()
        .map(|step| {
            let nodes = step
                .nodes
                .iter()
                .map(|node| (node.node.clone(), node.status, node.writes.clone(), node.attempt))
                .collect();
            (step.step, step.kind, nodes)
        })
        .collect()
}

fn writes(node: &str, keys: &[&str]) -> (String, NodeStatus, Vec<String>, u32) {
    (node.to_string(), NodeStatus::Done, keys.iter().map(|k| k.to_string()).collect(), 1)
}

/// Every step record of a thread, in checkpoint order.
async fn thread_steps(checkpointer: &dyn Checkpointer, thread: &str) -> Vec<StepRecord> {
    let mut steps = Vec::new();
    for checkpoint in checkpointer.list(thread).await.unwrap() {
        steps.extend(checkpoint.step_records().unwrap());
    }
    steps
}

fn invoice_graph(checkpointer: Arc<dyn Checkpointer>) -> CompiledGraph {
    StateGraph::with_channels(&["invoice", "charge", "receipt"])
        .add_node_fn("draft", |_ctx| async {
            Ok(NodeOutput::new().with_update("invoice", json!({ "amountCents": 4200 })))
        })
        .add_node_fn("pay", |_ctx| async {
            Ok(NodeOutput::new().with_update("charge", json!({ "id": "ch_1" })))
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

#[tokio::test]
async fn each_completed_checkpoint_records_its_step_and_run() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = invoice_graph(checkpointer.clone());
    graph.invoke(State::new(), ExecutionConfig::new("t1")).await.unwrap();

    let checkpoints = checkpointer.list("t1").await.unwrap();
    let per_checkpoint: Vec<(usize, Outline)> = checkpoints
        .iter()
        .map(|checkpoint| (checkpoint.step, outline(&checkpoint.step_records().unwrap())))
        .collect();
    assert_eq!(
        per_checkpoint,
        vec![
            (1, vec![(1, StepKind::Completed, vec![writes("draft", &["invoice"])])]),
            (2, vec![(2, StepKind::Completed, vec![writes("pay", &["charge"])])]),
            (3, vec![(3, StepKind::Completed, vec![writes("receipt", &["receipt"])])]),
        ]
    );
    let runs: BTreeSet<String> = checkpoints
        .iter()
        .map(|checkpoint| checkpoint.run_record().unwrap().expect("a run").run_id)
        .collect();
    assert_eq!(runs.len(), 1, "one run covers every step: {runs:?}");
    assert!(checkpoints.iter().all(|checkpoint| checkpoint.lineage().unwrap().is_none()));
}

#[tokio::test]
async fn records_round_trip_through_the_memory_checkpointer() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = invoice_graph(checkpointer.clone());
    graph.invoke(State::new(), ExecutionConfig::new("t1")).await.unwrap();

    let saved = checkpointer.list("t1").await.unwrap();
    let copy = MemoryCheckpointer::new();
    for checkpoint in &saved {
        copy.save(checkpoint).await.unwrap();
    }
    assert_eq!(copy.list("t1").await.unwrap(), saved);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn records_round_trip_through_the_sqlite_checkpointer() {
    let checkpointer =
        Arc::new(adk_graph::checkpoint::SqliteCheckpointer::in_memory().await.unwrap());
    let graph = invoice_graph(checkpointer.clone());
    graph.invoke(State::new(), ExecutionConfig::new("t1")).await.unwrap();

    let steps = thread_steps(checkpointer.as_ref(), "t1").await;
    assert_eq!(
        outline(&steps),
        vec![
            (1, StepKind::Completed, vec![writes("draft", &["invoice"])]),
            (2, StepKind::Completed, vec![writes("pay", &["charge"])]),
            (3, StepKind::Completed, vec![writes("receipt", &["receipt"])]),
        ]
    );
    let memory = MemoryCheckpointer::new();
    for checkpoint in checkpointer.list("t1").await.unwrap() {
        memory.save(&checkpoint).await.unwrap();
        assert_eq!(
            checkpointer.load_by_id(&checkpoint.checkpoint_id).await.unwrap(),
            memory.load_by_id(&checkpoint.checkpoint_id).await.unwrap()
        );
    }
}

#[tokio::test]
async fn parallel_nodes_record_their_own_durations() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["fast", "slow"])
        .add_node_fn("fast", |_ctx| async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(NodeOutput::new().with_update("fast", json!(true)))
        })
        .add_node_fn("slow", |_ctx| async {
            tokio::time::sleep(Duration::from_millis(250)).await;
            Ok(NodeOutput::new().with_update("slow", json!(true)))
        })
        .add_edge(START, "fast")
        .add_edge(START, "slow")
        .add_edge("fast", END)
        .add_edge("slow", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer.clone());
    graph.invoke(State::new(), ExecutionConfig::new("t1")).await.unwrap();

    let steps = thread_steps(checkpointer.as_ref(), "t1").await;
    assert_eq!(
        outline(&steps),
        vec![(1, StepKind::Completed, vec![writes("fast", &["fast"]), writes("slow", &["slow"])])]
    );
    let (fast, slow) = (&steps[0].nodes[0], &steps[0].nodes[1]);
    assert!(fast.duration_ms >= 20 && fast.duration_ms < 200, "fast took {}", fast.duration_ms);
    assert!(slow.duration_ms >= 250, "slow took {}", slow.duration_ms);
    // Both started together, so their start times are within the fast node's run.
    assert!((slow.started_at - fast.started_at).num_milliseconds().abs() < 200);
}

#[tokio::test]
async fn exit_durability_writes_every_step_in_its_one_checkpoint() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = invoice_graph(checkpointer.clone());
    graph
        .invoke(State::new(), ExecutionConfig::new("t1").with_durability(Durability::Exit))
        .await
        .unwrap();

    let checkpoints = checkpointer.list("t1").await.unwrap();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(
        outline(&checkpoints[0].step_records().unwrap()),
        vec![
            (1, StepKind::Completed, vec![writes("draft", &["invoice"])]),
            (2, StepKind::Completed, vec![writes("pay", &["charge"])]),
            (3, StepKind::Completed, vec![writes("receipt", &["receipt"])]),
        ]
    );
}

#[tokio::test]
async fn a_pause_and_the_completed_checkpoint_before_it_share_a_step_and_differ_in_kind() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["invoice", "answer"])
        .add_node_fn("draft", |_ctx| async {
            Ok(NodeOutput::new().with_update("invoice", json!(1)))
        })
        .add_node_fn("review", |ctx| async move {
            match ctx.resume_value() {
                Some(answer) => Ok(NodeOutput::new().with_update("answer", answer.clone())),
                None => Ok(NodeOutput::interrupt("approve?")),
            }
        })
        .add_edge(START, "draft")
        .add_edge("draft", "review")
        .add_edge("review", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer.clone());
    let paused = graph.invoke(State::new(), ExecutionConfig::new("t1")).await;
    assert!(paused.is_err(), "the review pauses");

    let checkpoints = checkpointer.list("t1").await.unwrap();
    let summary: Vec<(usize, bool, Outline)> = checkpoints
        .iter()
        .map(|c| (c.step, c.is_paused(), outline(&c.step_records().unwrap())))
        .collect();
    assert_eq!(
        summary,
        vec![
            (1, false, vec![(1, StepKind::Completed, vec![writes("draft", &["invoice"])])]),
            (
                1,
                true,
                vec![(
                    2,
                    StepKind::Paused,
                    vec![("review".to_string(), NodeStatus::Paused, Vec::new(), 1)]
                )]
            ),
        ]
    );
    assert_ne!(checkpoints[0].checkpoint_id, checkpoints[1].checkpoint_id);

    graph
        .invoke(State::new(), ExecutionConfig::new("t1").with_resume_value("review", json!("yes")))
        .await
        .unwrap();
    let last = checkpointer.load("t1").await.unwrap().unwrap();
    assert_eq!(
        outline(&last.step_records().unwrap()),
        vec![(2, StepKind::Completed, vec![writes("review", &["answer"])])]
    );
    assert_eq!(last.run_record().unwrap(), checkpoints[1].run_record().unwrap());
}

#[tokio::test]
async fn a_failed_step_records_the_failure_and_the_nodes_that_did_not_start() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["a"])
        .add_node_fn("a", |_ctx| async { Ok(NodeOutput::new().with_update("a", json!(1))) })
        .add_node_fn("boom", |_ctx| async {
            Err(adk_graph::GraphError::NodeExecutionFailed {
                node: "boom".to_string(),
                message: "card declined".to_string(),
            })
        })
        .add_edge(START, "a")
        .add_edge(START, "boom")
        .add_edge("a", END)
        .add_edge("boom", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer.clone());
    assert!(graph.invoke(State::new(), ExecutionConfig::new("t1")).await.is_err());

    // A finished sibling leaves a pending write, so the failure is checkpointed.
    let steps = thread_steps(checkpointer.as_ref(), "t1").await;
    assert_eq!(
        outline(&steps),
        vec![(
            1,
            StepKind::Failed,
            vec![writes("a", &["a"]), ("boom".to_string(), NodeStatus::Failed, Vec::new(), 1)]
        )]
    );
    assert_eq!(
        steps[0].nodes[1].error.as_deref(),
        Some("Node 'boom' execution failed: card declined")
    );
}

#[tokio::test]
async fn a_checkpoint_written_before_records_loads_and_resumes() {
    // A checkpoint as a release before spec 3.3 wrote it: no record keys.
    let fixture: Checkpoint = serde_json::from_value(json!({
        "thread_id": "old",
        "checkpoint_id": "old-1",
        "state": { "invoice": { "amountCents": 4200 } },
        "step": 1,
        "pending_nodes": ["pay"],
        "metadata": {},
        "created_at": "2026-09-01T09:14:00Z",
        "version": 1,
    }))
    .unwrap();
    assert_eq!(
        (
            fixture.run_record().unwrap(),
            fixture.step_records().unwrap(),
            fixture.lineage().unwrap()
        ),
        (None, Vec::new(), None)
    );

    let checkpointer = Arc::new(MemoryCheckpointer::new());
    checkpointer.save(&fixture).await.unwrap();
    let graph = invoice_graph(checkpointer.clone());
    graph.invoke(State::new(), ExecutionConfig::new("old")).await.unwrap();

    let checkpoints = checkpointer.list("old").await.unwrap();
    assert_eq!(
        outline(&thread_steps(checkpointer.as_ref(), "old").await),
        vec![
            (2, StepKind::Completed, vec![writes("pay", &["charge"])]),
            (3, StepKind::Completed, vec![writes("receipt", &["receipt"])]),
        ]
    );
    // The resumed run opens a run of its own, carried by every later checkpoint.
    assert!(checkpoints[1].run_record().unwrap().is_some());
    assert_eq!(checkpoints[1].run_record().unwrap(), checkpoints[2].run_record().unwrap());
}

#[tokio::test]
async fn update_state_carries_the_records() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = invoice_graph(checkpointer.clone()).with_interrupt_before(&["pay"]);
    assert!(graph.invoke(State::new(), ExecutionConfig::new("t1")).await.is_err());

    let before = checkpointer.load("t1").await.unwrap().unwrap();
    graph
        .update_state("t1", [("invoice".to_string(), json!({ "amountCents": 5000 }))])
        .await
        .unwrap();
    let after = checkpointer.load("t1").await.unwrap().unwrap();

    let records = |checkpoint: &Checkpoint| -> BTreeMap<String, Value> {
        [RUN_METADATA_KEY, STEPS_METADATA_KEY, LINEAGE_METADATA_KEY]
            .into_iter()
            .filter_map(|key| checkpoint.metadata.get(key).map(|v| (key.to_string(), v.clone())))
            .collect()
    };
    assert_eq!(records(&after), records(&before));
    assert_eq!(records(&before).len(), 2, "run and steps, no lineage: {:?}", records(&before));
}

// ── Run ids across turns ─────────────────────────────────────────────

/// A caller turn with its own invocation id, user message and run config.
struct Turn {
    invocation: String,
    content: Content,
    session: TurnSession,
    run_config: RunConfig,
}

struct TurnSession;

impl Session for TurnSession {
    fn id(&self) -> &str {
        "s1"
    }
    fn app_name(&self) -> &str {
        "shop"
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn state(&self) -> &dyn adk_core::State {
        &EmptyState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct EmptyState;

impl adk_core::State for EmptyState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

fn turn(invocation: &str, text: &str) -> Arc<dyn InvocationContext> {
    Arc::new(Turn {
        invocation: invocation.to_string(),
        content: Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: text.to_string() }],
        },
        session: TurnSession,
        run_config: RunConfig::default(),
    })
}

impl adk_core::ReadonlyContext for Turn {
    fn invocation_id(&self) -> &str {
        &self.invocation
    }
    fn agent_name(&self) -> &str {
        "invoice-run"
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn app_name(&self) -> &str {
        "shop"
    }
    fn session_id(&self) -> &str {
        "s1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait::async_trait]
impl adk_core::CallbackContext for Turn {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait::async_trait]
impl InvocationContext for Turn {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not exercised")
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        &self.run_config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn the_run_survives_an_answer_turn_and_a_restart_opens_a_new_one() {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["input", "answer"])
        .add_node_fn("review", |ctx| async move {
            match ctx.resume_value() {
                Some(answer) => Ok(NodeOutput::new().with_update("answer", answer.clone())),
                None => Ok(NodeOutput::interrupt("approve?")),
            }
        })
        .add_edge(START, "review")
        .add_edge("review", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer.clone());
    let agent = GraphAgent::from_graph("invoice-run", graph);
    let thread = session_thread_id("shop", "alice", "s1");

    for (invocation, text) in [("inv-1", "bill acme"), ("inv-2", "yes"), ("inv-3", "again")] {
        let mut events = agent.run(turn(invocation, text)).await.unwrap();
        while let Some(event) = events.next().await {
            event.unwrap();
        }
    }

    let runs: Vec<(String, Outline)> = checkpointer
        .list(&thread)
        .await
        .unwrap()
        .iter()
        .map(|c| (c.run_record().unwrap().unwrap().run_id, outline(&c.step_records().unwrap())))
        .collect();
    let review = |status, keys: &[&str]| {
        vec![("review".to_string(), status, keys.iter().map(|k| k.to_string()).collect(), 1)]
    };
    assert_eq!(
        runs,
        vec![
            // Turn 1 pauses; turn 2 answers it in a new invocation, on the same run.
            ("inv-1".to_string(), vec![(1, StepKind::Paused, review(NodeStatus::Paused, &[]))]),
            (
                "inv-1".to_string(),
                vec![(1, StepKind::Completed, review(NodeStatus::Done, &["answer"]))]
            ),
            // Turn 3 starts the finished thread again from its entry: a new run.
            ("inv-3".to_string(), vec![(2, StepKind::Paused, review(NodeStatus::Paused, &[]))]),
        ]
    );
}

// ── Property: recorded nodes are the executed nodes ──────────────────

/// A tree of nodes: node `i > 0` hangs off an earlier node, and writes some channels.
#[derive(Debug, Clone)]
struct TreeSpec {
    parents: Vec<usize>,
    writes: Vec<BTreeSet<usize>>,
}

fn tree_spec() -> impl Strategy<Value = TreeSpec> {
    (2usize..=6).prop_flat_map(|count| {
        let parents = (1..count).map(|i| 0..i).collect::<Vec<_>>();
        let writes =
            proptest::collection::vec(proptest::collection::btree_set(0usize..4, 0..=3), count);
        (parents, writes).prop_map(|(parents, writes)| TreeSpec { parents, writes })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn recorded_nodes_are_the_executed_nodes_and_writes_are_updated_keys(spec in tree_spec()) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let channels = ["c0", "c1", "c2", "c3"];
            let executed: Arc<Mutex<BTreeMap<usize, BTreeSet<String>>>> = Arc::default();
            let mut graph = StateGraph::with_channels(&channels);
            for (index, keys) in spec.writes.iter().enumerate() {
                let name = format!("n{index}");
                let keys: Vec<&'static str> = keys.iter().map(|k| channels[*k]).collect();
                let executed = Arc::clone(&executed);
                let node_name = name.clone();
                graph = graph.add_node_fn(&name, move |ctx| {
                    let keys = keys.clone();
                    let executed = Arc::clone(&executed);
                    let node_name = node_name.clone();
                    async move {
                        executed.lock().unwrap().entry(ctx.step + 1).or_default().insert(node_name.clone());
                        let mut output = NodeOutput::new();
                        for key in keys {
                            output = output.with_update(key, json!(node_name));
                        }
                        Ok(output)
                    }
                });
            }
            graph = graph.add_edge(START, "n0");
            let mut has_child = vec![false; spec.writes.len()];
            for (offset, parent) in spec.parents.iter().enumerate() {
                graph = graph.add_edge(&format!("n{parent}"), &format!("n{}", offset + 1));
                has_child[*parent] = true;
            }
            for (index, child) in has_child.iter().enumerate() {
                if !child {
                    graph = graph.add_edge(&format!("n{index}"), END);
                }
            }
            let checkpointer = Arc::new(MemoryCheckpointer::new());
            let graph = graph.compile().unwrap().with_checkpointer_arc(checkpointer.clone());
            graph.invoke(State::new(), ExecutionConfig::new("p")).await.unwrap();

            let mut recorded: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
            for checkpoint in checkpointer.list("p").await.unwrap() {
                for step in checkpoint.step_records().unwrap() {
                    prop_assert_eq!(step.kind, StepKind::Completed);
                    for node in &step.nodes {
                        let index: usize = node.node[1..].parse().unwrap();
                        let declared: Vec<String> =
                            spec.writes[index].iter().map(|k| channels[*k].to_string()).collect();
                        prop_assert_eq!(&node.writes, &declared);
                        for key in &node.writes {
                            prop_assert!(checkpoint.state.contains_key(key));
                        }
                        recorded.entry(step.step).or_default().insert(node.node.clone());
                    }
                }
            }
            prop_assert_eq!(recorded, executed.lock().unwrap().clone());
            Ok::<(), TestCaseError>(())
        })?;
    }
}
