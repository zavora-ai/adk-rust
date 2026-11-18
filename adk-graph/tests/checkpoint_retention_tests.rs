//! Tests for checkpoint retention.
//!
//! A long-running thread accumulates one checkpoint per super-step. Without a
//! policy that grows without bound. The invariant that matters most is that the
//! newest checkpoint is never discarded, because it is the one a resume loads.

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer, RetentionPolicy};
use adk_graph::edge::{END, START};
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::{Checkpoint, State};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// A chain of `steps` nodes, so the run leaves that many checkpoints.
fn chain(
    steps: usize,
    retention: Option<RetentionPolicy>,
) -> (CompiledGraph, Arc<MemoryCheckpointer>) {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let mut graph = StateGraph::with_channels(&["count"]);
    for index in 0..steps {
        let name: &'static str = Box::leak(format!("step{index}").into_boxed_str());
        graph = graph.add_node_fn(name, move |ctx| async move {
            let current = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("count", json!(current + 1)))
        });
    }
    let mut graph = graph.add_edge(START, "step0");
    for index in 0..steps.saturating_sub(1) {
        let from: &'static str = Box::leak(format!("step{index}").into_boxed_str());
        let to: &'static str = Box::leak(format!("step{}", index + 1).into_boxed_str());
        graph = graph.add_edge(from, to);
    }
    let last: &'static str = Box::leak(format!("step{}", steps - 1).into_boxed_str());
    let compiled = graph
        .add_edge(last, END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(Arc::clone(&checkpointer) as Arc<dyn Checkpointer>);

    let compiled = match retention {
        Some(policy) => compiled.with_checkpoint_retention(policy),
        None => compiled,
    };
    (compiled, checkpointer)
}

#[tokio::test]
async fn without_a_policy_every_checkpoint_is_kept() {
    let (graph, checkpointer) = chain(6, None);
    graph.invoke(State::new(), ExecutionConfig::new("unlimited")).await.unwrap();

    let kept = checkpointer.list("unlimited").await.unwrap();
    assert!(kept.len() > 3, "the whole history is kept, found {}", kept.len());
}

#[tokio::test]
async fn a_count_policy_keeps_only_the_newest() {
    let (graph, checkpointer) = chain(6, Some(RetentionPolicy::keep_last(2)));
    graph.invoke(State::new(), ExecutionConfig::new("counted")).await.unwrap();

    let kept = checkpointer.list("counted").await.unwrap();
    assert_eq!(kept.len(), 2, "the policy holds the thread at two");
}

#[tokio::test]
async fn the_newest_checkpoint_survives_the_tightest_policy() {
    // keep_last(0) is raised to 1: discarding the newest would end the thread.
    let policy = RetentionPolicy::keep_last(0);
    assert_eq!(policy.max_per_thread, Some(1));

    let (graph, checkpointer) = chain(5, Some(policy));
    let state = graph.invoke(State::new(), ExecutionConfig::new("tightest")).await.unwrap();

    let kept = checkpointer.list("tightest").await.unwrap();
    assert_eq!(kept.len(), 1, "exactly one is left");
    assert_eq!(
        checkpointer.load("tightest").await.unwrap().map(|c| c.state.get("count").cloned()),
        Some(state.get("count").cloned()),
        "and it is the newest, holding the final state"
    );
}

#[tokio::test]
async fn a_pruned_thread_still_resumes() {
    // The point of keeping the newest: a paused run must still continue.
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["first", "second"])
        .add_node_fn("one", |_ctx| async move {
            Ok(NodeOutput::new().with_update("first", json!(true)))
        })
        .add_node_fn("two", |_ctx| async move {
            Ok(NodeOutput::new().with_update("second", json!(true)))
        })
        .add_edge(START, "one")
        .add_edge("one", "two")
        .add_edge("two", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(Arc::clone(&checkpointer) as Arc<dyn Checkpointer>)
        .with_checkpoint_retention(RetentionPolicy::keep_last(1))
        .with_interrupt_before(&["two"]);

    let first = graph.invoke(State::new(), ExecutionConfig::new("pruned-resume")).await;
    assert!(first.is_err(), "the first run pauses");
    assert_eq!(checkpointer.list("pruned-resume").await.unwrap().len(), 1);

    let state = graph
        .invoke(State::new(), ExecutionConfig::new("pruned-resume"))
        .await
        .expect("a pruned thread must still resume");
    assert_eq!(state.get("second"), Some(&json!(true)));
}

#[tokio::test]
async fn an_age_policy_discards_by_timestamp() {
    // Built by hand, because a real run's checkpoints are all seconds old.
    let checkpointer = MemoryCheckpointer::new();
    let old = {
        let mut c = Checkpoint::new("aged", State::new(), 0, vec![]);
        c.created_at = chrono::Utc::now() - chrono::Duration::hours(48);
        c
    };
    let recent = Checkpoint::new("aged", State::new(), 1, vec![]).with_version(2);
    checkpointer.save(&old).await.unwrap();
    checkpointer.save(&recent).await.unwrap();

    let removed = checkpointer
        .prune("aged", &RetentionPolicy::max_age(Duration::from_secs(3600)))
        .await
        .unwrap();
    assert_eq!(removed, 1, "the 48-hour-old one goes");
    assert_eq!(checkpointer.list("aged").await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_policy_never_leaves_a_thread_empty() {
    // Every checkpoint is older than the limit, and one is still kept.
    let checkpointer = MemoryCheckpointer::new();
    for step in 0..3 {
        let mut c =
            Checkpoint::new("all-old", State::new(), step, vec![]).with_version(step as u64 + 1);
        c.created_at = chrono::Utc::now() - chrono::Duration::days(30);
        checkpointer.save(&c).await.unwrap();
    }

    checkpointer
        .prune("all-old", &RetentionPolicy::max_age(Duration::from_secs(60)))
        .await
        .unwrap();
    assert_eq!(
        checkpointer.list("all-old").await.unwrap().len(),
        1,
        "the newest survives even when the whole thread is past the limit"
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn the_sqlite_backend_prunes_the_same_set() {
    use adk_graph::checkpoint::SqliteCheckpointer;

    let checkpointer = SqliteCheckpointer::new("sqlite::memory:").await.unwrap();
    for step in 0..5 {
        checkpointer
            .save(&Checkpoint::new("sql", State::new(), step, vec![]).with_version(step as u64 + 1))
            .await
            .unwrap();
    }

    let removed = checkpointer.prune("sql", &RetentionPolicy::keep_last(2)).await.unwrap();
    assert_eq!(removed, 3, "five saved, two kept");
    assert_eq!(checkpointer.list("sql").await.unwrap().len(), 2);
    assert!(checkpointer.load("sql").await.unwrap().is_some(), "and the newest still loads");
}

/// `a`, then `b` behind a gate, then `c`, then `d` behind a second gate. Counts the
/// runs of `a`.
fn two_gates(
    retention: Option<RetentionPolicy>,
) -> (CompiledGraph, Arc<MemoryCheckpointer>, Arc<AtomicUsize>) {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let runs_of_a = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&runs_of_a);
    let mut graph =
        StateGraph::with_channels(&["a", "b", "c", "d"]).add_node_fn("a", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("a", json!(true)))
            }
        });
    for name in ["b", "c", "d"] {
        graph = graph.add_node_fn(name, move |_ctx| async move {
            Ok(NodeOutput::new().with_update(name, json!(true)))
        });
    }
    let compiled = graph
        .add_edge(START, "a")
        .add_edge("a", "b")
        .add_edge("b", "c")
        .add_edge("c", "d")
        .add_edge("d", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(Arc::clone(&checkpointer) as Arc<dyn Checkpointer>)
        .with_interrupt_before(&["b", "d"]);
    let compiled = match retention {
        Some(policy) => compiled.with_checkpoint_retention(policy),
        None => compiled,
    };
    (compiled, checkpointer, runs_of_a)
}

/// The checkpoint a paused run names.
fn paused_at(outcome: adk_graph::Result<State>) -> String {
    match outcome {
        Err(adk_graph::GraphError::Interrupted(paused)) => paused.checkpoint_id,
        other => panic!("expected a pause, got {other:?}"),
    }
}

async fn ids(checkpointer: &MemoryCheckpointer, thread: &str) -> Vec<String> {
    let checkpoints = checkpointer.list(thread).await.unwrap();
    checkpoints.into_iter().map(|checkpoint| checkpoint.checkpoint_id).collect()
}

#[tokio::test]
async fn a_pause_no_run_resumed_survives_a_later_branch() {
    let (graph, checkpointer, _) = two_gates(None);
    let thread = "branched";

    let first_gate = paused_at(graph.invoke(State::new(), ExecutionConfig::new(thread)).await);
    let second_gate = paused_at(graph.invoke(State::new(), ExecutionConfig::new(thread)).await);

    // The first approval is redone on the same thread: a second branch runs past the
    // second gate's checkpoint and pauses at its own copy of the gate.
    let redone = paused_at(
        graph
            .invoke(State::new(), ExecutionConfig::new(thread).with_resume_from(&first_gate))
            .await,
    );
    assert_ne!(redone, second_gate);

    // An operator's sweep with the tightest policy.
    let removed = checkpointer.prune(thread, &RetentionPolicy::keep_last(1)).await.unwrap();
    assert!(removed > 0, "the history the branches no longer need goes");
    let kept = ids(&checkpointer, thread).await;
    assert_eq!(
        kept,
        vec![second_gate.clone(), redone.clone()],
        "the newest, and the first branch's pause that no run answered, are kept"
    );

    let state = graph
        .invoke(State::new(), ExecutionConfig::new(thread).with_resume_from(&second_gate))
        .await
        .expect("the pause a caller was handed still resumes");
    assert_eq!(state.get("d"), Some(&json!(true)));

    // Answered now, so the next sweep lets it go; the redone branch's pause stays.
    checkpointer.prune(thread, &RetentionPolicy::keep_last(1)).await.unwrap();
    let kept = ids(&checkpointer, thread).await;
    assert!(!kept.contains(&second_gate), "an answered pause is no longer kept: {kept:?}");
    assert!(kept.contains(&redone), "the other branch's pause still is: {kept:?}");
}

#[tokio::test]
async fn resuming_a_pruned_checkpoint_is_refused_before_anything_runs() {
    let (graph, checkpointer, runs_of_a) = two_gates(Some(RetentionPolicy::keep_last(1)));
    let thread = "pruned-resume-point";

    let first_gate = paused_at(graph.invoke(State::new(), ExecutionConfig::new(thread)).await);
    paused_at(graph.invoke(State::new(), ExecutionConfig::new(thread)).await);
    assert!(
        !ids(&checkpointer, thread).await.contains(&first_gate),
        "the first pause was answered, so retention let it go"
    );
    assert_eq!(runs_of_a.load(Ordering::SeqCst), 1);

    let refused = graph
        .invoke(State::new(), ExecutionConfig::new(thread).with_resume_from(&first_gate))
        .await;
    match refused {
        Err(adk_graph::GraphError::CheckpointError(message)) => {
            assert!(message.contains(&first_gate), "the error names the checkpoint: {message}")
        }
        other => panic!("expected the resume to be refused, got {other:?}"),
    }
    assert_eq!(runs_of_a.load(Ordering::SeqCst), 1, "nothing ran again");
}

#[tokio::test]
async fn editing_a_paused_thread_moves_its_open_pause_to_the_revision() {
    let (graph, checkpointer, _) = two_gates(None);
    let thread = "edited";

    let paused = paused_at(graph.invoke(State::new(), ExecutionConfig::new(thread)).await);
    graph.update_state(thread, [("a".to_string(), json!("edited"))]).await.unwrap();
    let revision = checkpointer.load(thread).await.unwrap().unwrap();
    assert_eq!(revision.open_pauses().unwrap(), vec![revision.checkpoint_id.clone()]);

    checkpointer.prune(thread, &RetentionPolicy::keep_last(1)).await.unwrap();
    assert_eq!(ids(&checkpointer, thread).await, vec![revision.checkpoint_id.clone()]);
    assert!(!ids(&checkpointer, thread).await.contains(&paused));

    let finished = paused_at(graph.invoke(State::new(), ExecutionConfig::new(thread)).await);
    let second = checkpointer.load_by_id(&finished).await.unwrap().unwrap();
    assert_eq!(second.state.get("a"), Some(&json!("edited")), "the edit carried into the resume");
    assert_eq!(second.open_pauses().unwrap(), vec![finished], "only the new pause is open");
}

/// Live on Anthropic: a release-note graph whose draft and reviews call
/// `claude-haiku-5-5`, checkpointed to a SQLite file under `keep_last(2)`. The run pauses
/// before `publish`; a branch on the same thread pauses again; a sweep keeps only what a
/// resume needs; the store is reopened as a restarted process would; and the first pause,
/// resumed by its ID, publishes from the state written before it.
#[cfg(feature = "sqlite")]
#[tokio::test]
#[ignore = "calls the Anthropic API with ANTHROPIC_API_KEY"]
async fn live_a_pruned_thread_resumes_on_anthropic() {
    use adk_core::{Content, Llm, LlmRequest};
    use adk_graph::checkpoint::SqliteCheckpointer;
    use futures::StreamExt;

    const MODEL: &str = "claude-haiku-5-5";
    const CODE: &str = "ZEBRA-42";

    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        println!("skipped: no ANTHROPIC_API_KEY");
        return;
    };
    let llm: Arc<dyn Llm> = Arc::new(
        adk_model::anthropic::AnthropicClient::new(adk_model::anthropic::AnthropicConfig::new(
            key, MODEL,
        ))
        .unwrap(),
    );
    let calls = Arc::new(AtomicUsize::new(0));

    async fn ask(
        llm: Arc<dyn Llm>,
        calls: Arc<AtomicUsize>,
        prompt: String,
    ) -> adk_graph::Result<String> {
        calls.fetch_add(1, Ordering::SeqCst);
        let request = LlmRequest::new(MODEL, vec![Content::new("user").with_text(prompt)]);
        let failed = |error: adk_core::AdkError| adk_graph::GraphError::Other(error.to_string());
        let mut stream = llm.generate_content(request, false).await.map_err(failed)?;
        let mut text = String::new();
        while let Some(response) = stream.next().await {
            for part in response.map_err(failed)?.content.into_iter().flat_map(|c| c.parts) {
                text.push_str(part.text().unwrap_or_default());
            }
        }
        Ok(text.trim().to_string())
    }

    let release_graph =
        |checkpointer: Arc<dyn Checkpointer>| {
            let (draft_llm, draft_calls) = (Arc::clone(&llm), Arc::clone(&calls));
            let (review_llm, review_calls) = (Arc::clone(&llm), Arc::clone(&calls));
            let (publish_llm, publish_calls) = (Arc::clone(&llm), Arc::clone(&calls));
            StateGraph::with_channels(&["feature", "draft", "revisions", "code", "approved_by"])
                .add_node_fn("draft", move |ctx| {
                    let (llm, calls) = (Arc::clone(&draft_llm), Arc::clone(&draft_calls));
                    async move {
                        let feature =
                            ctx.get("feature").and_then(|v| v.as_str()).unwrap_or_default();
                        let draft = ask(
                            llm,
                            calls,
                            format!(
                                "Write a one-sentence release note for this feature: {feature}. \
                         Include the code {CODE} verbatim. Reply with the sentence only."
                            ),
                        )
                        .await?;
                        Ok(NodeOutput::new()
                            .with_update("draft", json!(draft))
                            .with_update("revisions", json!(0)))
                    }
                })
                .add_node_fn("review", move |ctx| {
                    let (llm, calls) = (Arc::clone(&review_llm), Arc::clone(&review_calls));
                    async move {
                        let draft = ctx.get("draft").and_then(|v| v.as_str()).unwrap_or_default();
                        let revisions = ctx.get("revisions").and_then(|v| v.as_i64()).unwrap_or(0);
                        let tightened = ask(llm, calls, format!(
                        "Tighten this release note to at most 20 words. Keep the code {CODE} \
                         verbatim. Reply with the note only.\n\n{draft}"
                    ))
                    .await?;
                        Ok(NodeOutput::new()
                            .with_update("draft", json!(tightened))
                            .with_update("revisions", json!(revisions + 1)))
                    }
                })
                .add_node_fn("publish", move |ctx| {
                    let (llm, calls) = (Arc::clone(&publish_llm), Arc::clone(&publish_calls));
                    async move {
                        let draft = ctx.get("draft").and_then(|v| v.as_str()).unwrap_or_default();
                        let code = ask(llm, calls, format!(
                        "Reply with only the code of the form WORD-NUMBER that appears in this \
                         release note, nothing else.\n\n{draft}"
                    ))
                    .await?;
                        Ok(NodeOutput::new().with_update("code", json!(code)))
                    }
                })
                .add_edge(START, "draft")
                .add_edge("draft", "review")
                .add_conditional_edges(
                    "review",
                    |state| {
                        let revisions =
                            state.get("revisions").and_then(|v| v.as_i64()).unwrap_or(0);
                        if revisions < 3 { "review".to_string() } else { "publish".to_string() }
                    },
                    [("review", "review"), ("publish", "publish")],
                )
                .add_edge("publish", END)
                .compile()
                .unwrap()
                .with_checkpointer_arc(checkpointer)
                .with_checkpoint_retention(RetentionPolicy::keep_last(2))
                .with_interrupt_before(&["publish"])
        };
    async fn stored(checkpointer: &dyn Checkpointer, thread: &str) -> Vec<(u64, bool)> {
        let newest = checkpointer.load(thread).await.unwrap().unwrap();
        let open = newest.open_pauses().unwrap();
        let checkpoints = checkpointer.list(thread).await.unwrap();
        checkpoints.iter().map(|c| (c.version, open.contains(&c.checkpoint_id))).collect()
    }

    let path =
        std::env::temp_dir().join(format!("adk-graph-live-retention-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let thread = "release-notes";
    let input: State =
        [("feature".to_string(), json!("checkpoint pruning that keeps open pauses"))].into();

    let first_store: Arc<dyn Checkpointer> = Arc::new(SqliteCheckpointer::new(&url).await.unwrap());
    let graph = release_graph(Arc::clone(&first_store));
    let paused = paused_at(graph.invoke(input, ExecutionConfig::new(thread)).await);
    let head = first_store.load(thread).await.unwrap().unwrap();
    let before_pause = first_store.load_by_id(&paused).await.unwrap().unwrap();
    println!(
        "run 1: {} model calls, paused at version {} — stored (version, open pause): {:?}",
        calls.load(Ordering::SeqCst),
        head.version,
        stored(&*first_store, thread).await
    );
    println!("  draft before the pause: {}", before_pause.state["draft"]);
    assert!(head.version >= 5, "draft, three reviews and the pause were written");
    assert!(
        first_store.list(thread).await.unwrap().len() <= 2,
        "keep_last(2) pruned as the run went"
    );

    // A branch on the same thread from the oldest checkpoint still stored.
    let oldest = first_store.list(thread).await.unwrap()[0].checkpoint_id.clone();
    assert_ne!(oldest, paused);
    let branch = paused_at(
        graph.invoke(State::new(), ExecutionConfig::new(thread).with_resume_from(&oldest)).await,
    );
    let removed = first_store.prune(thread, &RetentionPolicy::keep_last(1)).await.unwrap();
    println!(
        "branch paused at {branch}; sweep keep_last(1) removed {removed} — stored: {:?}",
        stored(&*first_store, thread).await
    );
    assert!(
        first_store.load_by_id(&paused).await.unwrap().is_some(),
        "the unanswered pause is kept"
    );
    drop(graph);
    drop(first_store);

    // A restarted process: a new pool over the same file.
    let reopened: Arc<dyn Checkpointer> = Arc::new(SqliteCheckpointer::new(&url).await.unwrap());
    let graph = release_graph(Arc::clone(&reopened));
    let approval: State = [("approved_by".to_string(), json!("release-manager"))].into();
    let done = graph
        .invoke(approval, ExecutionConfig::new(thread).with_resume_from(&paused))
        .await
        .expect("the first pause resumes from the reopened store");
    let code = done["code"].as_str().unwrap_or_default().to_string();
    println!(
        "resumed {paused}: publish answered {code:?}, approved_by {}, {} model calls in all",
        done["approved_by"],
        calls.load(Ordering::SeqCst)
    );
    assert_eq!(done["draft"], before_pause.state["draft"], "publish read the pre-pause draft");
    assert!(code.contains(CODE), "the model read the code back from the resumed state: {code}");

    reopened.prune(thread, &RetentionPolicy::keep_last(1)).await.unwrap();
    let after = stored(&*reopened, thread).await;
    println!("after the resume, sweep keep_last(1) — stored: {after:?}");
    assert!(
        reopened.load_by_id(&paused).await.unwrap().is_none(),
        "the answered pause is released"
    );
    assert!(reopened.load_by_id(&branch).await.unwrap().is_some(), "the branch's pause stays open");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
