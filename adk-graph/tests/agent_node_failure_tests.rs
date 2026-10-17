//! An error from the agent inside an `AgentNode` fails the node.
//!
//! `AgentNode::execute` discarded every `Err` the agent yielded, so a model
//! failure, a guardrail block, or a budget stop became a node that succeeded with
//! whatever text arrived first. Retry policies never fired and the nodes after it
//! ran on a missing answer.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::{Agent, Content, Event, EventStream, InvocationContext};
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::StateGraph;
use adk_graph::node::{AgentNode, ExecutionConfig, NodeOutput};
use adk_graph::retry::RetryPolicy;
use adk_graph::state::State;
use async_trait::async_trait;
use serde_json::json;

/// Yields part of an answer, then a rate-limit error, for its first `failures`
/// runs; then answers in full.
struct FlakyAgent {
    runs: Arc<AtomicUsize>,
    failures: usize,
}

#[async_trait]
impl Agent for FlakyAgent {
    fn name(&self) -> &str {
        "writer"
    }

    fn description(&self) -> &str {
        "fails with a rate limit, then answers"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        let run = self.runs.fetch_add(1, Ordering::SeqCst);
        let mut partial = Event::new("invocation");
        partial.set_content(Content::new("model").with_text("half an answer"));
        let items: Vec<adk_core::Result<Event>> = if run < self.failures {
            vec![
                Ok(partial),
                Err(adk_core::AdkError::new(
                    adk_core::ErrorComponent::Model,
                    adk_core::ErrorCategory::RateLimited,
                    "model.test.rate_limited",
                    "rate limited",
                )),
            ]
        } else {
            let mut full = Event::new("invocation");
            full.set_content(Content::new("model").with_text("the full answer"));
            vec![Ok(full)]
        };
        Ok(Box::pin(futures::stream::iter(items)))
    }
}

fn graph_with(agent: FlakyAgent, published: Arc<AtomicUsize>) -> adk_graph::graph::CompiledGraph {
    StateGraph::with_channels(&["messages", "published"])
        .add_node(AgentNode::new(Arc::new(agent)))
        .add_node_fn("publish", move |_| {
            let published = Arc::clone(&published);
            async move {
                published.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("published", json!(true)))
            }
        })
        .add_edge(START, "writer")
        .add_edge("writer", "publish")
        .add_edge("publish", END)
        .compile()
        .unwrap()
}

#[tokio::test]
async fn an_agent_error_fails_the_node_and_stops_the_graph() {
    let runs = Arc::new(AtomicUsize::new(0));
    let published = Arc::new(AtomicUsize::new(0));
    let graph = graph_with(
        FlakyAgent { runs: Arc::clone(&runs), failures: usize::MAX },
        Arc::clone(&published),
    );

    let outcome = graph.invoke(State::new(), ExecutionConfig::new("agent-error")).await;

    let message = match &outcome {
        Err(GraphError::NodeExecutionFailed { node, message }) if node == "writer" => {
            message.clone()
        }
        other => panic!("expected the writer node to fail, got {other:?}"),
    };
    assert!(message.contains("rate limited"), "the agent's error must be reported: {message}");
    assert_eq!(published.load(Ordering::SeqCst), 0, "nothing after a failed node may run");
}

#[tokio::test]
async fn a_retry_policy_retries_an_agent_error() {
    let runs = Arc::new(AtomicUsize::new(0));
    let published = Arc::new(AtomicUsize::new(0));
    let graph =
        graph_with(FlakyAgent { runs: Arc::clone(&runs), failures: 2 }, Arc::clone(&published))
            .with_node_retry(
                "writer",
                RetryPolicy::new(3).with_initial_delay(Duration::from_millis(1)).with_jitter(0.0),
            );

    let state = graph.invoke(State::new(), ExecutionConfig::new("agent-retry")).await.unwrap();

    assert_eq!(
        (runs.load(Ordering::SeqCst), published.load(Ordering::SeqCst), state.get("messages")),
        (3, 1, Some(&json!([{"role": "assistant", "content": "the full answer"}])))
    );
}
