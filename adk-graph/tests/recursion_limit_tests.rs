//! The graph's own recursion limit bounds each run unless the run's `ExecutionConfig` sets one.

use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;

const STEPS: i64 = 70;

/// A one-node loop that counts its passes and ends after `steps` of them.
fn counting_loop(steps: i64) -> CompiledGraph {
    StateGraph::with_channels(&["count"])
        .add_node_fn("tick", |ctx| async move {
            let count = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("count", json!(count + 1)))
        })
        .add_edge(START, "tick")
        .add_conditional_edges(
            "tick",
            move |state| {
                let count = state.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                if count >= steps { END.to_string() } else { "tick".to_string() }
            },
            [("tick", "tick"), (END, END)],
        )
        .compile()
        .unwrap()
}

async fn count(graph: &CompiledGraph, config: ExecutionConfig) -> Result<i64, GraphError> {
    let state = graph.invoke(State::new(), config).await?;
    Ok(state.get("count").and_then(|v| v.as_i64()).unwrap_or(0))
}

#[tokio::test]
async fn the_graph_limit_lets_a_run_go_past_fifty_steps() {
    let graph = counting_loop(STEPS).with_recursion_limit(100);
    assert_eq!(count(&graph, ExecutionConfig::new("raised")).await.unwrap(), STEPS);
}

#[tokio::test]
async fn an_unset_limit_is_one_hundred_steps() {
    assert_eq!(count(&counting_loop(STEPS), ExecutionConfig::new("default")).await.unwrap(), STEPS);
    let error = count(&counting_loop(150), ExecutionConfig::new("cap")).await.unwrap_err();
    assert!(matches!(error, GraphError::RecursionLimitExceeded(100)), "{error:?}");
}

#[tokio::test]
async fn the_graph_limit_still_stops_a_longer_run() {
    let graph = counting_loop(STEPS).with_recursion_limit(60);
    let error = count(&graph, ExecutionConfig::new("lowered")).await.unwrap_err();
    assert!(matches!(error, GraphError::RecursionLimitExceeded(60)), "{error:?}");
}

#[tokio::test]
async fn an_execution_config_limit_overrides_the_graphs() {
    let graph = counting_loop(STEPS).with_recursion_limit(10);
    let config = ExecutionConfig::new("override").with_recursion_limit(100);
    assert_eq!(count(&graph, config).await.unwrap(), STEPS);

    let graph = counting_loop(STEPS).with_recursion_limit(100);
    let config = ExecutionConfig::new("override-lower").with_recursion_limit(10);
    let error = count(&graph, config).await.unwrap_err();
    assert!(matches!(error, GraphError::RecursionLimitExceeded(10)), "{error:?}");
}

#[tokio::test]
async fn a_graph_agent_limit_takes_effect() {
    let agent = GraphAgent::builder("counter")
        .channels(&["count"])
        .node_fn("tick", |ctx| async move {
            let count = ctx.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("count", json!(count + 1)))
        })
        .edge(START, "tick")
        .conditional_edge(
            "tick",
            |state| {
                let count = state.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                if count >= STEPS { END.to_string() } else { "tick".to_string() }
            },
            [("tick", "tick"), (END, END)],
        )
        .recursion_limit(80)
        .build()
        .unwrap();
    let state = agent.invoke(State::new(), ExecutionConfig::new("agent")).await.unwrap();
    assert_eq!(state.get("count"), Some(&json!(STEPS)));
}
