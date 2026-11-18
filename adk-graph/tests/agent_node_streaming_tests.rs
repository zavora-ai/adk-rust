//! Does an agent node hand the next node its whole answer when the model streams?
//!
//! A streamed response reaches an `AgentNode` as partial chunks followed by one
//! non-partial event that carries the complete content. The default output mapper
//! wrote a `messages` entry per chunk, so the next node's default input mapper, which
//! reads the last entry, received only the last chunk: `"."` instead of the sentence.

use crate::support;

use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, Event, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, RunConfig,
    StreamingMode,
};
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{AgentNode, ExecutionConfig};
use adk_graph::state::{State, StateSchema};
use adk_graph::stream::{StreamEvent, StreamMode};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

const CHUNKS: [&str; 3] = ["Your refund", " is approved", "."];
const REPLY: &str = "Your refund is approved.";
const QUESTION: &str = "Is my refund approved?";

/// How a streamed response ends, as providers end one.
#[derive(Clone, Copy, Debug)]
enum Ending {
    /// A last non-partial response with no content, as a usage-only final chunk is.
    Bare,
    /// A last non-partial response carrying the whole text, marked complete.
    Snapshot,
}

/// Answers `REPLY` in `CHUNKS` when asked to stream, and whole otherwise.
struct Streaming(Ending);

#[async_trait]
impl Llm for Streaming {
    fn name(&self) -> &str {
        "scripted-stream"
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let text = |text: &str| Some(Content::new("model").with_text(text));
        let responses = if stream {
            let mut responses: Vec<LlmResponse> = CHUNKS
                .iter()
                .map(|chunk| LlmResponse {
                    content: text(chunk),
                    partial: true,
                    ..Default::default()
                })
                .collect();
            responses.push(match self.0 {
                Ending::Bare => LlmResponse { turn_complete: true, ..Default::default() },
                Ending::Snapshot => LlmResponse {
                    content: text(REPLY),
                    turn_complete: true,
                    provider_metadata: Some(json!({"content_complete": true})),
                    ..Default::default()
                },
            });
            responses
        } else {
            vec![LlmResponse { content: text(REPLY), turn_complete: true, ..Default::default() }]
        };
        Ok(Box::pin(futures::stream::iter(responses.into_iter().map(Ok))))
    }
}

/// Records the user text of each request and answers `Noted.`.
#[derive(Default)]
struct Recording(Mutex<Vec<String>>);

#[async_trait]
impl Llm for Recording {
    fn name(&self) -> &str {
        "scripted-record"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let asked = request
            .contents
            .iter()
            .rev()
            .find(|content| content.role == "user")
            .map(|content| content.parts.iter().filter_map(Part::text).collect::<String>())
            .unwrap_or_default();
        self.0.lock().unwrap().push(asked);
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("Noted.")),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn agent(name: &str, model: Arc<dyn Llm>) -> Arc<dyn Agent> {
    Arc::new(LlmAgentBuilder::new(name).model(model).build().expect("agent"))
}

/// `writer -> reader`, both with the default mappers, `messages` appending.
fn two_nodes(ending: Ending, recorded: &Arc<Recording>) -> CompiledGraph {
    StateGraph::new(StateSchema::builder().list_channel("messages").build())
        .add_node(AgentNode::new(agent("writer", Arc::new(Streaming(ending)))))
        .add_node(AgentNode::new(agent("reader", Arc::clone(recorded) as Arc<dyn Llm>)))
        .add_edge(START, "writer")
        .add_edge("writer", "reader")
        .add_edge("reader", END)
        .compile()
        .expect("compile")
}

fn question() -> State {
    State::from([("messages".to_string(), json!([{"role": "user", "content": QUESTION}]))])
}

fn run_config(mode: StreamingMode) -> RunConfig {
    RunConfig { streaming_mode: mode, ..RunConfig::default() }
}

/// What the reader was asked, and the conversation the graph ends with.
fn expected() -> (Vec<String>, Option<Value>) {
    (
        vec![REPLY.to_string()],
        Some(json!([
            {"role": "user", "content": QUESTION},
            {"role": "assistant", "content": REPLY},
            {"role": "assistant", "content": "Noted."},
        ])),
    )
}

#[tokio::test]
async fn the_next_node_receives_a_streamed_reply_whole() {
    for ending in [Ending::Bare, Ending::Snapshot] {
        let recorded = Arc::new(Recording::default());
        let graph = two_nodes(ending, &recorded);

        let state = graph
            .invoke_with_run_config(
                question(),
                ExecutionConfig::new(&format!("invoke-{ending:?}")),
                run_config(StreamingMode::SSE),
            )
            .await
            .expect("invoke");

        let seen = recorded.0.lock().unwrap().clone();
        assert_eq!((seen, state.get("messages").cloned()), expected(), "{ending:?}");
    }
}

#[tokio::test]
async fn a_messages_stream_hands_the_next_node_the_whole_reply() {
    for ending in [Ending::Bare, Ending::Snapshot] {
        let recorded = Arc::new(Recording::default());
        let graph = two_nodes(ending, &recorded);

        let mut stream = Box::pin(graph.stream_with_run_config(
            question(),
            ExecutionConfig::new(&format!("stream-{ending:?}")),
            StreamMode::Messages,
            run_config(StreamingMode::SSE),
        ));
        let mut done = None;
        while let Some(event) = stream.next().await {
            if let StreamEvent::Done { state, .. } = event.expect("event") {
                done = Some(state);
            }
        }

        let seen = recorded.0.lock().unwrap().clone();
        let messages = done.and_then(|state| state.get("messages").cloned());
        assert_eq!((seen, messages), expected(), "{ending:?}");
    }
}

#[tokio::test]
async fn a_streamed_and_a_whole_reply_write_the_same_state() {
    let mut states = Vec::new();
    for mode in [StreamingMode::SSE, StreamingMode::None] {
        let recorded = Arc::new(Recording::default());
        let state = two_nodes(Ending::Bare, &recorded)
            .invoke_with_run_config(
                question(),
                ExecutionConfig::new(&format!("parity-{mode:?}")),
                run_config(mode),
            )
            .await
            .expect("invoke");
        states.push(state.get("messages").cloned());
    }

    assert_eq!(states, vec![expected().1, expected().1]);
}

#[tokio::test]
async fn a_graph_agent_answers_with_the_whole_streamed_reply() {
    let graph = GraphAgent::builder("desk")
        .channels(&["input", "messages", "session_id"])
        .node(AgentNode::new(agent("writer", Arc::new(Streaming(Ending::Bare)))))
        .edge(START, "writer")
        .edge("writer", END)
        .build()
        .expect("build");

    let events: Vec<Event> = graph
        .run(support::test_context_with_text(QUESTION))
        .await
        .expect("run")
        .map(|event| event.expect("event"))
        .collect()
        .await;

    let answer = events
        .last()
        .and_then(Event::content)
        .map(|content| content.parts.iter().filter_map(Part::text).collect::<String>());
    assert_eq!(answer.as_deref(), Some(REPLY));
}
