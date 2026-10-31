//! An error from the agent inside an `AgentNode` fails the node.
//!
//! `AgentNode::execute` discarded every `Err` the agent yielded, so a model
//! failure, a guardrail block, or a budget stop became a node that succeeded with
//! whatever text arrived first. Retry policies never fired and the nodes after it
//! ran on a missing answer.

use crate::support;

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
use futures::StreamExt;
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

    let (node, category, code) = match &outcome {
        Err(GraphError::AgentFailed { node, source }) => {
            (node.as_str(), source.category, source.code)
        }
        other => panic!("expected the writer node to fail, got {other:?}"),
    };
    assert_eq!(
        (node, category, code),
        ("writer", adk_core::ErrorCategory::RateLimited, "model.test.rate_limited"),
        "the agent's own error must be reported"
    );
    assert_eq!(published.load(Ordering::SeqCst), 0, "nothing after a failed node may run");
}

/// The agent's error reached a `Runner` as `Internal`, so a rate limit looked like
/// a defect: not retryable, and HTTP 500 instead of 429.
#[tokio::test]
async fn a_graph_agent_reports_the_agent_errors_category() {
    let graph = graph_with(
        FlakyAgent { runs: Arc::new(AtomicUsize::new(0)), failures: usize::MAX },
        Arc::new(AtomicUsize::new(0)),
    );
    let agent = adk_graph::agent::GraphAgent::from_graph("drafts", graph);

    let mut stream = agent.run(support::test_context("drafts")).await.expect("run");
    let error = loop {
        match stream.next().await {
            Some(Err(error)) => break error,
            Some(Ok(_)) => continue,
            None => panic!("the turn must fail"),
        }
    };

    assert_eq!(
        (error.category, error.code, error.is_retryable(), error.http_status_code()),
        (adk_core::ErrorCategory::RateLimited, "model.test.rate_limited", true, 429)
    );
    assert!(error.message.contains("writer"), "the failing node is named: {}", error.message);
}

/// A subgraph's agent failure keeps the agent's error, and names the path to it.
#[tokio::test]
async fn an_agent_error_inside_a_subgraph_keeps_its_category() {
    let inner = graph_with(
        FlakyAgent { runs: Arc::new(AtomicUsize::new(0)), failures: usize::MAX },
        Arc::new(AtomicUsize::new(0)),
    );
    let graph = StateGraph::with_channels(&["messages"])
        .add_node(adk_graph::subgraph::SubgraphNode::new("drafting", Arc::new(inner)))
        .add_edge(START, "drafting")
        .add_edge("drafting", END)
        .compile()
        .unwrap();

    let outcome = graph.invoke(State::new(), ExecutionConfig::new("nested-agent-error")).await;

    match outcome {
        Err(GraphError::AgentFailed { node, source }) => {
            assert_eq!(
                (node.as_str(), source.category),
                ("drafting.writer", adk_core::ErrorCategory::RateLimited)
            );
        }
        other => panic!("expected the nested agent's failure, got {other:?}"),
    }
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
