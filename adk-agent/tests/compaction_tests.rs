//! Tests for the LlmEventSummarizer context compaction.

use adk_agent::{LlmAgentBuilder, LlmEventSummarizer};
use adk_core::{
    BaseEventsSummarizer, Content, Event, EventCompaction, EventsCompactionConfig,
    FunctionResponseData, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result, RunConfig,
    StreamingMode, ToolContext,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use async_trait::async_trait;
use chrono::{Duration, Utc};
use futures::StreamExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// A mock LLM that echoes back a fixed summary.
struct MockSummarizerLlm {
    summary_text: String,
}

#[async_trait]
impl Llm for MockSummarizerLlm {
    fn name(&self) -> &str {
        "mock-summarizer"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let content = Content::new("model").with_text(&self.summary_text);
        let response = LlmResponse::new(content);
        Ok(Box::pin(futures::stream::once(async { Ok(response) })))
    }
}

fn make_event(author: &str, text: &str) -> Event {
    let mut event = Event::new("inv-test");
    event.author = author.to_string();
    event.set_content(Content {
        role: if author == "user" { "user" } else { "model" }.to_string(),
        parts: vec![Part::Text { text: text.to_string() }],
    });
    event
}

#[tokio::test]
async fn test_summarize_empty_events_returns_none() {
    let llm = Arc::new(MockSummarizerLlm { summary_text: "summary".into() });
    let summarizer = LlmEventSummarizer::new(llm);

    let result = summarizer.summarize_events(&[]).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_summarize_produces_compaction_event() {
    let llm = Arc::new(MockSummarizerLlm {
        summary_text: "User asked about weather. Agent provided NYC forecast.".into(),
    });
    let summarizer = LlmEventSummarizer::new(llm);

    let events = vec![
        make_event("user", "What's the weather in NYC?"),
        make_event("assistant", "The weather in NYC is 72°F and sunny."),
        make_event("user", "Thanks!"),
        make_event("assistant", "You're welcome!"),
    ];

    let result = summarizer.summarize_events(&events).await.unwrap();
    assert!(result.is_some());

    let compaction_event = result.unwrap();
    assert_eq!(compaction_event.author, "system");

    // Must have compaction metadata
    let compaction = compaction_event.actions.compaction.as_ref().unwrap();
    assert_eq!(compaction.start_timestamp, events[0].timestamp);
    assert_eq!(compaction.end_timestamp, events[3].timestamp);

    // Compacted content should be the LLM summary
    assert_eq!(compaction.compacted_content.role, "model");
    let text = match &compaction.compacted_content.parts[0] {
        Part::Text { text } => text.clone(),
        _ => panic!("Expected text part"),
    };
    assert!(text.contains("weather"));
}

#[tokio::test]
async fn test_summarize_with_custom_prompt_template() {
    let llm = Arc::new(MockSummarizerLlm { summary_text: "Custom summary output".into() });
    let summarizer = LlmEventSummarizer::new(llm)
        .with_prompt_template("Summarize briefly: {conversation_history}");

    let events = vec![make_event("user", "Hello"), make_event("assistant", "Hi there")];

    let result = summarizer.summarize_events(&events).await.unwrap();
    assert!(result.is_some());

    let compaction = result.unwrap().actions.compaction.unwrap();
    let text = match &compaction.compacted_content.parts[0] {
        Part::Text { text } => text.clone(),
        _ => panic!("Expected text part"),
    };
    assert_eq!(text, "Custom summary output");
}

/// A summarizer model that records each prompt it receives.
struct RecordingSummarizerLlm {
    prompts: Mutex<Vec<String>>,
}

#[async_trait]
impl Llm for RecordingSummarizerLlm {
    fn name(&self) -> &str {
        "recording-summarizer"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let prompt =
            req.contents.iter().flat_map(|c| c.parts.iter()).filter_map(Part::text).collect();
        self.prompts.lock().unwrap().push(prompt);
        let response = LlmResponse::new(Content::new("model").with_text("summary"));
        Ok(Box::pin(futures::stream::once(async { Ok(response) })))
    }
}

async fn summarizer_prompt(events: &[Event]) -> String {
    let llm = Arc::new(RecordingSummarizerLlm { prompts: Mutex::new(Vec::new()) });
    LlmEventSummarizer::new(llm.clone()).summarize_events(events).await.unwrap();
    llm.prompts.lock().unwrap().pop().unwrap()
}

fn event_with_parts(author: &str, role: &str, parts: Vec<Part>) -> Event {
    let mut event = Event::new("inv-test");
    event.author = author.to_string();
    event.set_content(Content { role: role.to_string(), parts });
    event
}

#[tokio::test]
async fn function_calls_and_responses_reach_the_summarizer() {
    let call = event_with_parts(
        "assistant",
        "model",
        vec![Part::FunctionCall {
            name: "get_weather".to_string(),
            args: json!({"city": "NYC"}),
            id: Some("call_1".to_string()),
            thought_signature: None,
        }],
    );
    let response = event_with_parts(
        "assistant",
        "function",
        vec![Part::FunctionResponse {
            function_response: FunctionResponseData::new("get_weather", json!({"temp_f": 72})),
            id: Some("call_1".to_string()),
            annotations: None,
        }],
    );
    let events = vec![make_event("user", "Check weather"), call, response];

    let prompt = summarizer_prompt(&events).await;

    assert!(prompt.ends_with(
        "user: Check weather\n\
         assistant: [function call] get_weather({\"city\":\"NYC\"})\n\
         assistant: [function response] get_weather: {\"temp_f\":72}"
    ));
}

#[tokio::test]
async fn an_earlier_summary_replaces_the_events_it_covers() {
    let base = Utc::now() - Duration::minutes(10);
    let at = |mut event: Event, minutes: i64| {
        event.timestamp = base + Duration::minutes(minutes);
        event
    };
    let mut earlier = at(Event::new("compaction"), 3);
    earlier.author = "system".to_string();
    earlier.actions.compaction = Some(EventCompaction {
        start_timestamp: base,
        end_timestamp: base + Duration::minutes(2),
        compacted_content: Content::new("model").with_text("The user's vault code is FACT-A."),
    });
    let events = vec![
        at(make_event("assistant", "covered by the earlier summary"), 2),
        earlier,
        at(make_event("user", "Second question"), 4),
        at(make_event("assistant", "Second answer"), 5),
    ];

    let llm = Arc::new(RecordingSummarizerLlm { prompts: Mutex::new(Vec::new()) });
    let summary =
        LlmEventSummarizer::new(llm.clone()).summarize_events(&events).await.unwrap().unwrap();
    let prompt = llm.prompts.lock().unwrap().pop().unwrap();

    assert!(prompt.ends_with(
        "summary of earlier conversation: The user's vault code is FACT-A.\n\
         user: Second question\n\
         assistant: Second answer"
    ));
    let compaction = summary.actions.compaction.unwrap();
    assert_eq!(
        (compaction.start_timestamp, compaction.end_timestamp),
        (base, base + Duration::minutes(5))
    );
}

#[tokio::test]
async fn thinking_and_streamed_partials_stay_out_of_the_summarizer() {
    let thinking = event_with_parts(
        "assistant",
        "model",
        vec![
            Part::Thinking { thinking: "private reasoning".to_string(), signature: None },
            Part::Text { text: "Final answer".to_string() },
        ],
    );
    let mut partial = make_event("assistant", "Final");
    partial.llm_response.partial = true;
    let events = vec![make_event("user", "Question"), partial, thinking];

    let prompt = summarizer_prompt(&events).await;

    assert!(prompt.ends_with("user: Question\nassistant: Final answer"));
}

/// The agent and summarizer model of the runner-level test: it calls `lookup_fact` when asked
/// to look something up, and its summaries list every `FACT-` token in their input.
struct ScriptedAgentLlm {
    requests: Mutex<Vec<LlmRequest>>,
}

const SUMMARY_MARKER: &str = "The following is a conversation history";

#[async_trait]
impl Llm for ScriptedAgentLlm {
    fn name(&self) -> &str {
        "scripted-agent"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(req.clone());
        let last = req.contents.last().cloned().unwrap_or_else(|| Content::new("user"));
        let last_text: String = last.parts.iter().filter_map(Part::text).collect();
        let content = if last_text.contains(SUMMARY_MARKER) {
            let mut facts: Vec<&str> = Vec::new();
            for word in last_text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
                if word.starts_with("FACT-") && !facts.contains(&word) {
                    facts.push(word);
                }
            }
            Content::new("model").with_text(format!("Facts: [{}]", facts.join(", ")))
        } else if last.parts.iter().any(|p| matches!(p, Part::FunctionResponse { .. })) {
            Content::new("model").with_text("noted")
        } else if last_text.contains("look up") {
            Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "lookup_fact".to_string(),
                    args: json!({}),
                    id: Some(format!("call_{}", req.contents.len())),
                    thought_signature: None,
                }],
            }
        } else {
            Content::new("model").with_text("ok")
        };
        let response = LlmResponse::new(content);
        Ok(Box::pin(futures::stream::once(async { Ok(response) })))
    }
}

/// With a history window, an older turn reaches the model only through the summaries, so each
/// summary must carry the one before it and the tool results it covered.
#[tokio::test]
async fn compaction_with_a_history_window_keeps_earlier_summaries_and_tool_results() {
    let llm = Arc::new(ScriptedAgentLlm { requests: Mutex::new(Vec::new()) });
    let lookup = FunctionTool::new(
        "lookup_fact",
        "Looks up the stored fact.",
        |_ctx: Arc<dyn ToolContext>, _args: Value| async move { Ok(json!({"fact": "FACT-TOOL-7731"})) },
    );
    let agent = LlmAgentBuilder::new("assistant")
        .model(llm.clone())
        .tool(Arc::new(lookup))
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "app".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("app")
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .run_config(
            RunConfig::builder()
                .streaming_mode(StreamingMode::None)
                .history_max_events(Some(4))
                .build(),
        )
        .compaction_config(EventsCompactionConfig {
            compaction_interval: 1,
            overlap_size: 1,
            summarizer: Arc::new(LlmEventSummarizer::new(llm.clone())),
        })
        .build()
        .unwrap();

    for text in ["Remember FACT-A, then look up the fact.", "Second.", "Third.", "Fourth."] {
        let mut stream =
            runner.run_str("user", "session", Content::new("user").with_text(text)).await.unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }

    let requests = llm.requests.lock().unwrap().clone();
    let is_summary = |req: &LlmRequest| {
        req.contents
            .iter()
            .flat_map(|c| c.parts.iter())
            .filter_map(Part::text)
            .any(|t| t.contains(SUMMARY_MARKER))
    };
    assert_eq!(requests.iter().filter(|req| is_summary(req)).count(), 3);
    let last_turn = requests.iter().rev().find(|req| !is_summary(req)).unwrap();
    let history = serde_json::to_string(&last_turn.contents).unwrap();
    assert!(!history.contains("Remember FACT-A"), "turn 1 is outside the window: {history}");
    assert!(history.contains("Facts: [FACT-A, FACT-TOOL-7731]"), "{history}");
}

fn window() -> Vec<Event> {
    vec![
        make_event("user", "Where is order A1?"),
        make_event("desk", "Order A1 ships Monday from the Nairobi warehouse."),
    ]
}

#[tokio::test]
async fn a_summary_not_smaller_than_its_window_is_not_used() {
    let llm = Arc::new(MockSummarizerLlm { summary_text: "A very detailed summary. ".repeat(20) });
    let result = LlmEventSummarizer::new(llm).summarize_events(&window()).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn a_summary_cut_short_fails_instead_of_replacing_history() {
    let mut truncated = LlmResponse::new(Content::new("model").with_text("The customer"));
    truncated.finish_reason = Some(adk_core::FinishReason::MaxTokens);
    let llm = Arc::new(adk_model::ScriptedLlm::new("summarizer", [truncated]));
    let error = LlmEventSummarizer::new(llm).summarize_events(&window()).await.unwrap_err();
    assert_eq!(error.code, "model.summary_incomplete");
}

#[tokio::test]
async fn a_stream_error_fails_the_summary() {
    let llm = Arc::new(adk_model::ScriptedLlm::new(
        "summarizer",
        [adk_model::ScriptedResponse::text("Summary.")
            .then_error(adk_core::AdkError::model("stream reset"))],
    ));
    let error = LlmEventSummarizer::new(llm).summarize_events(&window()).await.unwrap_err();
    assert_eq!(error.code, "model.summary_incomplete");
    assert_eq!(error.category, adk_core::ErrorCategory::Internal);
    assert_eq!(error.details.metadata["cause_code"], "model.legacy");
    assert!(error.message.contains("stream reset"), "{}", error.message);
    let cause = std::error::Error::source(&error).expect("the stream error is the source");
    assert!(cause.to_string().contains("stream reset"));
}

#[tokio::test]
async fn a_retryable_stream_error_stays_retryable() {
    let cause = adk_core::AdkError::new(
        adk_core::ErrorComponent::Model,
        adk_core::ErrorCategory::RateLimited,
        "model.openai.rate_limited",
        "rate limited",
    )
    .with_provider("openai")
    .with_upstream_status(429);
    let llm = Arc::new(adk_model::ScriptedLlm::new(
        "summarizer",
        [adk_model::ScriptedResponse::text("Summary.").then_error(cause)],
    ));
    let error = LlmEventSummarizer::new(llm).summarize_events(&window()).await.unwrap_err();
    assert_eq!(error.code, "model.summary_incomplete");
    assert!(error.is_retryable());
    assert_eq!(error.details.provider.as_deref(), Some("openai"));
    assert_eq!(error.details.upstream_status_code, Some(429));
    assert_eq!(error.details.metadata["cause_code"], "model.openai.rate_limited");
}

#[tokio::test]
async fn owned_provider_data_stays_private() {
    let mut event = Event::new("inv");
    event.author = "assistant".into();
    event.set_content(Content { role: "model".into(), parts: vec![
            Part::Text { text: "Visible answer".into() },
            Part::ServerToolResponse { server_tool_response: serde_json::json!({
                "type":"anthropic_message", "content":[{"type":"thinking", "thinking":"private-replay", "signature":"private-signature"}]
            }) },
        ] });
    event.llm_response.provider_metadata = Some(serde_json::json!({"provider_origin":{
        "provider":"anthropic", "model":"model", "scope":"owner"
    }}));
    let source = serde_json::to_vec(&event).unwrap();
    let prompt = summarizer_prompt(std::slice::from_ref(&event)).await;
    assert!(prompt.contains("Visible answer"));
    for forbidden in ["private-replay", "private-signature", "__adk_provider_replay"] {
        assert!(!prompt.contains(forbidden), "{forbidden}: {prompt}");
    }
    assert_eq!(serde_json::to_vec(&event).unwrap(), source);
}
