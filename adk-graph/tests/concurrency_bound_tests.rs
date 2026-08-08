//! A super-step must respect a ceiling on concurrent node execution.
//!
//! The executor dispatched its whole frontier at once, so a wide fan-out ran as
//! many nodes as the graph had — enough to exhaust a connection pool or trip a
//! provider rate limit with no way to hold it back. adk-python bounds this with
//! `max_concurrency` and adk-go with `WithMaxConcurrency`.

use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Tracks how many nodes are inside their body at once, and the highest it got.
#[derive(Default)]
struct Gauge {
    current: AtomicUsize,
    peak: AtomicUsize,
}

impl Gauge {
    fn enter(&self) {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
    }

    fn leave(&self) {
        self.current.fetch_sub(1, Ordering::SeqCst);
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

/// Builds a graph fanning out to `width` nodes, each holding for a moment so
/// overlap is observable.
fn fan_out(width: usize, gauge: Arc<Gauge>) -> StateGraph {
    let mut graph = StateGraph::with_channels(&["done"]);
    for index in 0..width {
        let name = format!("n{index}");
        let gauge = Arc::clone(&gauge);
        graph = graph.add_node_fn(&name, move |_ctx| {
            let gauge = Arc::clone(&gauge);
            async move {
                gauge.enter();
                tokio::time::sleep(Duration::from_millis(40)).await;
                gauge.leave();
                Ok(NodeOutput::new().with_update("done", json!(true)))
            }
        });
    }
    for index in 0..width {
        let name = format!("n{index}");
        graph = graph.add_edge(START, &name).add_edge(&name, END);
    }
    graph
}

/// Peak concurrency never exceeds the configured maximum.
#[tokio::test]
async fn concurrency_never_exceeds_the_limit() {
    let gauge = Arc::new(Gauge::default());
    let graph = fan_out(12, Arc::clone(&gauge)).compile().unwrap().with_max_concurrency(3);

    graph.invoke(State::new(), ExecutionConfig::new("bounded")).await.unwrap();

    let peak = gauge.peak();
    assert!(peak <= 3, "peak concurrency was {peak}, which exceeds the limit of 3");
    assert!(peak > 1, "the limit must still allow parallelism, but peak was {peak}");
}

/// Without a limit the whole frontier runs at once, which stays the default.
#[tokio::test]
async fn without_a_limit_the_whole_frontier_runs_at_once() {
    let gauge = Arc::new(Gauge::default());
    let graph = fan_out(12, Arc::clone(&gauge)).compile().unwrap();

    graph.invoke(State::new(), ExecutionConfig::new("unbounded")).await.unwrap();

    assert_eq!(
        gauge.peak(),
        12,
        "an unconfigured graph must keep running its whole frontier concurrently"
    );
}

/// A limit wider than the frontier changes nothing.
#[tokio::test]
async fn a_limit_above_the_frontier_width_is_inert() {
    let gauge = Arc::new(Gauge::default());
    let graph = fan_out(4, Arc::clone(&gauge)).compile().unwrap().with_max_concurrency(50);

    graph.invoke(State::new(), ExecutionConfig::new("wide-limit")).await.unwrap();

    assert_eq!(gauge.peak(), 4);
}

/// A limit of one serialises the frontier.
#[tokio::test]
async fn a_limit_of_one_serialises_the_frontier() {
    let gauge = Arc::new(Gauge::default());
    let graph = fan_out(5, Arc::clone(&gauge)).compile().unwrap().with_max_concurrency(1);

    graph.invoke(State::new(), ExecutionConfig::new("serial")).await.unwrap();

    assert_eq!(gauge.peak(), 1);
}

/// **Property 5: concurrency never exceeds the bound.**
/// *For any* frontier width and any configured maximum, the number of
/// simultaneously executing nodes never exceeds the maximum.
/// **Validates: Requirements 7.2, 7.5**
#[test]
fn prop_concurrency_respects_the_bound() {
    use proptest::prelude::*;

    proptest!(ProptestConfig::with_cases(16), |(width in 2usize..10, limit in 1usize..6)| {
        // adk-graph does not enable `rt-multi-thread`; a current-thread runtime is
        // enough, because the bound is on how many futures are polled, not on
        // threads.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");

        let peak = runtime.block_on(async {
            let gauge = Arc::new(Gauge::default());
            let graph =
                fan_out(width, Arc::clone(&gauge)).compile().unwrap().with_max_concurrency(limit);
            graph.invoke(State::new(), ExecutionConfig::new("prop")).await.unwrap();
            gauge.peak()
        });

        prop_assert!(peak <= limit, "peak {} exceeded limit {}", peak, limit);
    });
}
