//! A refusal or a malformed tool call ends an `LlmAgent` run with an error, whichever provider
//! reports it and whether or not it sets `error_code`: the run reaches the real
//! OpenAI-compatible, Anthropic and Gemini adapters over wiremock, and a scripted model for
//! the shapes no adapter sends yet.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AdkError, Agent, Content, ErrorCategory, Event, FinishReason, Llm, LlmResponse, Part,
    SessionId, UserId,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// What one run yielded: its complete events, the error that ended it, and what the
/// session kept.
struct Outcome {
    events: Vec<Event>,
    error: Option<AdkError>,
    stored: Vec<Event>,
}

async fn run(model: Arc<dyn Llm>, tool_runs: Option<Arc<AtomicUsize>>) -> Outcome {
    let mut agent = LlmAgentBuilder::new("assistant").model(model);
    if let Some(runs) = tool_runs {
        agent = agent.tool(Arc::new(FunctionTool::new(
            "lookup",
            "Looks something up.",
            move |_ctx, _args| {
                let runs = runs.clone();
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({"found": true}))
                }
            },
        )));
    }
    let agent = agent.build().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "refusals".into(),
            user_id: "user".into(),
            session_id: Some("session".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("refusals")
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(sessions.clone())
        .build()
        .unwrap();
    let mut stream = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("Help me with this."),
        )
        .await
        .unwrap();
    let mut events = Vec::new();
    let mut error = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) if !event.llm_response.partial => events.push(event),
            Ok(_) => {}
            Err(failure) => error = Some(failure),
        }
    }
    let session = sessions
        .get(GetRequest {
            app_name: "refusals".into(),
            user_id: "user".into(),
            session_id: "session".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    let stored = session.events().all();
    Outcome { events, error, stored }
}

fn text(event: &Event) -> String {
    event
        .llm_response
        .content
        .iter()
        .flat_map(|content| &content.parts)
        .filter_map(Part::text)
        .collect()
}

/// The model's last event: its text and finish reason.
fn last_model_turn(events: &[Event]) -> (String, Option<FinishReason>) {
    let event =
        events.iter().rev().find(|event| event.author == "assistant").expect("a model event");
    (text(event), event.llm_response.finish_reason)
}

fn sse(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

async fn server(reply: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(reply).mount(&server).await;
    server
}

#[tokio::test]
async fn an_openai_chat_refusal_fails_the_run_and_keeps_the_refusal_in_the_session() {
    let chunk = |delta: Value, finish: Option<&str>| {
        json!({"id": "c", "object": "chat.completion.chunk", "created": 1, "model": "m",
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let body: String = [
        chunk(
            json!({"role": "assistant", "content": null, "refusal": "I can't help with that."}),
            None,
        ),
        chunk(json!({}), Some("stop")),
    ]
    .iter()
    .map(|event| format!("data: {event}\n\n"))
    .chain(["data: [DONE]\n\n".to_string()])
    .collect();
    let upstream = server(sse(body)).await;
    let model =
        adk_model::openai::OpenAIClient::compatible("sk", format!("{}/v1", upstream.uri()), "m")
            .unwrap();

    let outcome = run(Arc::new(model), None).await;

    let error = outcome.error.expect("a refusal fails the run");
    assert_eq!((error.code, error.category), ("model.refusal", ErrorCategory::Forbidden));
    assert!(error.message.contains("I can't help with that."), "{}", error.message);
    let refusal = ("I can't help with that.".to_string(), Some(FinishReason::Refusal));
    assert_eq!(last_model_turn(&outcome.events), refusal);
    assert_eq!(last_model_turn(&outcome.stored), refusal);
}

#[tokio::test]
async fn an_anthropic_refusal_fails_the_run() {
    let event = |name: &str, data: Value| format!("event: {name}\ndata: {data}\n\n");
    let message = json!({"id": "msg", "type": "message", "role": "assistant",
        "model": "claude-sonnet-5-5", "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": {"input_tokens": 3, "output_tokens": 1}});
    let body = [
        event("message_start", json!({"type": "message_start", "message": message})),
        event(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        ),
        event(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Partial answer"}}),
        ),
        event("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
        event(
            "message_delta",
            json!({"type": "message_delta",
            "delta": {"stop_reason": "refusal", "stop_sequence": null},
            "usage": {"output_tokens": 2}}),
        ),
        event("message_stop", json!({"type": "message_stop"})),
    ]
    .concat();
    let upstream = server(sse(body)).await;
    let model = adk_model::anthropic::AnthropicClient::new(
        adk_model::anthropic::AnthropicConfig::new("k", "claude-sonnet-5-5")
            .with_base_url(upstream.uri()),
    )
    .unwrap()
    .with_retry_config(adk_model::retry::RetryConfig::disabled());

    let outcome = run(Arc::new(model), None).await;

    assert_eq!(outcome.error.expect("a refusal fails the run").code, "model.refusal");
    assert_eq!(
        last_model_turn(&outcome.events),
        ("Partial answer".to_string(), Some(FinishReason::Refusal))
    );
}

#[tokio::test]
async fn a_gemini_malformed_function_call_fails_the_run() {
    let chunk = json!({"candidates": [{"index": 0,
        "content": {"role": "model", "parts": [{"text": "Calling the tool."}]},
        "finishReason": "MALFORMED_FUNCTION_CALL"}], "modelVersion": "gemini-3.7-flash"});
    let upstream = server(sse(format!("data: {chunk}\r\n\r\n"))).await;
    let model = adk_model::GeminiModel::new_with_base_url(
        "k",
        "gemini-3.7-flash",
        format!("{}/v1beta/", upstream.uri()),
    )
    .unwrap()
    .with_retry_config(adk_model::retry::RetryConfig::disabled());

    let outcome = run(Arc::new(model), None).await;

    let error = outcome.error.expect("a malformed tool call fails the run");
    assert_eq!(error.code, "model.malformed_tool_call");
    assert_eq!(error.details.metadata["finish_reason"], json!("MalformedToolCall"));
}

/// The shape a provider that reports the refusal as an error sends: `error_code` and the
/// provider's own explanation beside `FinishReason::Refusal`.
#[tokio::test]
async fn a_refusal_with_an_error_code_fails_with_model_refusal_and_the_provider_message() {
    let mut refused = LlmResponse::new(Content::new("model").with_text("Partial"));
    refused.finish_reason = Some(FinishReason::Refusal);
    refused.error_code = Some("refusal".into());
    refused.error_message = Some("Claude declined the request (category cyber): Declined.".into());
    let model = ScriptedLlm::new("scripted", [ScriptedResponse::response(refused)]);

    let outcome = run(Arc::new(model), None).await;

    let error = outcome.error.expect("a refusal fails the run");
    assert_eq!(error.code, "model.refusal");
    assert!(error.message.contains("(category cyber): Declined."), "{}", error.message);
    assert_eq!(error.details.metadata["provider_error_code"], json!("refusal"));
}

#[tokio::test]
async fn no_tool_call_of_a_refused_turn_runs() {
    let mut refused = LlmResponse::new(Content {
        role: "model".into(),
        parts: vec![
            Part::Text { text: "I won't do that.".into() },
            Part::FunctionCall {
                name: "lookup".into(),
                args: json!({}),
                id: Some("call_1".into()),
                thought_signature: None,
            },
        ],
    });
    refused.finish_reason = Some(FinishReason::Refusal);
    let model = ScriptedLlm::new("scripted", [ScriptedResponse::response(refused)]);
    let runs = Arc::new(AtomicUsize::new(0));

    let outcome = run(Arc::new(model), Some(runs.clone())).await;

    assert_eq!(outcome.error.expect("a refusal fails the run").code, "model.refusal");
    assert_eq!(runs.load(Ordering::SeqCst), 0, "a tool call of a refused turn ran");
}

#[tokio::test]
async fn other_finish_reasons_still_end_the_run_normally() {
    for reason in [FinishReason::Stop, FinishReason::Safety, FinishReason::Other] {
        let mut answer = LlmResponse::new(Content::new("model").with_text("Paris."));
        answer.finish_reason = Some(reason);
        let model = ScriptedLlm::new("scripted", [ScriptedResponse::response(answer)]);

        let outcome = run(Arc::new(model), None).await;

        assert!(outcome.error.is_none(), "{reason:?}: {:?}", outcome.error);
        assert_eq!(last_model_turn(&outcome.events), ("Paris.".to_string(), Some(reason)));
    }
}
