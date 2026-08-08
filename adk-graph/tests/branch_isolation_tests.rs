//! Parallel branches must not read each other's work.
//!
//! adk-go reaches this by tagging every event with a dot-delimited branch and
//! filtering history by segment prefix, so an agent on `a.b` sees `a` and the root
//! but not `a.c`. `adk-graph` reaches the same result differently: every
//! `AgentNode` invocation builds its own `GraphInvocationContext` with a fresh
//! session, so there is no shared event log for a sibling to read.
//!
//! These tests pin that property, because it is a consequence of how the context
//! is built rather than a rule anything enforces — a later change that shared one
//! session between nodes would break it silently.

use adk_core::{Agent, Content, Event, EventStream, InvocationContext, Result as AdkResult};
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{AgentNode, ExecutionConfig};
use adk_graph::state::State;
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};

/// An agent that records what it could see, and says its own name.
struct Observer {
    name: String,
    /// Text of every event this agent found in its session history.
    seen: Arc<Mutex<Vec<String>>>,
    /// The branch this agent ran on.
    branch: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl Agent for Observer {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "records what it can see"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        *self.branch.lock().expect("branch") = Some(ctx.branch().to_string());

        // Everything already in this invocation's history.
        let history: Vec<String> = ctx
            .session()
            .conversation_history()
            .iter()
            .flat_map(|content| {
                content.parts.iter().filter_map(|part| part.text().map(str::to_string))
            })
            .collect();
        self.seen.lock().expect("seen").extend(history);

        let mut event = Event::new(&self.name);
        event.set_content(Content::new("assistant").with_text(format!("{} ran", self.name)));
        let stream = futures::stream::iter(vec![Ok(event)]);
        Ok(Box::pin(stream))
    }
}

/// Two agents fanned out in parallel cannot see each other's events.
#[tokio::test]
async fn a_parallel_branch_cannot_see_its_siblings_events() {
    let left_seen = Arc::new(Mutex::new(Vec::new()));
    let right_seen = Arc::new(Mutex::new(Vec::new()));
    let left_branch = Arc::new(Mutex::new(None));
    let right_branch = Arc::new(Mutex::new(None));

    let left = Arc::new(Observer {
        name: "left".to_string(),
        seen: Arc::clone(&left_seen),
        branch: Arc::clone(&left_branch),
    }) as Arc<dyn Agent>;
    let right = Arc::new(Observer {
        name: "right".to_string(),
        seen: Arc::clone(&right_seen),
        branch: Arc::clone(&right_branch),
    }) as Arc<dyn Agent>;

    let graph = StateGraph::with_channels(&["left_done", "right_done"])
        .add_node(
            AgentNode::new(left)
                .with_input_mapper(|_state| Content::new("user").with_text("left task"))
                .with_output_mapper(|_events| {
                    let mut updates = std::collections::HashMap::new();
                    updates.insert("left_done".to_string(), json!(true));
                    updates
                }),
        )
        .add_node(
            AgentNode::new(right)
                .with_input_mapper(|_state| Content::new("user").with_text("right task"))
                .with_output_mapper(|_events| {
                    let mut updates = std::collections::HashMap::new();
                    updates.insert("right_done".to_string(), json!(true));
                    updates
                }),
        )
        .add_edge(START, "left")
        .add_edge(START, "right")
        .add_edge("left", END)
        .add_edge("right", END)
        .compile()
        .unwrap();

    graph.invoke(State::new(), ExecutionConfig::new("branches")).await.unwrap();

    let left_history = left_seen.lock().expect("seen").clone();
    let right_history = right_seen.lock().expect("seen").clone();

    assert!(
        !left_history.iter().any(|text| text.contains("right")),
        "left saw the right branch: {left_history:?}"
    );
    assert!(
        !right_history.iter().any(|text| text.contains("left")),
        "right saw the left branch: {right_history:?}"
    );

    // Each saw only the task it was given.
    assert_eq!(left_history, vec!["left task".to_string()]);
    assert_eq!(right_history, vec!["right task".to_string()]);
}

/// A node inside a graph runs on its own branch, so its events are attributable.
#[tokio::test]
async fn a_node_runs_on_its_own_branch() {
    let branch = Arc::new(Mutex::new(None));
    let agent = Arc::new(Observer {
        name: "worker".to_string(),
        seen: Arc::new(Mutex::new(Vec::new())),
        branch: Arc::clone(&branch),
    }) as Arc<dyn Agent>;

    let graph = StateGraph::with_channels(&["done"])
        .add_node(AgentNode::new(agent).with_output_mapper(|_events| {
            let mut updates = std::collections::HashMap::new();
            updates.insert("done".to_string(), json!(true));
            updates
        }))
        .add_edge(START, "worker")
        .add_edge("worker", END)
        .compile()
        .unwrap();

    graph.invoke(State::new(), ExecutionConfig::new("branch-name")).await.unwrap();

    let observed = branch.lock().expect("branch").clone().expect("the agent recorded its branch");
    assert!(!observed.is_empty(), "a node must run on a named branch");
}
