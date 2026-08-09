//! Tests for a node routing itself, without a declared edge.
//!
//! This is the counterpart to LangGraph's `Command(update=..., goto=...)`: a node
//! both writes state and names where control goes next. A conditional edge cannot
//! express it, because a conditional edge's targets are fixed when the graph is
//! built.

use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::stream::StreamMode;
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn a_node_routes_to_a_node_it_has_no_edge_to() {
    // `start` has no edge to `left` or `right`. It picks one at run time.
    let graph = StateGraph::with_channels(&["choice", "seen"])
        .add_node_fn("start", |_ctx| async move {
            Ok(NodeOutput::new().with_update("choice", json!("right")).with_goto(["right"]))
        })
        .add_node_fn("left", |_ctx| async move {
            Ok(NodeOutput::new().with_update("seen", json!("left")))
        })
        .add_node_fn("right", |_ctx| async move {
            Ok(NodeOutput::new().with_update("seen", json!("right")))
        })
        .add_edge(START, "start")
        .add_edge("left", END)
        .add_edge("right", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("goto")).await.unwrap();
    assert_eq!(state.get("seen"), Some(&json!("right")));
    assert_eq!(state.get("choice"), Some(&json!("right")));
}

#[tokio::test]
async fn a_goto_replaces_the_declared_edge() {
    // `start` has a declared edge to `left`, and routes to `right` instead. Only
    // `right` runs, matching LangGraph, where goto stands in for an edge.
    let left_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&left_runs);

    let graph = StateGraph::with_channels(&["seen"])
        .add_node_fn("start", |_ctx| async move { Ok(NodeOutput::new().with_goto(["right"])) })
        .add_node_fn("left", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("seen", json!("left")))
            }
        })
        .add_node_fn("right", |_ctx| async move {
            Ok(NodeOutput::new().with_update("seen", json!("right")))
        })
        .add_edge(START, "start")
        .add_edge("start", "left")
        .add_edge("left", END)
        .add_edge("right", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("replace")).await.unwrap();
    assert_eq!(state.get("seen"), Some(&json!("right")));
    assert_eq!(left_runs.load(Ordering::SeqCst), 0, "the declared edge must not also fire");
}

#[tokio::test]
async fn a_node_that_sets_no_goto_follows_its_edges() {
    // The default is unchanged: no goto means the declared edges decide.
    let graph = StateGraph::with_channels(&["seen"])
        .add_node_fn("start", |_ctx| async move { Ok(NodeOutput::new()) })
        .add_node_fn("next", |_ctx| async move {
            Ok(NodeOutput::new().with_update("seen", json!("next")))
        })
        .add_edge(START, "start")
        .add_edge("start", "next")
        .add_edge("next", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("default")).await.unwrap();
    assert_eq!(state.get("seen"), Some(&json!("next")));
}

#[tokio::test]
async fn a_goto_can_name_several_targets() {
    let graph = StateGraph::with_channels(&["seen"])
        .add_node_fn(
            "start",
            |_ctx| async move { Ok(NodeOutput::new().with_goto(["left", "right"])) },
        )
        .add_node_fn("left", |_ctx| async move {
            Ok(NodeOutput::new().with_update("seen", json!(["left"])))
        })
        .add_node_fn("right", |_ctx| async move {
            Ok(NodeOutput::new().with_update("seen", json!(["right"])))
        })
        .add_edge(START, "start")
        .add_edge("left", END)
        .add_edge("right", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("fanout")).await.unwrap();
    // Both ran; the overwrite reducer keeps whichever applied last, and node order
    // is sorted, so `right` wins deterministically.
    assert_eq!(state.get("seen"), Some(&json!(["right"])));
}

#[tokio::test]
async fn a_goto_to_end_stops_the_branch() {
    let after_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&after_runs);

    let graph = StateGraph::with_channels(&["seen"])
        .add_node_fn("start", |_ctx| async move {
            Ok(NodeOutput::new().with_update("seen", json!("start")).with_goto([END]))
        })
        .add_node_fn("after", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new())
            }
        })
        .add_edge(START, "start")
        .add_edge("start", "after")
        .add_edge("after", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("to-end")).await.unwrap();
    assert_eq!(state.get("seen"), Some(&json!("start")));
    assert_eq!(after_runs.load(Ordering::SeqCst), 0, "routing to END must skip `after`");
}

#[tokio::test]
async fn a_goto_to_an_unknown_node_is_an_error() {
    let graph = StateGraph::with_channels(&["seen"])
        .add_node_fn("start", |_ctx| async move { Ok(NodeOutput::new().with_goto(["nowhere"])) })
        .add_edge(START, "start")
        .add_edge("start", END)
        .compile()
        .unwrap();

    let error = graph
        .invoke(State::new(), ExecutionConfig::new("unknown"))
        .await
        .expect_err("a goto naming no node must fail the run");
    assert!(
        matches!(error, GraphError::UnknownRouteTarget(ref m) if m.contains("nowhere")),
        "expected UnknownRouteTarget naming the target, got {error:?}"
    );
}

#[tokio::test]
async fn a_goto_survives_a_resume() {
    // The frontier a goto produced is checkpointed like any other, so a paused
    // run continues to the node the goto chose.
    let graph = StateGraph::with_channels(&["seen"])
        .add_node_fn("start", |_ctx| async move { Ok(NodeOutput::new().with_goto(["gated"])) })
        .add_node_fn("gated", |_ctx| async move {
            Ok(NodeOutput::new().with_update("seen", json!("gated")))
        })
        .add_edge(START, "start")
        .add_edge("gated", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
        .with_interrupt_before(&["gated"]);

    let first = graph.invoke(State::new(), ExecutionConfig::new("resume-goto")).await;
    assert!(matches!(first, Err(GraphError::Interrupted(_))), "must pause before `gated`");

    let state = graph
        .invoke(State::new(), ExecutionConfig::new("resume-goto"))
        .await
        .expect("the resume must reach the node the goto chose");
    assert_eq!(state.get("seen"), Some(&json!("gated")));
}

#[tokio::test]
async fn a_goto_is_honoured_on_the_streamed_path() {
    // Messages mode is the one that executes nodes through `execute_stream`,
    // which yields
    // events rather than a NodeOutput. A goto has to reach it too, or the same
    // graph routes one way under `invoke` and another under `stream`.
    let left_runs = Arc::new(AtomicUsize::new(0));
    let right_runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&left_runs);
    let right_counter = Arc::clone(&right_runs);

    let graph = StateGraph::with_channels(&["seen"])
        .add_node_fn("start", |_ctx| async move { Ok(NodeOutput::new().with_goto(["right"])) })
        .add_node_fn("left", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("seen", json!("left")))
            }
        })
        .add_node_fn("right", move |_ctx| {
            let counter = Arc::clone(&right_counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("seen", json!("right")))
            }
        })
        .add_edge(START, "start")
        .add_edge("start", "left")
        .add_edge("left", END)
        .add_edge("right", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    let mut stream = Box::pin(graph.stream(
        State::new(),
        ExecutionConfig::new("streamed-goto"),
        StreamMode::Messages,
    ));
    while let Some(item) = stream.next().await {
        item.expect("the streamed run must not fail");
    }

    assert_eq!(
        left_runs.load(Ordering::SeqCst),
        0,
        "the streamed path must honour the goto and skip the declared edge"
    );
    assert_eq!(
        right_runs.load(Ordering::SeqCst),
        1,
        "the node the goto named must have run exactly once"
    );
}

/// An agent that answers with a fixed word, so the routing decision is the thing
/// under test rather than a model's judgement.
struct FixedAgent {
    name: String,
    answer: String,
}

#[async_trait::async_trait]
impl adk_core::Agent for FixedAgent {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "answers with a fixed word"
    }
    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }
    async fn run(
        &self,
        _ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<adk_core::EventStream> {
        let mut event = adk_core::Event::new(&self.name);
        event.set_content(adk_core::Content::new("model").with_text(&self.answer));
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }
}

#[tokio::test]
async fn an_agent_node_routes_on_what_it_answered() {
    use adk_graph::node::AgentNode;

    let classifier =
        Arc::new(FixedAgent { name: "classifier".to_string(), answer: "refund".to_string() });

    let graph = StateGraph::with_channels(&["category", "handled"])
        .add_node(
            AgentNode::new(classifier as Arc<dyn adk_core::Agent>)
                .with_input_mapper(|_state: &State| adk_core::Content::new("user").with_text("hi"))
                .with_output_mapper(|events: &[adk_core::Event]| {
                    let text: String = events
                        .iter()
                        .filter_map(|event| event.content())
                        .flat_map(|content| content.parts.iter().filter_map(|part| part.text()))
                        .collect();
                    let mut updates = std::collections::HashMap::new();
                    updates.insert("category".to_string(), json!(text.trim()));
                    updates
                })
                // The agent's answer decides the branch. No edge declares it.
                .with_goto_mapper(
                    |updates: &std::collections::HashMap<String, serde_json::Value>| match updates
                        .get("category")
                        .and_then(|v| v.as_str())
                    {
                        Some("refund") => Some(vec!["refund_desk".to_string()]),
                        Some(_) => Some(vec!["general_desk".to_string()]),
                        None => None,
                    },
                ),
        )
        .add_node_fn("refund_desk", |_ctx| async move {
            Ok(NodeOutput::new().with_update("handled", json!("refund_desk")))
        })
        .add_node_fn("general_desk", |_ctx| async move {
            Ok(NodeOutput::new().with_update("handled", json!("general_desk")))
        })
        .add_edge(START, "classifier")
        .add_edge("refund_desk", END)
        .add_edge("general_desk", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("agent-goto")).await.unwrap();
    assert_eq!(state.get("category"), Some(&json!("refund")));
    assert_eq!(state.get("handled"), Some(&json!("refund_desk")));
}

#[tokio::test]
async fn an_agent_node_without_a_goto_mapper_follows_its_edges() {
    use adk_graph::node::AgentNode;

    let agent = Arc::new(FixedAgent { name: "plain".to_string(), answer: "x".to_string() });

    let graph = StateGraph::with_channels(&["handled"])
        .add_node(
            AgentNode::new(agent as Arc<dyn adk_core::Agent>)
                .with_input_mapper(|_state: &State| adk_core::Content::new("user").with_text("hi"))
                .with_output_mapper(|_events: &[adk_core::Event]| std::collections::HashMap::new()),
        )
        .add_node_fn("next", |_ctx| async move {
            Ok(NodeOutput::new().with_update("handled", json!("next")))
        })
        .add_edge(START, "plain")
        .add_edge("plain", "next")
        .add_edge("next", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("agent-plain")).await.unwrap();
    assert_eq!(state.get("handled"), Some(&json!("next")));
}
