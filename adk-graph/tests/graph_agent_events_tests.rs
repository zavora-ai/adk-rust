//! What an agent inside a `GraphAgent` sees, and what the caller sees of it.
//!
//! An `AgentNode` ran its agent on a synthetic session: the session id was the
//! composite thread id and the state was empty, so an instruction template naming
//! a session key failed. `GraphAgent` ran the whole graph before yielding, and
//! yielded only the output mapper's events, so a caller saw nothing until the
//! graph finished and the session never held the inner agents' turns.

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::{Agent, Content, Event, EventStream, InvocationContext};
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{AgentNode, ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

/// What an agent saw of its context, recorded when it ran.
#[derive(Debug, Default, Clone, PartialEq)]
struct Seen {
    session_id: String,
    context_session_id: String,
    customer: Option<Value>,
    topic: Option<Value>,
    instruction: String,
}

/// Records what it sees, then answers with `answer` after `delay`.
struct Probe {
    name: &'static str,
    answer: &'static str,
    delay: Duration,
    seen: Arc<Mutex<Option<Seen>>>,
}

#[async_trait]
impl Agent for Probe {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "records its context"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        let instruction =
            adk_core::inject_session_state(ctx.as_ref(), "Greet {customer?} about {topic?}.")
                .await?;
        *self.seen.lock().unwrap() = Some(Seen {
            session_id: ctx.session().id().to_string(),
            context_session_id: ctx.session_id().to_string(),
            customer: ctx.session().state().get("customer"),
            topic: ctx.session().state().get("topic"),
            instruction,
        });
        let delay = self.delay;
        // No branch, as an `LlmAgent`'s model events carry none: the node attributes it.
        let mut event = Event::new(ctx.invocation_id());
        event.author = self.name.to_string();
        event.set_content(Content::new("model").with_text(self.answer));
        Ok(Box::pin(futures::stream::once(async move {
            tokio::time::sleep(delay).await;
            Ok(event)
        })))
    }
}

fn probe(
    name: &'static str,
    answer: &'static str,
    delay: Duration,
) -> (Probe, Arc<Mutex<Option<Seen>>>) {
    let seen = Arc::new(Mutex::new(None));
    (Probe { name, answer, delay, seen: Arc::clone(&seen) }, seen)
}

#[tokio::test]
async fn an_agent_node_sees_the_callers_session_with_graph_state_on_top() {
    let (greeter, seen) = probe("greeter", "hello", Duration::ZERO);
    let graph = StateGraph::with_channels(&["topic", "messages"])
        .add_node_fn("set_topic", |_| async {
            Ok(NodeOutput::new().with_update("topic", json!("a delayed shipment")))
        })
        .add_node(AgentNode::new(Arc::new(greeter)))
        .add_edge(START, "set_topic")
        .add_edge("set_topic", "greeter")
        .add_edge("greeter", END)
        .compile()
        .unwrap();
    let agent = GraphAgent::from_graph("concierge", graph);
    let context = support::test_context_with_state(
        "go",
        HashMap::from([("customer".to_string(), json!("Ada Lovelace"))]),
    );

    let events: Vec<_> = agent.run(context).await.unwrap().collect().await;

    assert!(events.iter().all(Result::is_ok), "the turn must succeed: {events:?}");
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(Seen {
            session_id: "caller-session".to_string(),
            context_session_id: "caller-session".to_string(),
            customer: Some(json!("Ada Lovelace")),
            topic: Some(json!("a delayed shipment")),
            instruction: "Greet Ada Lovelace about a delayed shipment.".to_string(),
        })
    );
}

#[tokio::test]
async fn graph_state_takes_precedence_over_session_state() {
    let (greeter, seen) = probe("greeter", "hello", Duration::ZERO);
    let graph = StateGraph::with_channels(&["topic", "messages"])
        .add_node(AgentNode::new(Arc::new(greeter)))
        .add_edge(START, "greeter")
        .add_edge("greeter", END)
        .compile()
        .unwrap();
    let context = support::test_context_with_state(
        "go",
        HashMap::from([
            ("customer".to_string(), json!("Ada")),
            ("topic".to_string(), json!("from the session")),
        ]),
    );

    let state = graph
        .invoke(
            State::from([("topic".to_string(), json!("from the graph"))]),
            ExecutionConfig::new("precedence").with_parent_context(context),
        )
        .await
        .unwrap();

    assert!(state.contains_key("messages"));
    let seen = seen.lock().unwrap().clone().expect("the agent ran");
    assert_eq!(seen.topic, Some(json!("from the graph")));
}

#[tokio::test]
async fn standalone_an_agent_node_keeps_the_thread_as_its_session() {
    let (greeter, seen) = probe("greeter", "hello", Duration::ZERO);
    let graph = StateGraph::with_channels(&["customer", "topic", "messages"])
        .add_node(AgentNode::new(Arc::new(greeter)))
        .add_edge(START, "greeter")
        .add_edge("greeter", END)
        .compile()
        .unwrap();

    graph
        .invoke(
            State::from([
                ("customer".to_string(), json!("Grace")),
                ("topic".to_string(), json!("billing")),
            ]),
            ExecutionConfig::new("standalone-thread"),
        )
        .await
        .unwrap();

    let seen = seen.lock().unwrap().clone().expect("the agent ran");
    assert_eq!(
        (seen.session_id.as_str(), seen.instruction.as_str()),
        ("standalone-thread", "Greet Grace about billing.")
    );
}

/// Two agents in sequence; the second takes `delay` to answer.
fn two_agent_graph(delay: Duration) -> GraphAgent {
    let (researcher, _) = probe("researcher", "a finding", Duration::ZERO);
    let (writer, _) = probe("writer", "a report", delay);
    let graph = StateGraph::with_channels(&["messages"])
        .add_node(AgentNode::new(Arc::new(researcher)))
        .add_node(AgentNode::new(Arc::new(writer)))
        .add_edge(START, "researcher")
        .add_edge("researcher", "writer")
        .add_edge("writer", END)
        .compile()
        .unwrap();
    GraphAgent::from_graph("incident", graph)
}

#[tokio::test]
async fn node_agents_events_reach_the_caller_while_the_graph_runs() {
    let agent = two_agent_graph(Duration::from_millis(300));
    let started = Instant::now();
    let mut stream = agent.run(support::test_context_with_text("investigate")).await.unwrap();

    let mut arrivals = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.expect("event");
        arrivals.push((started.elapsed(), event));
    }
    let ended = started.elapsed();

    let authors: Vec<&str> = arrivals.iter().map(|(_, event)| event.author.as_str()).collect();
    assert_eq!(authors, ["researcher", "writer", "incident"]);
    let (researcher_at, _) = &arrivals[0];
    assert!(
        ended - *researcher_at >= Duration::from_millis(250),
        "the researcher's event arrived {researcher_at:?} into a {ended:?} turn"
    );
}

#[tokio::test]
async fn every_event_of_a_turn_carries_its_invocation_and_branch() {
    let agent = two_agent_graph(Duration::ZERO);

    let events: Vec<Event> = agent
        .run(support::test_context_with_text("investigate"))
        .await
        .unwrap()
        .map(|event| event.expect("event"))
        .collect()
        .await;

    assert_eq!(
        events
            .iter()
            .map(|event| (event.invocation_id.as_str(), event.branch.as_str()))
            .collect::<Vec<_>>(),
        [
            ("caller-invocation", "caller-branch.researcher"),
            ("caller-invocation", "caller-branch.writer"),
            ("caller-invocation", "caller-branch"),
        ]
    );
}
