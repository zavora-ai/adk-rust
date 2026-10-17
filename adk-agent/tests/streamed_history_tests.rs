//! A streamed response is replaced by its complete terminal snapshot, so a later agent in
//! the same invocation sees each response once — not its deltas plus the snapshot.

use adk_agent::{LlmAgentBuilder, LoopAgent, SequentialAgent};
use adk_core::{
    Agent, Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result,
    SessionId, Tool, ToolContext, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// Replies to each request with the next scripted chunk sequence and records the request.
struct StreamingModel {
    requests: Mutex<Vec<LlmRequest>>,
    turns: Mutex<VecDeque<Vec<LlmResponse>>>,
}

impl StreamingModel {
    fn new(turns: Vec<Vec<LlmResponse>>) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::new(Vec::new()), turns: Mutex::new(turns.into()) })
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

fn chunk(parts: Vec<Part>, partial: bool) -> LlmResponse {
    LlmResponse {
        content: Some(Content { role: "model".to_string(), parts }),
        partial,
        turn_complete: !partial,
        finish_reason: (!partial).then_some(FinishReason::Stop),
        ..Default::default()
    }
}

fn text(text: &str) -> Part {
    Part::Text { text: text.to_string() }
}

#[async_trait]
impl Llm for StreamingModel {
    fn name(&self) -> &str {
        "streaming-model"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(request);
        let turn = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![chunk(vec![text("done")], false)]);
        Ok(Box::pin(futures::stream::iter(turn.into_iter().map(Ok))))
    }
}

struct LookupTool;

#[async_trait]
impl Tool for LookupTool {
    fn name(&self) -> &str {
        "lookup"
    }

    fn description(&self) -> &str {
        "Looks something up."
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(json!({ "found": true }))
    }
}

/// Summarises request contents as `(role, parts)` with text parts of one content joined.
fn summarise(contents: &[Content]) -> Vec<(String, Vec<String>)> {
    contents
        .iter()
        .map(|content| {
            let mut parts = Vec::new();
            let mut text = String::new();
            for part in &content.parts {
                match part {
                    Part::Text { text: chunk } => text.push_str(chunk),
                    Part::FunctionCall { name, .. } => parts.push(format!("call:{name}")),
                    Part::FunctionResponse { function_response, .. } => {
                        parts.push(format!("response:{}", function_response.name));
                    }
                    other => parts.push(format!("{other:?}")),
                }
            }
            if !text.is_empty() {
                parts.insert(0, format!("text:{text}"));
            }
            (content.role.clone(), parts)
        })
        .collect()
}

async fn run(agent: Arc<dyn Agent>, prompt: &str) {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "streamed-history".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("streamed-history")
        .agent(agent)
        .session_service(sessions as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let mut stream = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text(prompt),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
}

#[tokio::test]
async fn sequential_successor_sees_a_streamed_response_once() {
    let call = Part::FunctionCall {
        name: "lookup".to_string(),
        args: json!({}),
        id: Some("call-1".to_string()),
        thought_signature: None,
    };
    let writer_model = StreamingModel::new(vec![
        vec![chunk(vec![text("Let me ")], true), chunk(vec![text("check"), call], false)],
        vec![chunk(vec![text("Fou")], true), chunk(vec![text("nd")], false)],
    ]);
    let reviewer_model = StreamingModel::new(vec![]);
    let writer = LlmAgentBuilder::new("writer")
        .model(writer_model.clone())
        .tool(Arc::new(LookupTool))
        .build()
        .unwrap();
    let reviewer = LlmAgentBuilder::new("reviewer").model(reviewer_model.clone()).build().unwrap();
    let pipeline = SequentialAgent::new("pipeline", vec![Arc::new(writer), Arc::new(reviewer)]);

    run(Arc::new(pipeline), "question").await;

    // Each of the writer's responses reaches the reviewer once, as context.
    let requests = reviewer_model.requests();
    assert_eq!(requests.len(), 1);
    let context: Vec<String> = requests[0]
        .contents
        .iter()
        .filter(|content| adk_core::history::is_other_agent_context(content))
        .map(|content| {
            content.parts[1..].iter().filter_map(Part::text).collect::<Vec<_>>().join("|")
        })
        .collect();
    let quoted = |text: &str| {
        format!(
            "{}\n{text}\n{}",
            adk_core::history::QUOTED_CONTENT_BEGIN,
            adk_core::history::QUOTED_CONTENT_END
        )
    };
    assert_eq!(
        context,
        vec![
            format!(
                "[writer] said:\n{}|[writer] called tool `lookup` with parameters:\n{}",
                quoted("Let me check"),
                quoted("{}")
            ),
            format!("[writer] `lookup` tool returned result:\n{}", quoted(r#"{"found":true}"#)),
            format!("[writer] said:\n{}", quoted("Found")),
        ]
    );
    assert_eq!(requests[0].contents.len(), 4, "the question, then one context message per event");
}

#[tokio::test]
async fn loop_iteration_sees_a_streamed_response_once() {
    let model = StreamingModel::new(vec![
        vec![
            chunk(vec![text("Hel")], true),
            chunk(vec![text("lo")], true),
            chunk(vec![text("!")], false),
        ],
        vec![chunk(vec![text("Again")], false)],
    ]);
    let writer = LlmAgentBuilder::new("writer").model(model.clone()).build().unwrap();
    let refine = LoopAgent::new("refine", vec![Arc::new(writer)]).with_max_iterations(2);

    run(Arc::new(refine), "go").await;

    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    let second = summarise(&requests[1].contents);
    assert_eq!(
        second[..2],
        [
            ("user".to_string(), vec!["text:go".to_string()]),
            ("model".to_string(), vec!["text:Hello!".to_string()]),
        ]
    );
    // The round after the agent's own answer gets a user turn to answer.
    assert_eq!(second.len(), 3);
    assert_eq!(second[2].0, "user");
}
