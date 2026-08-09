//! The trio and the graph compose, rather than competing.
//!
//! `SequentialAgent`, `ParallelAgent` and `LoopAgent` are agents. `AgentNode`
//! wraps any agent. So a trio agent is a graph node, and a graph is an agent a
//! trio can hold. These tests pin both directions, so neither API can become
//! something the other cannot carry.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::{ParallelAgent, SequentialAgent};
use adk_core::{Agent, Content, Event, EventStream, InvocationContext, Result as CoreResult};
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{AgentNode, ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;

/// Records that it ran and answers with its own name.
struct Marker {
    name: String,
    runs: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Agent for Marker {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "records that it ran"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> CoreResult<EventStream> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let mut event = Event::new(&self.name);
        event.set_content(Content::new("assistant").with_text(&self.name));
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }
}

fn marker(name: &str, runs: &Arc<AtomicUsize>) -> Arc<dyn Agent> {
    Arc::new(Marker { name: name.to_string(), runs: Arc::clone(runs) })
}

fn names_into(
    channel: &'static str,
) -> impl Fn(&[Event]) -> std::collections::HashMap<String, serde_json::Value> {
    move |events| {
        let names: Vec<String> = events
            .iter()
            .filter_map(|event| event.content())
            .flat_map(|content| {
                content.parts.iter().filter_map(|part| part.text().map(str::to_string))
            })
            .collect();
        let mut updates = std::collections::HashMap::new();
        updates.insert(channel.to_string(), json!(names));
        updates
    }
}

/// A `SequentialAgent` runs inside a graph node.
#[tokio::test]
async fn a_sequential_agent_is_a_graph_node() {
    let runs = Arc::new(AtomicUsize::new(0));

    let pipeline = Arc::new(SequentialAgent::new(
        "pipeline",
        vec![marker("first", &runs), marker("second", &runs)],
    ));

    let graph = StateGraph::with_channels(&["seen", "after"])
        .add_node(
            AgentNode::new(pipeline as Arc<dyn Agent>)
                .with_input_mapper(|_state: &State| Content::new("user").with_text("go"))
                .with_output_mapper(names_into("seen")),
        )
        .add_node_fn("after", |_ctx| async move {
            Ok(NodeOutput::new().with_update("after", json!(true)))
        })
        .add_edge(START, "pipeline")
        .add_edge("pipeline", "after")
        .add_edge("after", END)
        .compile()
        .expect("the graph compiles");

    let state = graph.invoke(State::new(), ExecutionConfig::new("trio-node")).await.unwrap();

    assert_eq!(runs.load(Ordering::SeqCst), 2, "both sub-agents ran, inside one node");
    assert_eq!(state.get("after"), Some(&json!(true)), "the graph continued past the trio node");
    let seen = state.get("seen").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    assert!(seen.contains(&json!("first")), "the first sub-agent's answer reached the state");
    assert!(seen.contains(&json!("second")), "the second sub-agent's answer reached the state");
}

/// A `ParallelAgent` runs inside a graph node.
#[tokio::test]
async fn a_parallel_agent_is_a_graph_node() {
    let runs = Arc::new(AtomicUsize::new(0));

    let fanout =
        Arc::new(ParallelAgent::new("fanout", vec![marker("left", &runs), marker("right", &runs)]));

    let graph = StateGraph::with_channels(&["seen"])
        .add_node(
            AgentNode::new(fanout as Arc<dyn Agent>)
                .with_input_mapper(|_state: &State| Content::new("user").with_text("go"))
                .with_output_mapper(names_into("seen")),
        )
        .add_edge(START, "fanout")
        .add_edge("fanout", END)
        .compile()
        .expect("the graph compiles");

    let state = graph.invoke(State::new(), ExecutionConfig::new("parallel-node")).await.unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 2, "both sub-agents ran");
    let seen = state.get("seen").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    assert_eq!(seen.len(), 2, "both answers reached the node");
}

/// The other direction: a graph is an agent, so a trio agent can hold one.
#[tokio::test]
async fn a_graph_agent_is_a_trio_sub_agent() {
    use adk_graph::agent::GraphAgent;

    let runs = Arc::new(AtomicUsize::new(0));

    let inner = GraphAgent::builder("inner_graph")
        .channels(&["value"])
        .node_fn("step", |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(1))) })
        .edge(START, "step")
        .edge("step", END)
        .build()
        .expect("the graph agent builds");

    let pipeline = SequentialAgent::new(
        "outer",
        vec![Arc::new(inner) as Arc<dyn Agent>, marker("tail", &runs)],
    );

    // The graph agent satisfies the trait the trio takes, so a pipeline holds one.
    assert_eq!(pipeline.sub_agents().len(), 2);
    assert_eq!(pipeline.sub_agents()[0].name(), "inner_graph");
    assert_eq!(pipeline.sub_agents()[1].name(), "tail");
}
