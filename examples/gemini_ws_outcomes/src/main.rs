//! Gemini streams and the OpenAI Responses WebSocket transport report errors, blocks, and
//! truncation instead of a silent success.
//!
//! **Gemini.** Every scripted Gemini agent scenario drives the stack a user runs —
//! `LlmAgentBuilder`, `Runner`, an in-memory session service, and `GeminiModel` pointed at
//! a local upstream on 127.0.0.1 through its base URL — in both `StreamingMode::SSE` and
//! `StreamingMode::None`. `LlmAgent` always streams from the model, so both modes reach
//! `streamGenerateContent`; a direct `generate_content(request, false)` call, as a graph
//! node or custom code makes one, covers `generateContent`. The scenarios assert that an
//! error object inside a successful response fails the turn, a stream that ends before any
//! chunk carries `finishReason` fails the turn, a prompt blocked through
//! `promptFeedback.blockReason` ends as a `Safety` turn whose error code names the block
//! reason (so the agent run fails), and every block-type finish reason is reported as
//! `Safety`. The Interactions transport (`use_interactions_api`) must see
//! `interaction.completed` and fails on a `failed` status.
//!
//! **Responses WebSocket.** `WsTransport` is used directly, as a caller of the transport
//! does, against a scripted WebSocket server. The scenarios assert that an abandoned
//! response never leaks into the next request, that `response.incomplete`, `failed`,
//! `error`, an early Close, and an unparseable completed response each end the stream with
//! the right outcome instead of hanging or succeeding, that an answer is streamed once,
//! and that handshakes are retried for transient failures only.
//!
//! Live scenarios run against Gemini and the OpenAI Responses WebSocket endpoint when keys
//! are set. Keys come from this crate's `.env` or the repository `.env`, read explicitly so
//! stale shell variables do not win. Set `SKIP_LIVE=1` to skip them.

mod http_upstream;
mod ws_upstream;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AdkError, Content, ErrorCategory, FinishReason, Llm, LlmRequest, LlmResponse, Part, RunConfig,
    SessionId, StreamingMode, UserId,
};
use adk_model::GeminiModel;
use adk_model::openai::{OpenAIResponsesConfig, WsTransport};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_openai::types::responses::{CreateResponse, CreateResponseArgs, InputParam};
use futures::StreamExt;
use http_upstream::{Reply, Upstream};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use ws_upstream::{Step, WsUpstream};

const APP: &str = "gemini-ws-outcomes";
const AGENT: &str = "assistant";
const USER: &str = "user";
const SESSION: &str = "session";
const ANSWER: &str = "The capital of France is Paris.";
const SCRIPTED_MODEL: &str = "gemini-3.7-flash";

// ---------------------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------------------

#[derive(Default)]
struct Report {
    rows: Vec<(String, bool)>,
}

impl Report {
    fn record(&mut self, name: impl Into<String>, result: Result<String, String>) {
        let name = name.into();
        match &result {
            Ok(note) if note.is_empty() => println!("PASS  {name}"),
            Ok(note) => println!("PASS  {name}\n      {note}"),
            Err(reason) => println!("FAIL  {name}\n      {reason}"),
        }
        self.rows.push((name, result.is_ok()));
    }

    fn failures(&self) -> usize {
        self.rows.iter().filter(|(_, passed)| !passed).count()
    }
}

fn error_summary(error: &AdkError) -> String {
    let mut message = error.message.clone();
    if message.len() > 120 {
        let mut end = 120;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push('…');
    }
    format!("{} [{:?}, retryable={}]: {message}", error.code, error.category, error.is_retryable())
}

fn text_of(response: &LlmResponse) -> String {
    response.content.iter().flat_map(|content| &content.parts).filter_map(Part::text).collect()
}

fn mode_label(mode: StreamingMode) -> &'static str {
    match mode {
        StreamingMode::None => "none",
        StreamingMode::SSE => "sse",
        StreamingMode::Bidi => "bidi",
    }
}

const MODES: [StreamingMode; 2] = [StreamingMode::SSE, StreamingMode::None];

// ---------------------------------------------------------------------------------------
// Agent turns
// ---------------------------------------------------------------------------------------

/// One non-partial model event the run persisted.
#[derive(Debug)]
struct Persisted {
    text: String,
    finish_reason: Option<FinishReason>,
    error_code: Option<String>,
}

/// What one run left behind.
#[derive(Debug, Default)]
struct Outcome {
    error: Option<String>,
    persisted: Vec<Persisted>,
    /// `state["answer"]`, written by the agent's `output_key`.
    answer: Option<Value>,
    /// Text of the partial events the run streamed to the caller.
    streamed: String,
}

impl Outcome {
    fn answer_text(&self) -> Option<&str> {
        self.persisted.iter().rev().map(|event| event.text.as_str()).find(|text| !text.is_empty())
    }

    fn summary(&self) -> String {
        let persisted: Vec<_> = self
            .persisted
            .iter()
            .map(|event| {
                format!("{:?}/{:?}/{:?}", event.text, event.finish_reason, event.error_code)
            })
            .collect();
        match &self.error {
            Some(error) => format!("run failed: {error}; persisted {persisted:?}"),
            None => format!("run ok; persisted {persisted:?}"),
        }
    }
}

async fn run_turn(
    model: Arc<dyn Llm>,
    mode: StreamingMode,
    prompt: &str,
) -> anyhow::Result<Outcome> {
    let agent = Arc::new(
        LlmAgentBuilder::new(AGENT)
            .instruction("Answer briefly.")
            .model(model)
            .output_key("answer")
            .build()?,
    );
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: HashMap::new(),
        })
        .await?;
    let runner =
        Runner::builder().app_name(APP).agent(agent).session_service(sessions.clone()).build()?;

    let run_config = RunConfig { streaming_mode: mode, ..RunConfig::default() };
    let mut outcome = Outcome::default();
    let run = runner.run_with_config(
        UserId::new(USER)?,
        SessionId::new(SESSION)?,
        Content::new("user").with_text(prompt),
        Some(run_config),
    );
    match tokio::time::timeout(Duration::from_secs(60), async {
        match run.await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(event) if event.llm_response.partial => {
                            outcome.streamed.push_str(&text_of(&event.llm_response));
                        }
                        Ok(_) => {}
                        Err(error) => {
                            outcome.error = Some(error_summary(&error));
                            break;
                        }
                    }
                }
            }
            Err(error) => outcome.error = Some(error_summary(&error)),
        }
    })
    .await
    {
        Ok(()) => {}
        Err(_) => outcome.error = Some("the run did not finish within 60 seconds".into()),
    }

    let session = sessions
        .get(GetRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: SESSION.into(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    outcome.persisted = session
        .events()
        .all()
        .into_iter()
        .filter(|event| event.author == AGENT && !event.llm_response.partial)
        .map(|event| Persisted {
            text: text_of(&event.llm_response),
            finish_reason: event.llm_response.finish_reason,
            error_code: event.llm_response.error_code.clone(),
        })
        .collect();
    outcome.answer = session.state().get("answer");
    Ok(outcome)
}

/// What a scripted Gemini scenario requires of the run.
#[derive(Clone, Copy, Debug)]
enum Expect {
    /// The run succeeds and persists exactly this answer.
    Answer(&'static str),
    /// The run fails and persists no answer or `output_key` state.
    Fails,
    /// The run fails, persists a `Safety` turn whose error code names the block reason,
    /// and persists no answer or `output_key` state.
    Blocked(&'static str),
    /// The persisted turn ends with `FinishReason::Safety`.
    SafetyFinish,
}

fn verdict(expect: Expect, outcome: &Outcome) -> Result<String, String> {
    let no_answer = || -> Result<(), String> {
        if let Some(text) = outcome.answer_text() {
            return Err(format!("a failed turn persisted the answer {text:?}"));
        }
        if let Some(answer) = &outcome.answer {
            return Err(format!("a failed turn wrote output_key state {answer}"));
        }
        Ok(())
    };
    match expect {
        Expect::Answer(expected) => {
            if outcome.error.is_some() || outcome.answer_text() != Some(expected) {
                return Err(format!("expected {expected:?}; {}", outcome.summary()));
            }
            if outcome.answer.as_ref().and_then(Value::as_str) != Some(expected) {
                return Err(format!("expected output_key {expected:?}, got {:?}", outcome.answer));
            }
            if !expected.starts_with(&outcome.streamed) {
                return Err(format!("streamed {:?} but persisted {expected:?}", outcome.streamed));
            }
            Ok(String::new())
        }
        Expect::Fails => {
            let Some(error) = &outcome.error else {
                return Err(format!("expected the run to fail; {}", outcome.summary()));
            };
            no_answer()?;
            Ok(error.clone())
        }
        Expect::Blocked(reason) => {
            let Some(error) = &outcome.error else {
                return Err(format!(
                    "expected the blocked prompt to fail the run; {}",
                    outcome.summary()
                ));
            };
            no_answer()?;
            let blocked = outcome.persisted.iter().any(|event| {
                event.finish_reason == Some(FinishReason::Safety)
                    && event.error_code.as_deref().is_some_and(|code| code.contains(reason))
            });
            if !blocked {
                return Err(format!(
                    "expected a persisted Safety turn naming {reason}; {}",
                    outcome.summary()
                ));
            }
            Ok(error.clone())
        }
        Expect::SafetyFinish => match outcome.persisted.last() {
            Some(event) if event.finish_reason == Some(FinishReason::Safety) => Ok(String::new()),
            _ => Err(format!("expected a persisted Safety finish; {}", outcome.summary())),
        },
    }
}

// ---------------------------------------------------------------------------------------
// Gemini generateContent scenarios
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Fault {
    Complete,
    /// An `{"error": …}` object arrives inside the successful response.
    ErrorObject,
    /// The stream ends before any chunk carries `finishReason`; the blocking body is cut.
    CutBeforeFinish,
    /// The prompt is blocked through `promptFeedback.blockReason`.
    PromptBlocked,
    /// The candidate ends with this block-type finish reason.
    FinishBlocked(&'static str),
}

impl Fault {
    fn label(self) -> String {
        match self {
            Self::Complete => "complete".into(),
            Self::ErrorObject => "error-object".into(),
            Self::CutBeforeFinish => "cut-before-finish-reason".into(),
            Self::PromptBlocked => "prompt-blocked".into(),
            Self::FinishBlocked(reason) => format!("finish-{}", reason.to_lowercase()),
        }
    }
}

fn candidate(text: Option<&str>, finish: Option<&str>) -> Value {
    let mut candidate = json!({"index": 0});
    if let Some(text) = text {
        candidate["content"] = json!({"role": "model", "parts": [{"text": text}]});
    }
    if let Some(finish) = finish {
        candidate["finishReason"] = json!(finish);
    }
    json!({
        "candidates": [candidate],
        "usageMetadata": {"promptTokenCount": 8, "candidatesTokenCount": 6, "totalTokenCount": 14},
        "modelVersion": SCRIPTED_MODEL
    })
}

fn overloaded() -> Value {
    json!({"error": {"code": 503, "message": "The model is overloaded. Please try again later.",
        "status": "UNAVAILABLE"}})
}

fn prompt_blocked() -> Value {
    json!({
        "promptFeedback": {"blockReason": "PROHIBITED_CONTENT"},
        "usageMetadata": {"promptTokenCount": 8, "totalTokenCount": 8},
        "modelVersion": SCRIPTED_MODEL
    })
}

fn gemini_reply(fault: Fault, streaming: bool) -> Reply {
    match (fault, streaming) {
        (Fault::Complete, true) => Reply::sse(&[
            candidate(Some("The capital of France "), None),
            candidate(Some("is Paris."), Some("STOP")),
        ]),
        (Fault::Complete, false) => Reply::json(&candidate(Some(ANSWER), Some("STOP"))),
        (Fault::ErrorObject, true) => {
            Reply::sse(&[candidate(Some("The capital of France "), None), overloaded()])
        }
        (Fault::ErrorObject, false) => Reply::json(&overloaded()),
        (Fault::CutBeforeFinish, true) => Reply::sse(&[
            candidate(Some("The capital of France "), None),
            candidate(Some("is Par"), None),
        ]),
        (Fault::CutBeforeFinish, false) => Reply::cut_json(
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"The capital of France is Par"#,
        ),
        (Fault::PromptBlocked, true) => Reply::sse(&[prompt_blocked()]),
        (Fault::PromptBlocked, false) => Reply::json(&prompt_blocked()),
        (Fault::FinishBlocked(reason), true) => Reply::sse(&[candidate(None, Some(reason))]),
        (Fault::FinishBlocked(reason), false) => Reply::json(&candidate(None, Some(reason))),
    }
}

async fn run_gemini_scripted(report: &mut Report) -> anyhow::Result<()> {
    let mut scenarios = vec![
        (Fault::Complete, Expect::Answer(ANSWER)),
        (Fault::ErrorObject, Expect::Fails),
        (Fault::CutBeforeFinish, Expect::Fails),
        (Fault::PromptBlocked, Expect::Blocked("PROHIBITED_CONTENT")),
    ];
    for reason in ["BLOCKLIST", "PROHIBITED_CONTENT", "SPII", "IMAGE_SAFETY", "MODEL_ARMOR"] {
        scenarios.push((Fault::FinishBlocked(reason), Expect::SafetyFinish));
    }
    for (fault, expect) in scenarios {
        for mode in MODES {
            let script: http_upstream::Script = Arc::new(move |path: &str, _body: &str| {
                gemini_reply(fault, path.contains("streamGenerateContent"))
            });
            let upstream = Upstream::start(script).await?;
            let model: Arc<dyn Llm> = Arc::new(GeminiModel::new_with_base_url(
                "scripted-key",
                SCRIPTED_MODEL,
                format!("{}/v1beta/", upstream.base),
            )?);
            let outcome = run_turn(model, mode, "What is the capital of France?").await?;
            let mut result = verdict(expect, &outcome);
            if upstream.requests() == 0 {
                result = Err("the client never reached the scripted upstream".into());
            }
            report.record(format!("gemini agent/{}/{}", fault.label(), mode_label(mode)), result);
        }
        run_gemini_direct(report, fault, expect).await?;
    }
    Ok(())
}

/// A direct non-streaming `generateContent` call, as a graph node or custom code makes
/// one. `LlmAgent` always streams from the model, so this is the only caller of the
/// non-streaming path.
async fn run_gemini_direct(
    report: &mut Report,
    fault: Fault,
    expect: Expect,
) -> anyhow::Result<()> {
    let script: http_upstream::Script = Arc::new(move |path: &str, _body: &str| {
        gemini_reply(fault, path.contains("streamGenerateContent"))
    });
    let upstream = Upstream::start(script).await?;
    let model = GeminiModel::new_with_base_url(
        "scripted-key",
        SCRIPTED_MODEL,
        format!("{}/v1beta/", upstream.base),
    )?;
    let request = LlmRequest::new(
        SCRIPTED_MODEL,
        vec![Content::new("user").with_text("What is the capital of France?")],
    );
    let mut responses = Vec::new();
    let mut failure = None;
    match model.generate_content(request, false).await {
        Ok(mut stream) => {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(response) => responses.push(response),
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
        }
        Err(error) => failure = Some(error),
    }
    let last = responses.last();
    let summary = format!(
        "{}; {} response(s), text {:?}, finish {:?}, error_code {:?}",
        failure.as_ref().map_or_else(|| "ok".to_string(), error_summary),
        responses.len(),
        last.map(text_of),
        last.and_then(|response| response.finish_reason),
        last.and_then(|response| response.error_code.clone()),
    );
    let result = match (expect, &failure, last) {
        (Expect::Answer(text), None, Some(response))
            if text_of(response) == text && response.finish_reason == Some(FinishReason::Stop) =>
        {
            Ok(String::new())
        }
        (Expect::Fails, Some(error), _) => Ok(error_summary(error)),
        // A direct caller reads a blocked prompt from `error_code`, the adk failure marker.
        (Expect::Blocked(reason), None, Some(response))
            if response.finish_reason == Some(FinishReason::Safety)
                && response.error_code.as_deref().is_some_and(|code| code.contains(reason)) =>
        {
            Ok(String::new())
        }
        (Expect::Blocked(_), Some(error), _) => Ok(error_summary(error)),
        (Expect::SafetyFinish, None, Some(response))
            if response.finish_reason == Some(FinishReason::Safety) =>
        {
            Ok(String::new())
        }
        _ => Err(format!("expected {expect:?}; {summary}")),
    };
    report.record(format!("gemini direct/{}/generate-content", fault.label()), result);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Gemini Interactions streaming scenarios
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum InteractionFault {
    Complete,
    /// The stream ends without `interaction.completed`.
    NoCompleted,
    /// `interaction.completed` carries status `failed`.
    CompletedFailed,
}

fn interaction_reply(fault: InteractionFault) -> Reply {
    let created = json!({"event_type": "interaction.created",
        "interaction": {"id": "int_1", "status": "in_progress"}});
    let delta = |text: &str| json!({"event_type": "step.delta", "index": 0, "delta": {"type": "text", "text": text}});
    let completed = |status: &str| {
        json!({"event_type": "interaction.completed",
            "interaction": {"id": "int_1", "status": status}})
    };
    match fault {
        InteractionFault::Complete => Reply::sse(&[
            created,
            delta("The capital of France "),
            delta("is Paris."),
            completed("completed"),
        ]),
        InteractionFault::NoCompleted => {
            Reply::sse(&[created, delta("The capital of France "), delta("is Par")])
        }
        InteractionFault::CompletedFailed => {
            Reply::sse(&[created, delta("The capital of France "), completed("failed")])
        }
    }
}

async fn run_interactions_scripted(report: &mut Report) -> anyhow::Result<()> {
    for (fault, label, expect) in [
        (InteractionFault::Complete, "complete", Expect::Answer(ANSWER)),
        (InteractionFault::NoCompleted, "no-completed-event", Expect::Fails),
        (InteractionFault::CompletedFailed, "completed-failed", Expect::Fails),
    ] {
        let script: http_upstream::Script =
            Arc::new(move |_path: &str, _body: &str| interaction_reply(fault));
        let upstream = Upstream::start(script).await?;
        let model: Arc<dyn Llm> = Arc::new(
            GeminiModel::new_with_base_url(
                "scripted-key",
                SCRIPTED_MODEL,
                format!("{}/v1beta/", upstream.base),
            )?
            .use_interactions_api(true)?,
        );
        let outcome = run_turn(model, StreamingMode::SSE, "What is the capital of France?").await?;
        let mut result = verdict(expect, &outcome);
        if upstream.requests() == 0 {
            result = Err("the client never reached the scripted upstream".into());
        }
        report.record(format!("gemini-interactions agent/{label}/sse"), result);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Responses WebSocket scenarios
// ---------------------------------------------------------------------------------------

fn ws_request(model: &str, text: &str) -> anyhow::Result<CreateResponse> {
    Ok(CreateResponseArgs::default().model(model).input(InputParam::Text(text.into())).build()?)
}

/// Everything one `send_request` stream produced.
#[derive(Debug, Default)]
struct WsOutcome {
    chunks: Vec<LlmResponse>,
    error: Option<AdkError>,
    hung: bool,
}

impl WsOutcome {
    fn text(&self) -> String {
        self.chunks.iter().map(text_of).collect()
    }

    fn summary(&self) -> String {
        let last = self.chunks.last();
        let state = match (&self.error, self.hung) {
            (_, true) => "hung (no terminal outcome within 3 seconds)".to_string(),
            (Some(error), false) => format!("error {}", error_summary(error)),
            (None, false) => "ok".to_string(),
        };
        format!(
            "{state}; {} chunk(s), text {:?}, last finish {:?}, last error {:?}/{:?}",
            self.chunks.len(),
            self.text(),
            last.and_then(|chunk| chunk.finish_reason),
            last.and_then(|chunk| chunk.error_code.clone()),
            last.and_then(|chunk| chunk.error_message.clone()),
        )
    }
}

async fn ws_collect(
    transport: &WsTransport,
    model: &str,
    text: &str,
    limit: Duration,
) -> WsOutcome {
    let mut outcome = WsOutcome::default();
    let request = match ws_request(model, text) {
        Ok(request) => request,
        Err(error) => {
            outcome.error = Some(AdkError::model(error.to_string()));
            return outcome;
        }
    };
    let collect = async {
        match transport.send_request(request).await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(chunk) => outcome.chunks.push(chunk),
                        Err(error) => {
                            outcome.error = Some(error);
                            break;
                        }
                    }
                }
            }
            Err(error) => outcome.error = Some(error),
        }
    };
    if tokio::time::timeout(limit, collect).await.is_err() {
        outcome.hung = true;
    }
    outcome
}

/// Sends a request, reads until the first text chunk, and drops the stream, as a caller
/// that times out or stops listening does.
async fn ws_abandon(transport: &WsTransport, model: &str, text: &str) -> anyhow::Result<String> {
    let mut stream = transport.send_request(ws_request(model, text)?).await?;
    let first = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(item) = stream.next().await {
            let chunk = item?;
            let text = text_of(&chunk);
            if !text.is_empty() {
                return Ok::<_, AdkError>(text);
            }
        }
        Ok(String::new())
    })
    .await
    .map_err(|_| anyhow::anyhow!("no first chunk within 30 seconds"))??;
    drop(stream);
    Ok(first)
}

const SCRIPT_LIMIT: Duration = Duration::from_secs(3);

fn ok_chunks(outcome: &WsOutcome) -> Result<(), String> {
    match (&outcome.error, outcome.hung) {
        (None, false) => Ok(()),
        _ => Err(outcome.summary()),
    }
}

fn expect_error(outcome: &WsOutcome, category: Option<ErrorCategory>) -> Result<String, String> {
    if outcome.hung {
        return Err(outcome.summary());
    }
    match (&outcome.error, category) {
        (Some(error), Some(expected)) if error.category != expected => {
            Err(format!("expected a {expected:?} error; {}", outcome.summary()))
        }
        (Some(error), _) => Ok(error_summary(error)),
        (None, _) => Err(format!("expected an error; {}", outcome.summary())),
    }
}

async fn ws_scenario(
    report: &mut Report,
    name: &str,
    script: ws_upstream::Script,
    check: impl FnOnce(&WsOutcome) -> Result<String, String>,
) -> anyhow::Result<()> {
    let upstream = WsUpstream::start(script, ws_upstream::accept_all()).await?;
    let config =
        OpenAIResponsesConfig::new("sk-scripted", "scripted").with_base_url(&upstream.base);
    let result = match WsTransport::connect(&config).await {
        Ok(transport) => {
            let outcome = ws_collect(&transport, "scripted", "q1", SCRIPT_LIMIT).await;
            check(&outcome)
        }
        Err(error) => Err(format!("connect failed: {}", error_summary(&error))),
    };
    report.record(format!("ws {name}"), result);
    Ok(())
}

async fn run_ws_scripted(report: &mut Report) -> anyhow::Result<()> {
    // A complete answer arrives exactly once across the deltas and the final chunk.
    ws_scenario(
        report,
        "complete-answer-streamed-once",
        Arc::new(|_, _| ws_upstream::answer("resp_1", &["The capital of France ", "is Paris."])),
        |outcome| {
            ok_chunks(outcome)?;
            let last = outcome.chunks.last();
            if outcome.text() != ANSWER {
                return Err(format!("expected the answer once; {}", outcome.summary()));
            }
            if last.is_none_or(|chunk| {
                chunk.partial || chunk.finish_reason != Some(FinishReason::Stop)
            }) {
                return Err(format!("expected a final Stop chunk; {}", outcome.summary()));
            }
            Ok(String::new())
        },
    )
    .await?;

    // The request frame is a `response.create` event, as the endpoint requires.
    {
        let upstream = WsUpstream::start(
            Arc::new(|_, _| ws_upstream::answer("resp_1", &[ANSWER])),
            ws_upstream::accept_all(),
        )
        .await?;
        let config =
            OpenAIResponsesConfig::new("sk-scripted", "scripted").with_base_url(&upstream.base);
        let result = match WsTransport::connect(&config).await {
            Ok(transport) => {
                let outcome = ws_collect(&transport, "scripted", "q1", SCRIPT_LIMIT).await;
                let frame = upstream.requests().first().cloned().unwrap_or(Value::Null);
                if frame["type"] != "response.create" || frame.get("stream").is_some() {
                    Err(format!(
                        "request frame type {} with stream {}",
                        frame["type"], frame["stream"]
                    ))
                } else {
                    ok_chunks(&outcome).map(|()| String::new())
                }
            }
            Err(error) => Err(format!("connect failed: {}", error_summary(&error))),
        };
        report.record("ws request-frame-is-response.create", result);
    }

    // A response cut short by max_output_tokens ends the stream with MaxTokens.
    ws_scenario(
        report,
        "incomplete-max-output-tokens",
        Arc::new(|_, _| {
            vec![
                Step::Send(ws_upstream::created("resp_1")),
                Step::Send(ws_upstream::delta("The capital")),
                Step::Send(ws_upstream::incomplete("resp_1", "The capital", "max_output_tokens")),
            ]
        }),
        |outcome| {
            ok_chunks(outcome)?;
            match outcome.chunks.last() {
                Some(chunk) if chunk.finish_reason == Some(FinishReason::MaxTokens) => {
                    Ok(String::new())
                }
                _ => Err(format!("expected a MaxTokens finish; {}", outcome.summary())),
            }
        },
    )
    .await?;

    // Any other incomplete reason is an error, never a hang.
    ws_scenario(
        report,
        "incomplete-other-reason",
        Arc::new(|_, _| {
            vec![
                Step::Send(ws_upstream::delta("The capital")),
                Step::Send(ws_upstream::incomplete("resp_1", "The capital", "interrupted")),
            ]
        }),
        |outcome| expect_error(outcome, None),
    )
    .await?;

    ws_scenario(
        report,
        "close-before-terminal",
        Arc::new(|_, _| vec![Step::Send(ws_upstream::delta("The capital of")), Step::Close]),
        |outcome| expect_error(outcome, Some(ErrorCategory::Unavailable)),
    )
    .await?;

    ws_scenario(
        report,
        "response-failed",
        Arc::new(|_, _| {
            vec![
                Step::Send(ws_upstream::delta("The capital")),
                Step::Send(ws_upstream::failed("resp_1")),
            ]
        }),
        |outcome| expect_error(outcome, Some(ErrorCategory::Unavailable)),
    )
    .await?;

    ws_scenario(
        report,
        "error-event-rate-limited",
        Arc::new(|_, _| vec![Step::Send(ws_upstream::rate_limited())]),
        |outcome| expect_error(outcome, Some(ErrorCategory::RateLimited)),
    )
    .await?;

    ws_scenario(
        report,
        "completed-unparseable",
        Arc::new(|_, _| {
            vec![
                Step::Send(ws_upstream::delta("The capital")),
                Step::Send(ws_upstream::malformed_completed("resp_1")),
            ]
        }),
        |outcome| expect_error(outcome, None),
    )
    .await?;

    run_ws_abandoned(report).await?;
    run_ws_idle_close(report).await?;
    run_ws_handshakes(report).await?;
    Ok(())
}

/// A caller drops the first response mid-stream; the second request must get its own
/// answer, not the rest of the first one.
async fn run_ws_abandoned(report: &mut Report) -> anyhow::Result<()> {
    let script: ws_upstream::Script = Arc::new(|_, request| {
        if ws_upstream::input_text(request).contains("q1") {
            let mut steps = vec![
                Step::Send(ws_upstream::created("resp_old")),
                Step::Send(ws_upstream::delta("OLD-")),
            ];
            steps.push(Step::Pause(300));
            steps.extend([
                Step::Send(ws_upstream::delta("ANSWER-to-q1")),
                Step::Send(ws_upstream::text_done("OLD-ANSWER-to-q1")),
                Step::Send(ws_upstream::completed("resp_old", "OLD-ANSWER-to-q1")),
            ]);
            steps
        } else {
            ws_upstream::answer("resp_new", &["NEW-ANSWER-to-q2"])
        }
    });
    let upstream = WsUpstream::start(script, ws_upstream::accept_all()).await?;
    let config =
        OpenAIResponsesConfig::new("sk-scripted", "scripted").with_base_url(&upstream.base);
    let result = async {
        let transport = WsTransport::connect(&config).await.map_err(|e| error_summary(&e))?;
        ws_abandon(&transport, "scripted", "q1").await.map_err(|e| e.to_string())?;
        let outcome = ws_collect(&transport, "scripted", "q2", SCRIPT_LIMIT).await;
        ok_chunks(&outcome)?;
        if outcome.text() != "NEW-ANSWER-to-q2" {
            return Err(format!("q2 received {:?}; {}", outcome.text(), outcome.summary()));
        }
        Ok(format!("{} handshake(s)", upstream.handshakes()))
    }
    .await;
    report.record("ws abandoned-response-does-not-leak", result);
    Ok(())
}

/// The server closes an idle connection after a completed response; the next request
/// reconnects and gets its answer instead of an empty success.
async fn run_ws_idle_close(report: &mut Report) -> anyhow::Result<()> {
    let script: ws_upstream::Script = Arc::new(|_, request| {
        let mut steps = ws_upstream::answer(
            "resp_1",
            &[&format!("answer-to-{}", ws_upstream::input_text(request))],
        );
        steps.push(Step::Close);
        steps
    });
    let upstream = WsUpstream::start(script, ws_upstream::accept_all()).await?;
    let config =
        OpenAIResponsesConfig::new("sk-scripted", "scripted").with_base_url(&upstream.base);
    let result = async {
        let transport = WsTransport::connect(&config).await.map_err(|e| error_summary(&e))?;
        let first = ws_collect(&transport, "scripted", "q1", SCRIPT_LIMIT).await;
        ok_chunks(&first)?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let second = ws_collect(&transport, "scripted", "q2", SCRIPT_LIMIT).await;
        ok_chunks(&second)?;
        if second.text() != "answer-to-q2" {
            return Err(format!("expected \"answer-to-q2\"; {}", second.summary()));
        }
        Ok(format!("{} handshake(s)", upstream.handshakes()))
    }
    .await;
    report.record("ws idle-close-then-next-request", result);
    Ok(())
}

async fn run_ws_handshakes(report: &mut Report) -> anyhow::Result<()> {
    let answer: ws_upstream::Script = Arc::new(|_, _| ws_upstream::answer("resp_1", &[ANSWER]));

    // A transient 503 on the first handshake is retried.
    let upstream =
        WsUpstream::start(answer.clone(), Arc::new(|attempt| (attempt == 0).then_some(503)))
            .await?;
    let config =
        OpenAIResponsesConfig::new("sk-scripted", "scripted").with_base_url(&upstream.base);
    let result = match WsTransport::connect(&config).await {
        Ok(transport) => {
            let outcome = ws_collect(&transport, "scripted", "q1", SCRIPT_LIMIT).await;
            ok_chunks(&outcome).and_then(|()| {
                (outcome.text() == ANSWER)
                    .then(|| format!("{} handshake(s)", upstream.handshakes()))
                    .ok_or_else(|| outcome.summary())
            })
        }
        Err(error) => Err(format!(
            "connect failed after {} handshake(s): {}",
            upstream.handshakes(),
            error_summary(&error)
        )),
    };
    report.record("ws handshake-503-is-retried", result);

    // A 401 handshake is not retried and is reported as Unauthorized.
    let upstream = WsUpstream::start(answer.clone(), Arc::new(|_| Some(401))).await?;
    let config =
        OpenAIResponsesConfig::new("sk-scripted", "scripted").with_base_url(&upstream.base);
    let result = match WsTransport::connect(&config).await {
        Ok(_) => Err("connect succeeded against a 401 handshake".into()),
        Err(error)
            if error.category == ErrorCategory::Unauthorized && upstream.handshakes() == 1 =>
        {
            Ok(error_summary(&error))
        }
        Err(error) => Err(format!(
            "expected one Unauthorized attempt; {} handshake(s), {}",
            upstream.handshakes(),
            error_summary(&error)
        )),
    };
    report.record("ws handshake-401-not-retried", result);

    // A reconnect that meets a 401 stops at once instead of retrying.
    let closing: ws_upstream::Script = Arc::new(|_, _| {
        let mut steps = ws_upstream::answer("resp_1", &[ANSWER]);
        steps.push(Step::Close);
        steps
    });
    let upstream =
        WsUpstream::start(closing, Arc::new(|attempt| (attempt > 0).then_some(401))).await?;
    let config =
        OpenAIResponsesConfig::new("sk-scripted", "scripted").with_base_url(&upstream.base);
    let result = async {
        let transport = WsTransport::connect(&config).await.map_err(|e| error_summary(&e))?;
        ok_chunks(&ws_collect(&transport, "scripted", "q1", SCRIPT_LIMIT).await)?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let second = ws_collect(&transport, "scripted", "q2", SCRIPT_LIMIT).await;
        match &second.error {
            Some(error)
                if error.category == ErrorCategory::Unauthorized && upstream.handshakes() == 2 =>
            {
                Ok(error_summary(error))
            }
            _ => Err(format!(
                "expected one Unauthorized reconnect attempt; {} handshake(s), {}",
                upstream.handshakes(),
                second.summary()
            )),
        }
    }
    .await;
    report.record("ws reconnect-401-not-retried", result);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Live scenarios
// ---------------------------------------------------------------------------------------

/// Reads `name` from the file `ENV_FILE` names, this crate's `.env`, or the repository
/// `.env` (in that order of precedence), then from the shell.
struct Keys(HashMap<String, String>);

impl Keys {
    fn load() -> Self {
        let mut keys = HashMap::new();
        let mut paths = vec![
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env").to_string(),
            concat!(env!("CARGO_MANIFEST_DIR"), "/.env").to_string(),
        ];
        paths.extend(std::env::var("ENV_FILE").ok());
        for path in paths {
            if let Ok(entries) = dotenvy::from_path_iter(path) {
                keys.extend(entries.flatten());
            }
        }
        Self(keys)
    }

    fn get(&self, name: &str) -> Option<String> {
        self.0
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
            .filter(|value| !value.trim().is_empty())
    }
}

fn check_live(outcome: &Outcome, needle: &str) -> Result<String, String> {
    if let Some(error) = &outcome.error {
        return Err(format!("run failed: {error}"));
    }
    let Some(text) = outcome.answer_text() else {
        return Err(format!("no answer persisted; {}", outcome.summary()));
    };
    if !text.to_lowercase().contains(needle) {
        return Err(format!("expected the answer to mention {needle:?}; got {text:?}"));
    }
    if !text.starts_with(&outcome.streamed) {
        return Err(format!("streamed {:?} but persisted {text:?}", outcome.streamed));
    }
    Ok(format!("{text:?}"))
}

async fn run_live(report: &mut Report, keys: &Keys) -> anyhow::Result<()> {
    if keys.get("SKIP_LIVE").is_some() {
        println!("SKIP  live scenarios (SKIP_LIVE is set)");
        return Ok(());
    }
    let capital = "What is the capital of France? Answer in one short sentence.";

    match keys.get("GEMINI_API_KEY").or_else(|| keys.get("GOOGLE_API_KEY")) {
        Some(key) => {
            let model = keys.get("GEMINI_MODEL").unwrap_or_else(|| "gemini-3.5-flash-lite".into());
            for mode in MODES {
                let llm: Arc<dyn Llm> = Arc::new(GeminiModel::new(key.clone(), model.clone())?);
                let outcome = run_turn(llm, mode, capital).await?;
                report.record(
                    format!("live gemini/{model}/answer/{}", mode_label(mode)),
                    check_live(&outcome, "paris"),
                );
            }
            let llm: Arc<dyn Llm> =
                Arc::new(GeminiModel::new(key.clone(), model.clone())?.use_interactions_api(true)?);
            let outcome = run_turn(llm, StreamingMode::SSE, capital).await?;
            report.record(
                format!("live gemini-interactions/{model}/answer/sse"),
                check_live(&outcome, "paris"),
            );
        }
        None => println!("SKIP  live gemini (GEMINI_API_KEY is not set)"),
    }

    match keys.get("OPENAI_API_KEY") {
        Some(key) => {
            let model = keys.get("OPENAI_MODEL").unwrap_or_else(|| "gpt-5.6-terra".into());
            let config = OpenAIResponsesConfig::new(key, model.clone());
            match WsTransport::connect(&config).await {
                Ok(transport) => {
                    let limit = Duration::from_secs(60);
                    let first = ws_collect(&transport, &model, capital, limit).await;
                    let result = ok_chunks(&first).and_then(|()| {
                        let text = first.text();
                        let mentions = text.to_lowercase().matches("paris").count();
                        if mentions == 1 { Ok(format!("{text:?}")) } else { Err(first.summary()) }
                    });
                    report.record(format!("live openai-ws/{model}/answer"), result);

                    let abandoned = ws_abandon(
                        &transport,
                        &model,
                        "Count from one to forty in English words, separated by commas.",
                    )
                    .await;
                    let second = ws_collect(
                        &transport,
                        &model,
                        "What is the capital of Japan? Answer with one word.",
                        limit,
                    )
                    .await;
                    let result = match abandoned {
                        Err(error) => Err(format!("the first request failed: {error}")),
                        Ok(_) => ok_chunks(&second).and_then(|()| {
                            let text = second.text().to_lowercase();
                            if text.contains("tokyo")
                                && !text.contains("forty")
                                && !text.contains("thirty")
                            {
                                Ok(format!("{:?}", second.text()))
                            } else {
                                Err(second.summary())
                            }
                        }),
                    };
                    report.record(format!("live openai-ws/{model}/abandon-then-ask"), result);
                }
                Err(error) => report
                    .record(format!("live openai-ws/{model}/connect"), Err(error_summary(&error))),
            }
        }
        None => println!("SKIP  live openai-ws (OPENAI_API_KEY is not set)"),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    adk_core::ensure_crypto_provider();
    let keys = Keys::load();
    let mut report = Report::default();
    run_gemini_scripted(&mut report).await?;
    run_interactions_scripted(&mut report).await?;
    run_ws_scripted(&mut report).await?;
    run_live(&mut report, &keys).await?;

    let failures = report.failures();
    println!(
        "\n{} scenarios, {} passed, {failures} failed",
        report.rows.len(),
        report.rows.len() - failures
    );
    if failures > 0 {
        std::process::exit(1);
    }
    Ok(())
}
