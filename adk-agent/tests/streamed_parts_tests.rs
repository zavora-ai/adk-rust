//! A streamed response is persisted as whole parts: consecutive text and thinking deltas are
//! joined, and tool-call markup split across deltas is parsed once the text is complete.

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Content, Event, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result,
    SessionId, Tool, ToolContext, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
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
            .unwrap_or_else(|| vec![terminal(vec![text("done")])]);
        Ok(Box::pin(futures::stream::iter(turn.into_iter().map(Ok))))
    }
}

fn delta(parts: Vec<Part>) -> LlmResponse {
    LlmResponse {
        content: Some(Content { role: "model".to_string(), parts }),
        partial: true,
        ..Default::default()
    }
}

fn terminal(parts: Vec<Part>) -> LlmResponse {
    LlmResponse {
        content: (!parts.is_empty()).then(|| Content { role: "model".to_string(), parts }),
        partial: false,
        turn_complete: true,
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    }
}

fn text(text: &str) -> Part {
    Part::Text { text: text.to_string() }
}

fn thinking(thinking: &str, signature: Option<&str>) -> Part {
    Part::Thinking { thinking: thinking.to_string(), signature: signature.map(str::to_string) }
}

/// Counts its calls so a test can tell the model's tool call was executed.
struct CountingTool {
    calls: AtomicUsize,
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "lookup"
    }

    fn description(&self) -> &str {
        "Looks something up."
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "found": args }))
    }
}

/// Runs one turn and returns the session's persisted events.
async fn run(model: Arc<StreamingModel>, tool: Option<Arc<CountingTool>>) -> Vec<Event> {
    let mut builder = LlmAgentBuilder::new("assistant").model(model);
    if let Some(tool) = tool {
        builder = builder.tool(tool);
    }
    let agent = Arc::new(builder.build().unwrap());
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "streamed-parts".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("streamed-parts")
        .agent(agent)
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let mut stream = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("hello"),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    sessions
        .get(GetRequest {
            app_name: "streamed-parts".to_string(),
            user_id: "user".to_string(),
            session_id: "session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .events()
        .all()
}

/// Parts of the persisted model responses, in order.
fn model_parts(events: &[Event]) -> Vec<Vec<Part>> {
    events
        .iter()
        .filter(|event| event.author == "assistant")
        .filter_map(|event| event.content().map(|content| content.parts.clone()))
        .collect()
}

#[tokio::test]
async fn consecutive_text_and_thinking_deltas_persist_as_one_part_each() {
    let model = StreamingModel::new(vec![vec![
        delta(vec![thinking("The user ", None)]),
        delta(vec![thinking("says hello.", None)]),
        delta(vec![text("Hel")]),
        delta(vec![text("lo")]),
        delta(vec![text(" there")]),
        terminal(vec![]),
    ]]);

    let events = run(model, None).await;

    assert_eq!(
        model_parts(&events),
        vec![vec![thinking("The user says hello.", None), text("Hello there")]]
    );
}

#[tokio::test]
async fn a_signed_thinking_part_closes_its_block() {
    let model = StreamingModel::new(vec![vec![
        delta(vec![thinking("first ", None)]),
        delta(vec![thinking("block", None)]),
        delta(vec![thinking("", Some("sig-1"))]),
        delta(vec![thinking("second block", None)]),
        delta(vec![text("answer")]),
        terminal(vec![]),
    ]]);

    let events = run(model, None).await;

    assert_eq!(
        model_parts(&events),
        vec![vec![
            thinking("first block", Some("sig-1")),
            thinking("second block", None),
            text("answer"),
        ]]
    );
}

#[tokio::test]
async fn tool_call_markup_split_across_deltas_is_executed() {
    let tool = Arc::new(CountingTool { calls: AtomicUsize::new(0) });
    let model = StreamingModel::new(vec![
        vec![
            delta(vec![text("Checking. <tool_")]),
            delta(vec![text("call>\nlookup\n<arg_key>q</arg_key>\n<arg_value>ru")]),
            delta(vec![text("st</arg_value>\n</tool_call>")]),
            terminal(vec![]),
        ],
        vec![terminal(vec![text("found it")])],
    ]);

    let events = run(model.clone(), Some(tool.clone())).await;

    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    let first_response = model_parts(&events).into_iter().next().unwrap();
    assert!(
        first_response.iter().any(|part| matches!(
            part,
            Part::FunctionCall { name, args, .. } if name == "lookup" && args == &json!({"q": "rust"})
        )),
        "{first_response:?}"
    );
    assert_eq!(model.requests.lock().unwrap().len(), 2);
}
