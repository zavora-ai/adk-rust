//! Tool confirmation must pause a graph without flattening its lifecycle.

use adk_core::{
    Agent, Content, Event, EventStream, InvocationContext, RunConfig, ToolConfirmationDecision,
    ToolConfirmationRequest,
};
use adk_graph::agent::GraphAgent;
use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{AgentNode, ExecutionConfig};
use adk_graph::state::State;
use adk_graph::stream::{StreamEvent, StreamMode};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

mod support;
use std::sync::atomic::{AtomicUsize, Ordering};

struct ConfirmationAgent {
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Agent for ConfirmationAgent {
    fn name(&self) -> &str {
        "sensitive_agent"
    }

    fn description(&self) -> &str {
        "requests confirmation before changing state"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let approved = ctx.run_config().tool_confirmation_decisions.get("call-1")
            == Some(&ToolConfirmationDecision::Approve);

        let stream = async_stream::stream! {
            if approved {
                let mut event = Event::new(ctx.invocation_id());
                event.set_content(Content::new("assistant").with_text("authorized"));
                yield Ok(event);
            } else {
                let mut event = Event::new(ctx.invocation_id());
                event.llm_response.interrupted = true;
                event.actions.tool_confirmation = Some(ToolConfirmationRequest {
                    tool_name: "delete_file".to_string(),
                    function_call_id: Some("call-1".to_string()),
                    args: json!({ "path": "/tmp/report.txt" }),
                });
                yield Ok(event);
            }
        };
        Ok(Box::pin(stream))
    }
}

#[tokio::test]
async fn graph_streams_a_persisted_tool_confirmation_and_resumes_with_a_decision() {
    let runs = Arc::new(AtomicUsize::new(0));
    let graph = StateGraph::with_channels(&["messages"])
        .add_node(AgentNode::new(Arc::new(ConfirmationAgent { runs: Arc::clone(&runs) })))
        .add_edge(START, "sensitive_agent")
        .add_edge("sensitive_agent", END)
        .compile()
        .expect("compile")
        .with_checkpointer(MemoryCheckpointer::new());

    let mut stream = Box::pin(graph.stream(
        State::new(),
        ExecutionConfig::new("confirmation-thread"),
        StreamMode::Debug,
    ));
    let pause = loop {
        if let StreamEvent::ToolConfirmationRequired { node, request, thread_id, checkpoint_id } =
            stream.next().await.expect("stream event").expect("graph event")
        {
            break (node, request, thread_id, checkpoint_id);
        }
    };

    assert_eq!(pause.0, "sensitive_agent");
    assert_eq!(pause.1.tool_name, "delete_file");
    assert_eq!(pause.1.function_call_id.as_deref(), Some("call-1"));
    assert_eq!(pause.2, "confirmation-thread");
    assert!(!pause.3.is_empty(), "a confirmation event must be resumable");
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    let decisions = HashMap::from([(String::from("call-1"), ToolConfirmationDecision::Approve)]);
    let config = ExecutionConfig::new("confirmation-thread")
        .with_run_config(RunConfig::builder().tool_confirmation_decisions(decisions).build());
    let events = graph.stream(State::new(), config, StreamMode::Debug).collect::<Vec<_>>().await;

    assert!(
        events
            .iter()
            .all(|event| !matches!(event, Ok(StreamEvent::ToolConfirmationRequired { .. })))
    );
    assert!(events.iter().any(|event| matches!(event, Ok(StreamEvent::Done { .. }))));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn graph_agent_preserves_tool_confirmation_for_runner_compatibility() {
    let graph = StateGraph::with_channels(&["messages"])
        .add_node(AgentNode::new(Arc::new(ConfirmationAgent {
            runs: Arc::new(AtomicUsize::new(0)),
        })))
        .add_edge(START, "sensitive_agent")
        .add_edge("sensitive_agent", END)
        .compile()
        .expect("compile")
        .with_checkpointer(MemoryCheckpointer::new());
    let graph_agent = GraphAgent::from_graph("graph", graph);

    let mut events = graph_agent
        .run(support::test_context("graph-agent-confirmation"))
        .await
        .expect("run graph agent");
    let event = events.next().await.expect("event").expect("valid event");

    assert!(event.llm_response.interrupted);
    assert_eq!(
        event.actions.tool_confirmation.as_ref().map(|request| request.tool_name.as_str()),
        Some("delete_file")
    );
}

#[tokio::test]
async fn a_confirmation_pause_does_not_replay_completed_frontier_nodes() {
    let agent_runs = Arc::new(AtomicUsize::new(0));
    let completed_runs = Arc::new(AtomicUsize::new(0));
    let completed_for_node = Arc::clone(&completed_runs);
    let graph = StateGraph::with_channels(&["done", "messages"])
        .add_node_fn("completed", move |_ctx| {
            let completed_runs = Arc::clone(&completed_for_node);
            async move {
                completed_runs.fetch_add(1, Ordering::SeqCst);
                Ok(adk_graph::node::NodeOutput::new().with_update("done", json!(true)))
            }
        })
        .add_node(AgentNode::new(Arc::new(ConfirmationAgent { runs: Arc::clone(&agent_runs) })))
        .add_edge(START, "completed")
        .add_edge(START, "sensitive_agent")
        .add_edge("completed", END)
        .add_edge("sensitive_agent", END)
        .compile()
        .expect("compile")
        .with_checkpointer(MemoryCheckpointer::new());

    let first = graph
        .stream(
            State::new(),
            ExecutionConfig::new("parallel-confirmation-thread"),
            StreamMode::Debug,
        )
        .collect::<Vec<_>>()
        .await;
    assert!(
        first.iter().any(|event| matches!(event, Ok(StreamEvent::ToolConfirmationRequired { .. })))
    );
    assert_eq!(completed_runs.load(Ordering::SeqCst), 1);

    let decisions = HashMap::from([(String::from("call-1"), ToolConfirmationDecision::Approve)]);
    let resumed = graph
        .stream(
            State::new(),
            ExecutionConfig::new("parallel-confirmation-thread").with_run_config(
                RunConfig::builder().tool_confirmation_decisions(decisions).build(),
            ),
            StreamMode::Debug,
        )
        .collect::<Vec<_>>()
        .await;

    assert!(resumed.iter().any(|event| matches!(event, Ok(StreamEvent::Done { .. }))));
    assert_eq!(completed_runs.load(Ordering::SeqCst), 1, "completed node must not replay");
    assert_eq!(agent_runs.load(Ordering::SeqCst), 2);
}
