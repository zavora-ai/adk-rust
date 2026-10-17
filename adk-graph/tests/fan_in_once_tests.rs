//! A fan-in node must run once per set of predecessor completions.
//!
//! The executor advances the frontier from whichever nodes finished in the last
//! super-step. A node with several incoming edges therefore became eligible as
//! soon as *any* one predecessor finished, so on branches of unequal length it
//! ran once per super-step in which a predecessor completed — applying its
//! updates more than once and reading state that was still incomplete.
//!
//! `mark_deferred` fixed this for a caller who knew to reach for it. Nothing in
//! the graph shape required it, so the default was wrong.

use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Two predecessors, one of them two hops away, so they complete in different
/// super-steps. The join must still run once.
#[tokio::test]
async fn a_join_runs_once_when_its_branches_are_unequal() {
    let joins = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&joins);

    let graph = StateGraph::with_channels(&["short", "long", "joined"])
        // Short branch: one hop.
        .add_node_fn("short", |_ctx| async move {
            Ok(NodeOutput::new().with_update("short", json!("s")))
        })
        // Long branch: two hops, so it lands a super-step later.
        .add_node_fn("long_a", |_ctx| async move {
            Ok(NodeOutput::new().with_update("long", json!("a")))
        })
        .add_node_fn("long_b", |_ctx| async move {
            Ok(NodeOutput::new().with_update("long", json!("ab")))
        })
        .add_node_fn("join", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let short = ctx.get("short").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let long = ctx.get("long").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Ok(NodeOutput::new().with_update("joined", json!(format!("{short}+{long}"))))
            }
        })
        .add_edge(START, "short")
        .add_edge(START, "long_a")
        .add_edge("long_a", "long_b")
        .add_edge("short", "join")
        .add_edge("long_b", "join")
        .add_edge("join", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("uneven")).await.unwrap();

    assert_eq!(
        joins.load(Ordering::SeqCst),
        1,
        "the join must wait for both branches, not run once per arriving predecessor"
    );
    assert_eq!(
        state.get("joined").and_then(|v| v.as_str()),
        Some("s+ab"),
        "the join must see the completed long branch, not its intermediate value"
    );
}

/// Equal-length branches already worked, and must keep working.
#[tokio::test]
async fn a_join_runs_once_when_its_branches_are_equal() {
    let joins = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&joins);

    let graph = StateGraph::with_channels(&["a", "b", "joined"])
        .add_node_fn(
            "first",
            |_ctx| async move { Ok(NodeOutput::new().with_update("a", json!(1))) },
        )
        .add_node_fn(
            "second",
            |_ctx| async move { Ok(NodeOutput::new().with_update("b", json!(2))) },
        )
        .add_node_fn("join", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let a = ctx.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
                let b = ctx.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("joined", json!(a + b)))
            }
        })
        .add_edge(START, "first")
        .add_edge(START, "second")
        .add_edge("first", "join")
        .add_edge("second", "join")
        .add_edge("join", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("even")).await.unwrap();

    assert_eq!(joins.load(Ordering::SeqCst), 1);
    assert_eq!(state.get("joined").and_then(|v| v.as_i64()), Some(3));
}

/// A single incoming edge is not a fan-in and must not be deferred.
///
/// Guards against the in-degree rule catching an ordinary chain, which would
/// change the behaviour of every linear graph.
#[tokio::test]
async fn a_node_with_one_incoming_edge_is_not_deferred() {
    let runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&runs);

    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn(
            "first",
            |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(1))) },
        )
        .add_node_fn("second", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let value = ctx.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(NodeOutput::new().with_update("value", json!(value + 1)))
            }
        })
        .add_edge(START, "first")
        .add_edge("first", "second")
        .add_edge("second", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("linear")).await.unwrap();

    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(state.get("value").and_then(|v| v.as_i64()), Some(2));
}

/// A join can release once a quorum of branches has answered.
///
/// `min_predecessors` decides how many arrivals release the node when
/// `fan_in_timeout` expires. Neither adk-python nor adk-go offers this: both
/// support wait-for-all only.
#[tokio::test]
async fn a_join_can_release_on_a_quorum() {
    use adk_graph::deferred::{DeferredNodeConfig, MergeStrategy};
    use std::time::Duration;

    let graph = StateGraph::with_channels(&["fast_a", "fast_b", "slow", "joined"])
        .add_node_fn("fast_a", |_ctx| async move {
            Ok(NodeOutput::new().with_update("fast_a", json!(1)))
        })
        .add_node_fn("fast_b", |_ctx| async move {
            Ok(NodeOutput::new().with_update("fast_b", json!(2)))
        })
        // Never answers within the timeout.
        .add_node_fn("slow", |_ctx| async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(NodeOutput::new().with_update("slow", json!(3)))
        })
        .add_node_fn("join", |ctx| async move {
            let a = ctx.get("fast_a").and_then(|v| v.as_i64()).unwrap_or(0);
            let b = ctx.get("fast_b").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("joined", json!(a + b)))
        })
        .add_edge(START, "fast_a")
        .add_edge(START, "fast_b")
        .add_edge("fast_a", "join")
        .add_edge("fast_b", "join")
        .add_edge("join", END)
        .mark_deferred(
            "join",
            DeferredNodeConfig {
                merge_strategy: MergeStrategy::MergeMap,
                fan_in_timeout: Some(Duration::from_millis(50)),
                min_predecessors: Some(2),
            },
        )
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("quorum")).await.unwrap();
    assert_eq!(state.get("joined").and_then(|v| v.as_i64()), Some(3));
}

// ── A join waits only for the predecessors that can still run in this pass ──

/// Counts executions and records the step each one ran in.
#[derive(Default, Clone)]
struct Runs(Arc<std::sync::Mutex<Vec<(String, usize)>>>);

impl Runs {
    fn record(&self, node: &str, step: usize) {
        self.0.lock().unwrap().push((node.to_string(), step));
    }

    fn count(&self, node: &str) -> usize {
        self.0.lock().unwrap().iter().filter(|(name, _)| name == node).count()
    }

    fn steps(&self, node: &str) -> Vec<usize> {
        self.0.lock().unwrap().iter().filter(|(name, _)| name == node).map(|(_, s)| *s).collect()
    }
}

/// `review` has two incoming edges, `implement` and the loop's `revise`, as in
/// `examples/coding_graph`. Only one of them arrives in each pass, so waiting for
/// both stranded the node and the run returned `Ok` without reviewing anything.
#[tokio::test]
async fn a_node_re_entered_by_a_loop_back_edge_runs_on_every_pass() {
    let runs = Runs::default();
    let (review_runs, finalize_runs) = (runs.clone(), runs.clone());
    let graph = StateGraph::with_channels(&["code", "decision", "rounds"])
        .add_node_fn("implement", |_| async {
            Ok(NodeOutput::new().with_update("code", json!("v1")))
        })
        .add_node_fn("review", move |ctx| {
            let runs = review_runs.clone();
            async move {
                runs.record("review", ctx.step);
                let rounds = ctx.get("rounds").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
                let decision = if rounds < 2 { "revise" } else { "finalize" };
                Ok(NodeOutput::new()
                    .with_update("rounds", json!(rounds))
                    .with_update("decision", json!(decision)))
            }
        })
        .add_node_fn("revise", |_| async { Ok(NodeOutput::new().with_update("code", json!("v2"))) })
        .add_node_fn("finalize", move |ctx| {
            let runs = finalize_runs.clone();
            async move {
                runs.record("finalize", ctx.step);
                Ok(NodeOutput::new())
            }
        })
        .add_edge(START, "implement")
        .add_edge("implement", "review")
        .add_edge("revise", "review")
        .add_conditional_edges(
            "review",
            |s| s.get("decision").and_then(|v| v.as_str()).unwrap_or("finalize").to_string(),
            [("revise", "revise"), ("finalize", "finalize")],
        )
        .add_edge("finalize", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("loop-reentry")).await.unwrap();

    assert_eq!(
        (runs.count("review"), runs.count("finalize"), state.get("code")),
        (2, 1, Some(&json!("v2"))),
        "review must run once per pass, then the loop must finish"
    );
}

/// A merge after a conditional branch receives only the branch that was taken.
#[tokio::test]
async fn a_merge_after_a_conditional_branch_runs_once() {
    use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};

    let runs = Runs::default();
    let merge_runs = runs.clone();
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["route", "a", "b", "merged"])
        .add_node_fn("router", |_| async { Ok(NodeOutput::new().with_update("route", json!("a"))) })
        .add_node_fn("path_a", |_| async { Ok(NodeOutput::new().with_update("a", json!(1))) })
        .add_node_fn("path_b", |_| async { Ok(NodeOutput::new().with_update("b", json!(2))) })
        .add_node_fn("merge", move |ctx| {
            let runs = merge_runs.clone();
            async move {
                runs.record("merge", ctx.step);
                Ok(NodeOutput::new()
                    .with_update("merged", ctx.get("merge_fan_in").cloned().unwrap_or_default()))
            }
        })
        .add_edge(START, "router")
        .add_conditional_edges(
            "router",
            |s| s.get("route").and_then(|v| v.as_str()).unwrap_or("a").to_string(),
            [("a", "path_a"), ("b", "path_b")],
        )
        .add_edge("path_a", "merge")
        .add_edge("path_b", "merge")
        .add_edge("merge", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer.clone());

    let state = graph.invoke(State::new(), ExecutionConfig::new("merge")).await.unwrap();
    let last = checkpointer.load("merge").await.unwrap().unwrap();

    assert_eq!(
        (runs.count("merge"), state.get("merged"), last.pending_nodes.is_empty()),
        (1, Some(&json!([{"a": 1}])), true)
    );
    assert!(
        !last.metadata.contains_key("adk.graph.fanIn"),
        "a finished run must not leave a join waiting: {:?}",
        last.metadata
    );
}

/// A predecessor that ran but routed elsewhere does not arrive, whichever step
/// it finished in. It used to count as arrived only when it finished in the same
/// step as the other predecessor.
#[tokio::test]
async fn a_predecessor_that_routes_away_does_not_arrive() {
    use adk_graph::deferred::DeferredNodeConfig;

    for long_branch in [false, true] {
        let runs = Runs::default();
        let join_runs = runs.clone();
        let mut graph = StateGraph::with_channels(&["a_route", "b", "x", "seen"])
            .add_node_fn("a", |_| async {
                Ok(NodeOutput::new().with_update("a_route", json!("x")))
            })
            .add_node_fn("x", |_| async { Ok(NodeOutput::new().with_update("x", json!(true))) })
            .add_node_fn("b", |_| async { Ok(NodeOutput::new().with_update("b", json!(true))) })
            .add_node_fn("join", move |ctx| {
                let runs = join_runs.clone();
                async move {
                    runs.record("join", ctx.step);
                    Ok(NodeOutput::new()
                        .with_update("seen", ctx.get("join_fan_in").cloned().unwrap_or_default()))
                }
            })
            .add_edge(START, "a")
            .add_conditional_edges(
                "a",
                |s| s.get("a_route").and_then(|v| v.as_str()).unwrap_or("j").to_string(),
                [("j", "join"), ("x", "x")],
            )
            .add_edge("x", END)
            .add_edge("b", "join")
            .add_edge("join", END)
            .mark_deferred("join", DeferredNodeConfig::default());
        graph = if long_branch {
            graph
                .add_node_fn("b0", |_| async { Ok(NodeOutput::new()) })
                .add_edge(START, "b0")
                .add_edge("b0", "b")
        } else {
            graph.add_edge(START, "b")
        };
        let state = graph
            .compile()
            .unwrap()
            .invoke(State::new(), ExecutionConfig::new("routed-away"))
            .await
            .unwrap();

        assert_eq!(
            (runs.count("join"), state.get("seen")),
            (1, Some(&json!([true]))),
            "long_branch = {long_branch}"
        );
    }
}

/// A join waits for a predecessor that is reached only through another join that
/// is still waiting, so it runs once, with both arrivals.
#[tokio::test]
async fn a_join_waits_for_a_predecessor_behind_another_waiting_join() {
    let runs = Runs::default();
    let last_runs = runs.clone();
    let graph = StateGraph::with_channels(&["a", "b", "c", "m", "seen_m"])
        .add_node_fn("a", |_| async { Ok(NodeOutput::new().with_update("a", json!(1))) })
        .add_node_fn("b0", |_| async { Ok(NodeOutput::new()) })
        .add_node_fn("b", |_| async { Ok(NodeOutput::new().with_update("b", json!(2))) })
        .add_node_fn("first", |_| async { Ok(NodeOutput::new()) })
        .add_node_fn("m", |_| async { Ok(NodeOutput::new().with_update("m", json!(3))) })
        .add_node_fn("c", |_| async { Ok(NodeOutput::new().with_update("c", json!(4))) })
        .add_node_fn("last", move |ctx| {
            let runs = last_runs.clone();
            async move {
                runs.record("last", ctx.step);
                Ok(NodeOutput::new().with_update("seen_m", json!(ctx.get("m").is_some())))
            }
        })
        .add_edge(START, "a")
        .add_edge(START, "b0")
        .add_edge(START, "c")
        .add_edge("b0", "b")
        .add_edge("a", "first")
        .add_edge("b", "first")
        .add_edge("first", "m")
        .add_edge("m", "last")
        .add_edge("c", "last")
        .add_edge("last", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("chained")).await.unwrap();

    assert_eq!((runs.count("last"), state.get("seen_m")), (1, Some(&json!(true))));
}

/// Three reviewers re-entered by a revise loop and joined by `synthesize`, wired
/// as in `examples/coding_graph`. The reviewers run together in one step on each
/// pass, and `synthesize` runs once per pass.
#[tokio::test]
async fn parallel_reviewers_in_a_revise_loop_run_together_on_every_pass() {
    let runs = Runs::default();
    let mut graph = StateGraph::with_channels(&["round", "decision"])
        .add_node_fn("implement", |_| async { Ok(NodeOutput::new()) })
        .add_node_fn("revise", |_| async { Ok(NodeOutput::new()) })
        .add_node_fn("finalize", |_| async { Ok(NodeOutput::new()) });
    for reviewer in ["review_correctness", "review_edge_cases", "review_style"] {
        let runs = runs.clone();
        graph = graph
            .add_node_fn(reviewer, move |ctx| {
                let runs = runs.clone();
                async move {
                    runs.record(reviewer, ctx.step);
                    Ok(NodeOutput::new().with_update(reviewer, json!("approve")))
                }
            })
            .add_edge("implement", reviewer)
            .add_edge("revise", reviewer)
            .add_edge(reviewer, "synthesize");
    }
    let synthesize_runs = runs.clone();
    let graph = graph
        .add_node_fn("synthesize", move |ctx| {
            let runs = synthesize_runs.clone();
            async move {
                runs.record("synthesize", ctx.step);
                let round = ctx.get("round").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
                let decision = if round < 2 { "revise" } else { "finalize" };
                Ok(NodeOutput::new()
                    .with_update("round", json!(round))
                    .with_update("decision", json!(decision)))
            }
        })
        .add_edge(START, "implement")
        .add_conditional_edges(
            "synthesize",
            |s| s.get("decision").and_then(|v| v.as_str()).unwrap_or("finalize").to_string(),
            [("revise", "revise"), ("finalize", "finalize")],
        )
        .add_edge("finalize", END)
        .compile()
        .unwrap();

    graph.invoke(State::new(), ExecutionConfig::new("reviewers")).await.unwrap();

    let reviewer_steps: Vec<Vec<usize>> =
        ["review_correctness", "review_edge_cases", "review_style"]
            .iter()
            .map(|reviewer| runs.steps(reviewer))
            .collect();
    assert_eq!(
        (reviewer_steps, runs.steps("synthesize")),
        (vec![vec![1, 4]; 3], vec![2, 5]),
        "each pass runs the three reviewers in one step, then synthesize once"
    );
}

/// `min_predecessors` still applies when the missing predecessors can no longer
/// run: the run fails rather than releasing the join with too few.
#[tokio::test]
async fn a_join_short_of_its_minimum_fails_when_the_rest_cannot_arrive() {
    use adk_graph::deferred::DeferredNodeConfig;
    use adk_graph::error::GraphError;

    let graph = StateGraph::with_channels(&["route"])
        .add_node_fn("router", |_| async { Ok(NodeOutput::new().with_update("route", json!("a"))) })
        .add_node_fn("path_a", |_| async { Ok(NodeOutput::new()) })
        .add_node_fn("path_b", |_| async { Ok(NodeOutput::new()) })
        .add_node_fn("merge", |_| async { Ok(NodeOutput::new()) })
        .add_edge(START, "router")
        .add_conditional_edges(
            "router",
            |s| s.get("route").and_then(|v| v.as_str()).unwrap_or("a").to_string(),
            [("a", "path_a"), ("b", "path_b")],
        )
        .add_edge("path_a", "merge")
        .add_edge("path_b", "merge")
        .add_edge("merge", END)
        .mark_deferred(
            "merge",
            DeferredNodeConfig { min_predecessors: Some(2), ..Default::default() },
        )
        .compile()
        .unwrap();

    let outcome = graph.invoke(State::new(), ExecutionConfig::new("short")).await;

    assert!(
        matches!(
            outcome,
            Err(GraphError::FanInIncomplete { ref node, received: 1, required: 2 }) if node == "merge"
        ),
        "got {outcome:?}"
    );
}

/// A join in a loop records each predecessor's own updates, so its fan-in value
/// stays the same size on every pass. Recording the whole state embedded the
/// previous pass's fan-in value and doubled it each time.
#[tokio::test]
async fn fan_in_arrivals_do_not_grow_across_loop_passes() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let graph = StateGraph::with_channels(&["a_out", "b_out", "iter"])
        .add_node_fn("start", |_| async { Ok(NodeOutput::new()) })
        .add_node_fn("a", |_| async { Ok(NodeOutput::new().with_update("a_out", json!("A"))) })
        .add_node_fn("b", |_| async { Ok(NodeOutput::new().with_update("b_out", json!("B"))) })
        .add_node_fn("j", move |ctx| {
            let record = Arc::clone(&record);
            async move {
                record.lock().unwrap().push(ctx.get("j_fan_in").cloned());
                let iter = ctx.get("iter").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
                Ok(NodeOutput::new().with_update("iter", json!(iter)))
            }
        })
        .add_edge(START, "start")
        .add_edge("start", "a")
        .add_edge("start", "b")
        .add_edge("a", "j")
        .add_edge("b", "j")
        .add_conditional_edges(
            "j",
            |s| {
                if s.get("iter").and_then(|v| v.as_i64()).unwrap_or(0) < 12 {
                    "again"
                } else {
                    "done"
                }
                .to_string()
            },
            [("again", "start"), ("done", END)],
        )
        .compile()
        .unwrap()
        .with_recursion_limit(200);

    graph.invoke(State::new(), ExecutionConfig::new("bounded")).await.unwrap();

    let expected = Some(json!([{"a_out": "A"}, {"b_out": "B"}]));
    assert_eq!(*seen.lock().unwrap(), vec![expected; 12]);
}

/// A graph built with `GraphAgentBuilder` defers a node with several incoming
/// edges, as one built with `StateGraph` does. The builder replaced the compiled
/// fan-in configuration with its own explicit one, which dropped the default.
#[tokio::test]
async fn a_graph_agent_builder_join_runs_once_on_unequal_branches() {
    use adk_graph::agent::GraphAgent;

    let joins = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&joins);
    let agent = GraphAgent::builder("joined")
        .channels(&["short", "long", "joined"])
        .node_fn("short", |_| async { Ok(NodeOutput::new().with_update("short", json!("s"))) })
        .node_fn("long_a", |_| async { Ok(NodeOutput::new()) })
        .node_fn("long_b", |_| async { Ok(NodeOutput::new().with_update("long", json!("l"))) })
        .node_fn("join", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let short = ctx.get("short").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let long = ctx.get("long").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Ok(NodeOutput::new().with_update("joined", json!(format!("{short}+{long}"))))
            }
        })
        .edge(START, "short")
        .edge(START, "long_a")
        .edge("long_a", "long_b")
        .edge("short", "join")
        .edge("long_b", "join")
        .edge("join", END)
        .build()
        .unwrap();

    let state = agent.invoke(State::new(), ExecutionConfig::new("builder-join")).await.unwrap();

    assert_eq!((joins.load(Ordering::SeqCst), state.get("joined")), (1, Some(&json!("s+l"))));
}

/// A quorum released by its timeout ignores the abandoned branch when it arrives
/// later, instead of running again for it.
#[tokio::test]
async fn a_quorum_ignores_the_branch_it_abandoned() {
    use adk_graph::deferred::{DeferredNodeConfig, MergeStrategy};
    use std::time::Duration;

    let runs = Runs::default();
    let join_runs = runs.clone();
    let graph = StateGraph::with_channels(&["a", "b", "slow", "joined"])
        .add_node_fn("fast_a", |_| async { Ok(NodeOutput::new().with_update("a", json!(1))) })
        .add_node_fn("fast_b", |_| async { Ok(NodeOutput::new().with_update("b", json!(2))) })
        .add_node_fn("slow_0", |_| async { Ok(NodeOutput::new()) })
        .add_node_fn("slow_1", |_| async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(NodeOutput::new())
        })
        .add_node_fn("slow_2", |_| async { Ok(NodeOutput::new().with_update("slow", json!(3))) })
        .add_node_fn("join", move |ctx| {
            let runs = join_runs.clone();
            async move {
                runs.record("join", ctx.step);
                Ok(NodeOutput::new()
                    .with_update("joined", ctx.get("join_fan_in").cloned().unwrap_or_default()))
            }
        })
        .add_edge(START, "fast_a")
        .add_edge(START, "fast_b")
        .add_edge(START, "slow_0")
        .add_edge("slow_0", "slow_1")
        .add_edge("slow_1", "slow_2")
        .add_edge("fast_a", "join")
        .add_edge("fast_b", "join")
        .add_edge("slow_2", "join")
        .add_edge("join", END)
        .mark_deferred(
            "join",
            DeferredNodeConfig {
                merge_strategy: MergeStrategy::MergeMap,
                fan_in_timeout: Some(Duration::from_millis(50)),
                min_predecessors: Some(2),
            },
        )
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("quorum-late")).await.unwrap();

    assert_eq!((runs.count("join"), state.get("joined")), (1, Some(&json!({"a": 1, "b": 2}))));
}
