//! Graph nodes see the run's acting context: function nodes through
//! `NodeContext::acting_context`, agent nodes through their invocation context.

use adk_core::{
    ActingContext, Agent, AgentId, AgentPrincipal, EventStream, InvocationContext, Result,
    RunConfig,
};
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{AgentNode, ExecutionConfig, NodeOutput};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

struct RecordingAgent {
    seen: Arc<Mutex<Vec<ActingContext>>>,
}

#[async_trait]
impl Agent for RecordingAgent {
    fn name(&self) -> &str {
        "reviewer"
    }
    fn description(&self) -> &str {
        "records its acting context"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        self.seen.lock().unwrap().push(ctx.acting_context());
        Ok(Box::pin(futures::stream::empty()))
    }
}

#[tokio::test]
async fn nodes_see_the_configured_acting_context() {
    let principal = AgentPrincipal::new(AgentId::new("spiffe://acme.test/agent/pipeline").unwrap())
        .with_tenant("acme");
    let run_config = RunConfig::builder()
        .acting_context(ActingContext::new(principal.clone(), "inv-graph"))
        .build();

    let function_seen = Arc::new(Mutex::new(Vec::new()));
    let agent_seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = function_seen.clone();
    let agent = AgentNode::new(Arc::new(RecordingAgent { seen: agent_seen.clone() }))
        .with_input_mapper(|_state| adk_core::Content::new("user").with_text("go"))
        .with_output_mapper(|_events| HashMap::new());
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn("prepare", move |ctx| {
            let recorder = recorder.clone();
            async move {
                recorder.lock().unwrap().push(ctx.acting_context().cloned());
                Ok(NodeOutput::new())
            }
        })
        .add_node(agent)
        .add_edge(START, "prepare")
        .add_edge("prepare", "reviewer")
        .add_edge("reviewer", END)
        .compile()
        .unwrap();

    graph
        .invoke_with_run_config(
            adk_graph::state::State::new(),
            ExecutionConfig::new("thread-1"),
            run_config,
        )
        .await
        .unwrap();

    let function_seen = function_seen.lock().unwrap();
    assert_eq!(function_seen.len(), 1);
    assert_eq!(function_seen[0].as_ref().map(|acting| &acting.agent), Some(&principal));
    let agent_seen = agent_seen.lock().unwrap();
    assert_eq!(agent_seen.len(), 1);
    assert_eq!(agent_seen[0].agent, principal, "the agent node acts as the configured principal");
}

#[tokio::test]
async fn without_configuration_agent_nodes_derive_their_identity() {
    let agent_seen = Arc::new(Mutex::new(Vec::new()));
    let agent = AgentNode::new(Arc::new(RecordingAgent { seen: agent_seen.clone() }))
        .with_input_mapper(|_state| adk_core::Content::new("user").with_text("go"))
        .with_output_mapper(|_events| HashMap::new());
    let graph = StateGraph::with_channels(&["value"])
        .add_node(agent)
        .add_edge(START, "reviewer")
        .add_edge("reviewer", END)
        .compile()
        .unwrap();

    graph.invoke(adk_graph::state::State::new(), ExecutionConfig::new("thread-1")).await.unwrap();

    let agent_seen = agent_seen.lock().unwrap();
    assert!(agent_seen[0].agent.id.as_str().starts_with("urn:adk:agent:"), "{agent_seen:?}");
    assert!(agent_seen[0].agent.id.as_str().ends_with("/reviewer"), "{agent_seen:?}");
}
