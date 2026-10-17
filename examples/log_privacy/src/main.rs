//! Error paths keep user data and secrets out of logs and traces.
//!
//! Every scenario runs the stack a user runs — `LlmAgentBuilder`, `Runner`, an in-memory
//! session service, and a real `adk-model` client — with a global tracing subscriber that
//! records every span field and event field at every level. The scripted scenarios point the
//! real OpenAI-compatible, Gemini, and Anthropic clients at a local upstream that answers with
//! errors echoing a planted secret, and plant the same secret in a tool error, a guardrail
//! denial, a callback error, and a toolset error.
//!
//! With `RunConfig::record_payloads` off (the default), each scenario asserts that:
//!
//! - the secret appears in no log line and no span field;
//! - the failure is still diagnosable: its code, category, and upstream status are logged;
//! - an error returned to the caller quotes a bounded excerpt of the upstream body.
//!
//! With `record_payloads` on, the same failures log their message text.
//!
//! One live scenario sends OpenAI an invalid `reasoning_effort` value that the API echoes in
//! its 400 response. Set `SKIP_LIVE=1` to skip it.

mod capture;
mod upstream;

use adk_agent::LlmAgentBuilder;
use adk_agent::guardrails::{PathAllowList, ToolGuardrailSet};
use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, GenerateContentConfig, Llm, LlmRequest,
    ReadonlyContext, RunConfig, Tool, Toolset,
};
use adk_model::GeminiModel;
use adk_model::RetryConfig;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use capture::Capture;
use futures::StreamExt;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;
use upstream::{Reply, Request, Script, Upstream};

/// The planted secret. No log line or span field may contain it while payloads are off.
const SECRET: &str = "acct-7731-secret";
/// Text after a long padding in each error body. A bounded error message never reaches it.
const TAIL: &str = "TAIL-OF-A-LONG-BODY";
/// Longest error message a caller may receive for a scripted upstream failure.
const MAX_ERROR_MESSAGE: usize = 1024;

fn padding() -> String {
    "x".repeat(4096)
}

// ── Upstream scripts ───────────────────────────────────────────────────

fn chat_chunk(delta: Value, finish_reason: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-scripted",
        "object": "chat.completion.chunk",
        "created": 1_760_000_000,
        "model": "scripted",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
    })
}

fn chat_text(text: &str) -> Reply {
    let mut finish = chat_chunk(json!({}), Some("stop"));
    finish["usage"] = json!({"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8});
    let events = [chat_chunk(json!({"role": "assistant", "content": text}), None), finish];
    let mut body: String = events.iter().map(|event| format!("data: {event}\n\n")).collect();
    body.push_str("data: [DONE]\n\n");
    Reply::raw_sse(body)
}

fn chat_tool_call(name: &str, arguments: &Value) -> Reply {
    let call = chat_chunk(
        json!({"role": "assistant", "tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": name, "arguments": arguments.to_string()},
        }]}),
        None,
    );
    let mut finish = chat_chunk(json!({}), Some("tool_calls"));
    finish["usage"] = json!({"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8});
    let mut body: String =
        [call, finish].iter().map(|event| format!("data: {event}\n\n")).collect();
    body.push_str("data: [DONE]\n\n");
    Reply::raw_sse(body)
}

/// Calls `name` with `arguments` once, then answers once the tool result comes back.
fn calls_tool(name: &'static str, arguments: Value) -> Script {
    Arc::new(move |request: &Request<'_>| {
        if request.body.contains(r#""role":"tool""#) {
            chat_text("Done.")
        } else {
            chat_tool_call(name, &arguments)
        }
    })
}

fn openai_400() -> Script {
    Arc::new(|_: &Request<'_>| {
        Reply::json(
            400,
            &json!({"error": {
                "message": format!("Invalid value: '{SECRET}'. {}{TAIL}", padding()),
                "type": "invalid_request_error",
                "param": "messages[0].content",
                "code": "invalid_value",
            }}),
        )
    })
}

fn openai_503_then_answer() -> Script {
    Arc::new(|request: &Request<'_>| {
        if request.index == 0 {
            Reply::json(
                503,
                &json!({"error": {
                    "message": format!("Overloaded while processing '{SECRET}'"),
                    "type": "server_error",
                }}),
            )
        } else {
            chat_text("Recovered.")
        }
    })
}

fn openai_stream_error() -> Script {
    Arc::new(|_: &Request<'_>| {
        Reply::sse(&[
            chat_chunk(json!({"role": "assistant", "content": "Partial "}), None),
            json!({"error": {
                "message": format!("generation failed for '{SECRET}' {}{TAIL}", padding()),
                "type": "server_error",
                "code": "server_error",
            }}),
        ])
    })
}

fn gemini_400() -> Script {
    Arc::new(|_: &Request<'_>| {
        Reply::json(
            400,
            &json!({"error": {
                "code": 400,
                "message": format!("Invalid value at 'contents[0]' ({SECRET}). {}{TAIL}", padding()),
                "status": "INVALID_ARGUMENT",
            }}),
        )
    })
}

fn gemini_stream_error() -> Script {
    Arc::new(|_: &Request<'_>| {
        Reply::sse(&[json!({"error": {
            "code": 400,
            "message": format!("Request contains an invalid argument: {SECRET}. {}{TAIL}", padding()),
            "status": "INVALID_ARGUMENT",
        }})])
    })
}

fn gemini_answer() -> Value {
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": format!("Account {SECRET} is active.")}]},
            "finishReason": "STOP",
            "index": 0,
        }],
        "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 5, "totalTokenCount": 10},
        "modelVersion": "gemini-3.5-flash-lite",
    })
}

fn gemini_answer_with_secret() -> Script {
    Arc::new(|_: &Request<'_>| Reply::sse(&[gemini_answer()]))
}

fn gemini_answer_json() -> Script {
    Arc::new(|_: &Request<'_>| Reply::json(200, &gemini_answer()))
}

fn anthropic_400() -> Script {
    Arc::new(|_: &Request<'_>| {
        Reply::json(
            400,
            &json!({"type": "error", "error": {
                "type": "invalid_request_error",
                "message": format!("messages.0.content: '{SECRET}' is not valid. {}{TAIL}", padding()),
            }}),
        )
    })
}

// ── Clients ────────────────────────────────────────────────────────────

fn fast_retries() -> RetryConfig {
    RetryConfig::default()
        .with_initial_delay(Duration::from_millis(10))
        .with_max_delay(Duration::from_millis(20))
}

async fn openai(script: Script) -> anyhow::Result<Arc<dyn Llm>> {
    let upstream = Upstream::start(script).await?;
    let client =
        OpenAIClient::compatible("sk-scripted", format!("{}/v1", upstream.base), "scripted")?
            .with_retry_config(fast_retries());
    Ok(Arc::new(client))
}

async fn gemini(script: Script) -> anyhow::Result<Arc<dyn Llm>> {
    let upstream = Upstream::start(script).await?;
    let model = GeminiModel::new_with_base_url(
        "gemini-scripted",
        "gemini-3.5-flash-lite",
        format!("{}/v1beta/", upstream.base),
    )?
    .with_retry_config(fast_retries());
    Ok(Arc::new(model))
}

async fn anthropic(script: Script) -> anyhow::Result<Arc<dyn Llm>> {
    let upstream = Upstream::start(script).await?;
    let client = AnthropicClient::new(
        AnthropicConfig::new("sk-ant-scripted", "claude-sonnet-4-6").with_base_url(upstream.base),
    )?
    .with_retry_config(fast_retries());
    Ok(Arc::new(client))
}

// ── Tools, guardrails, callbacks, toolsets ─────────────────────────────

fn failing_lookup() -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new("lookup_account", "Looks up an account", |_ctx, _args| async move {
        Err(AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Unavailable,
            "tool.orders_db.unavailable",
            format!("connect postgres://admin:{SECRET}@db.internal/prod failed"),
        ))
    }))
}

fn write_note() -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new("write_note", "Writes a note", |_ctx, _args| async move {
        Ok(json!({"written": true}))
    }))
}

fn send_report() -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new("send_report", "Sends a report", |_ctx, _args| async move {
        Ok(json!({"sent": true}))
    }))
}

/// A toolset whose server cannot be reached, with a credential in its connection string.
struct UnreachableToolset;

#[async_trait::async_trait]
impl Toolset for UnreachableToolset {
    fn name(&self) -> &str {
        "crm"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> adk_core::Result<Vec<Arc<dyn Tool>>> {
        Err(AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Unavailable,
            "tool.crm.unreachable",
            format!("GET https://crm.internal/tools?token={SECRET} failed: connection refused"),
        ))
    }
}

// ── Running a turn ─────────────────────────────────────────────────────

/// What one turn left behind.
struct Turn {
    error: Option<AdkError>,
    lines: Vec<String>,
    /// The planted value no record may contain.
    secret: String,
}

impl Turn {
    fn leaks(&self) -> Vec<&String> {
        self.lines.iter().filter(|line| line.contains(&self.secret)).collect()
    }

    /// Whether one log event names every fragment.
    fn logged(&self, fragments: &[&str]) -> bool {
        self.lines.iter().any(|line| {
            line.starts_with("EVENT") && fragments.iter().all(|fragment| line.contains(fragment))
        })
    }
}

async fn run_turn(
    capture: &Capture,
    agent: LlmAgentBuilder,
    record_payloads: bool,
) -> anyhow::Result<Turn> {
    let user = Content::new("user").with_text("Check my account.");
    let agent = agent.instruction("Answer briefly.").build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: "log-privacy".to_string(),
            user_id: "user".to_string(),
            session_id: None,
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("log-privacy")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .run_config(RunConfig { record_payloads, ..RunConfig::default() })
        .build()?;

    capture.clear();
    let mut error = None;
    let mut events = runner.run_str("user", session.id(), user).await?;
    while let Some(event) = events.next().await {
        if let Err(failure) = event {
            error = Some(failure);
        }
    }
    Ok(Turn { error, lines: capture.lines(), secret: SECRET.to_string() })
}

// ── Scenarios ──────────────────────────────────────────────────────────

struct Report {
    failures: usize,
}

impl Report {
    fn check(&mut self, scenario: &str, problems: Vec<String>) {
        if problems.is_empty() {
            println!("PASS  {scenario}");
        } else {
            self.failures += 1;
            println!("FAIL  {scenario}");
            for problem in problems {
                println!("        - {problem}");
            }
        }
    }
}

fn excerpt(line: &str) -> String {
    let line = line.replace(&padding(), "<4096 x>");
    if line.len() > 220 { format!("{}…", &line[..line.floor_char_boundary(220)]) } else { line }
}

/// The checks every failed model call must pass with payloads off.
fn model_failure_problems(turn: &Turn, expect_status: Option<u16>) -> Vec<String> {
    let mut problems = Vec::new();
    for leak in turn.leaks().into_iter().take(3) {
        problems.push(format!("secret logged: {}", excerpt(leak)));
    }
    let Some(error) = &turn.error else {
        problems.push("the run did not fail".to_string());
        return problems;
    };
    let category = error.category.to_string();
    let status = error.details.upstream_status_code.map(|status| status.to_string());
    let mut fragments = vec![error.code, category.as_str()];
    if let Some(status) = &status {
        fragments.push(status);
    }
    if !turn.logged(&fragments) {
        problems.push(format!("no log event carries the failure's diagnostics {fragments:?}"));
    }
    if let Some(expected) = expect_status
        && error.details.upstream_status_code != Some(expected)
    {
        problems.push(format!(
            "caller error has status {:?}, expected {expected}",
            error.details.upstream_status_code
        ));
    }
    if error.message.len() > MAX_ERROR_MESSAGE || error.message.contains(TAIL) {
        problems
            .push(format!("caller error message is not bounded: {} bytes", error.message.len()));
    }
    problems
}

/// The checks every failed tool-side call must pass with payloads off.
fn tool_failure_problems(turn: &Turn, diagnostics: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    for leak in turn.leaks().into_iter().take(3) {
        problems.push(format!("secret logged: {}", excerpt(leak)));
    }
    if !turn.logged(diagnostics) {
        problems.push(format!("no log event carries the diagnostics {diagnostics:?}"));
    }
    if let Some(error) = &turn.error {
        problems.push(format!("the run failed: {error}"));
    }
    problems
}

fn recorded_problems(turn: &Turn) -> Vec<String> {
    if turn.lines.iter().any(|line| line.starts_with("EVENT") && line.contains(SECRET)) {
        Vec::new()
    } else {
        vec!["no log event carries the message text while record_payloads is on".to_string()]
    }
}

async fn guardrail_agent(model: Arc<dyn Llm>) -> LlmAgentBuilder {
    let root = std::env::temp_dir().join("log-privacy-notes");
    let _ = std::fs::create_dir_all(&root);
    LlmAgentBuilder::new("assistant").model(model).tool(write_note()).tool_guardrails(
        ToolGuardrailSet::new()
            .with(PathAllowList::new("notes-only", ["path"], [root]).on_tools(["write_note"])),
    )
}

fn callback_agent(model: Arc<dyn Llm>) -> LlmAgentBuilder {
    LlmAgentBuilder::new("assistant").model(model).tool(send_report()).before_tool_callback(
        Box::new(|_ctx| {
            Box::pin(async move {
                Err::<Option<Content>, _>(AdkError::new(
                    ErrorComponent::Agent,
                    ErrorCategory::Unavailable,
                    "agent.policy_lookup.unavailable",
                    format!("policy lookup for {SECRET} failed"),
                ))
            })
        }),
    )
}

async fn scripted(capture: &Capture, report: &mut Report) -> anyhow::Result<()> {
    let agent = |model| LlmAgentBuilder::new("assistant").model(model);

    let turn = run_turn(capture, agent(openai(openai_400()).await?), false).await?;
    report.check("openai-compatible/http-400", model_failure_problems(&turn, Some(400)));

    let turn = run_turn(capture, agent(openai(openai_503_then_answer()).await?), false).await?;
    let mut problems: Vec<String> = turn
        .leaks()
        .into_iter()
        .take(3)
        .map(|leak| format!("secret logged: {}", excerpt(leak)))
        .collect();
    if !turn.logged(&["unavailable", "503"]) {
        problems.push("the retry log does not carry category and status".to_string());
    }
    if let Some(error) = &turn.error {
        problems.push(format!("the run failed after the retry: {error}"));
    }
    report.check("openai-compatible/retried-503", problems);

    let turn = run_turn(capture, agent(openai(openai_stream_error()).await?), false).await?;
    report.check("openai-compatible/stream-error", model_failure_problems(&turn, None));

    let turn = run_turn(capture, agent(gemini(gemini_400()).await?), false).await?;
    report.check("gemini/http-400", model_failure_problems(&turn, Some(400)));

    let turn = run_turn(capture, agent(gemini(gemini_stream_error()).await?), false).await?;
    report.check("gemini/stream-error", model_failure_problems(&turn, None));

    let turn = run_turn(capture, agent(gemini(gemini_answer_with_secret()).await?), false).await?;
    let mut problems: Vec<String> = turn
        .leaks()
        .into_iter()
        .take(3)
        .map(|leak| format!("answer logged: {}", excerpt(leak)))
        .collect();
    if let Some(error) = &turn.error {
        problems.push(format!("the run failed: {error}"));
    }
    report.check("gemini/answer-not-logged", problems);

    // A direct, non-streaming call: the response is not logged at any level either.
    let model = gemini(gemini_answer_json()).await?;
    capture.clear();
    let request = LlmRequest::new(
        "gemini-3.5-flash-lite",
        vec![Content::new("user").with_text("Check my account.")],
    );
    let mut problems = Vec::new();
    let mut responses = model.generate_content(request, false).await?;
    while let Some(response) = responses.next().await {
        if let Err(error) = response {
            problems.push(format!("the call failed: {error}"));
        }
    }
    let lines = capture.lines();
    for leak in lines.iter().filter(|line| line.contains(SECRET)).take(3) {
        problems.push(format!("answer logged: {}", excerpt(leak)));
    }
    report.check("gemini/direct-answer-not-logged", problems);

    let turn = run_turn(capture, agent(anthropic(anthropic_400()).await?), false).await?;
    report.check("anthropic/http-400", model_failure_problems(&turn, None));

    let model = openai(calls_tool("lookup_account", json!({"account": SECRET}))).await?;
    let turn = run_turn(capture, agent(model).tool(failing_lookup()), false).await?;
    report.check(
        "tool/credential-in-error",
        tool_failure_problems(&turn, &["tool.orders_db.unavailable", "unavailable"]),
    );

    let path = std::env::temp_dir().join(SECRET).join("notes.txt");
    let model = openai(calls_tool("write_note", json!({"path": path}))).await?;
    let turn = run_turn(capture, guardrail_agent(model).await, false).await?;
    report.check(
        "guardrail/path-denial",
        tool_failure_problems(&turn, &["notes-only", "write_note"]),
    );

    let model = openai(calls_tool("send_report", json!({"to": "ops"}))).await?;
    let turn = run_turn(capture, callback_agent(model), false).await?;
    report.check(
        "callback/error",
        tool_failure_problems(&turn, &["agent.policy_lookup.unavailable", "unavailable"]),
    );

    let model = openai(Arc::new(|_: &Request<'_>| chat_text("No CRM tools today."))).await?;
    let turn = run_turn(capture, agent(model).toolset(Arc::new(UnreachableToolset)), false).await?;
    report.check(
        "toolset/unreachable",
        tool_failure_problems(&turn, &["tool.crm.unreachable", "unavailable"]),
    );

    // The same failures with payload recording on: the message text is logged.
    let turn = run_turn(capture, agent(openai(openai_400()).await?), true).await?;
    report.check("record-payloads/openai-http-400", recorded_problems(&turn));
    let turn = run_turn(capture, agent(anthropic(anthropic_400()).await?), true).await?;
    report.check("record-payloads/anthropic-http-400", recorded_problems(&turn));
    let model = openai(calls_tool("lookup_account", json!({"account": "a-1"}))).await?;
    let turn = run_turn(capture, agent(model).tool(failing_lookup()), true).await?;
    report.check("record-payloads/tool-error", recorded_problems(&turn));
    let model = openai(calls_tool("write_note", json!({"path": path}))).await?;
    let turn = run_turn(capture, guardrail_agent(model).await, true).await?;
    report.check("record-payloads/guardrail-denial", recorded_problems(&turn));
    let model = openai(calls_tool("send_report", json!({"to": "ops"}))).await?;
    let turn = run_turn(capture, callback_agent(model), true).await?;
    report.check("record-payloads/callback-error", recorded_problems(&turn));

    caller_opt_out(capture, report).await
}

/// A caller keeps one run out of its trace by driving the run with `NoSubscriber`.
async fn caller_opt_out(capture: &Capture, report: &mut Report) -> anyhow::Result<()> {
    let model = openai(calls_tool("lookup_account", json!({"account": SECRET}))).await?;
    let agent = LlmAgentBuilder::new("assistant")
        .model(model)
        .tool(failing_lookup())
        .instruction("Answer briefly.")
        .build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: "log-privacy".to_string(),
            user_id: "user".to_string(),
            session_id: None,
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("log-privacy")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .run_config(RunConfig { record_payloads: true, ..RunConfig::default() })
        .build()?;

    capture.clear();
    // The run is driven with `NoSubscriber` inside the caller's span: its spans and events
    // go nowhere, while the caller's own events around it are still recorded.
    let run = async {
        let mut count = 0;
        let mut stream = runner
            .run_str("user", session.id(), Content::new("user").with_text("Check my account."))
            .await?;
        while let Some(event) = stream.next().await {
            event?;
            count += 1;
        }
        anyhow::Ok(count)
    }
    .with_subscriber(tracing::subscriber::NoSubscriber::default());
    let events = async {
        tracing::info!("caller before the run");
        let events = run.await?;
        tracing::info!("caller after the run");
        anyhow::Ok(events)
    }
    .instrument(tracing::info_span!("caller_request"))
    .await?;

    let lines = capture.lines();
    let mut problems = Vec::new();
    let from_run: Vec<&String> = lines
        .iter()
        .filter(|line| !line.contains("caller before") && !line.contains("caller after"))
        .filter(|line| !line.starts_with("SPAN caller_request"))
        .collect();
    if events == 0 {
        problems.push("the run produced no events".to_string());
    }
    if !from_run.is_empty() {
        problems.push(format!(
            "{} records from the run reached the caller's subscriber",
            from_run.len()
        ));
    }
    if !lines.iter().any(|line| line.contains("caller after")) {
        problems.push("the caller's own events after the run were not recorded".to_string());
    }
    report.check("caller/run-outside-the-trace", problems);
    Ok(())
}

/// OpenAI echoes the name of a request argument it does not recognize in its 400 response.
///
/// It masks the middle of an invalid enum value (`'acc...ret'`), so the planted value is an
/// argument name instead.
async fn live_openai(capture: &Capture, report: &mut Report) -> anyhow::Result<()> {
    let Ok(key) = std::env::var("OPENAI_API_KEY") else {
        println!("SKIP  live/openai-http-400 (OPENAI_API_KEY not set)");
        return Ok(());
    };
    let model_name = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.6-terra".to_string());
    let model: Arc<dyn Llm> = Arc::new(OpenAIClient::new(OpenAIConfig::new(key, model_name))?);
    let planted = SECRET.replace('-', "_");
    let mut openai = serde_json::Map::new();
    openai.insert(planted.clone(), json!(true));
    let mut extensions = serde_json::Map::new();
    extensions.insert("openai".to_string(), Value::Object(openai));
    let agent = LlmAgentBuilder::new("assistant")
        .model(model)
        .generate_content_config(GenerateContentConfig { extensions, ..Default::default() });

    let mut turn = run_turn(capture, agent, false).await?;
    turn.secret = planted;
    let echoed = turn.error.as_ref().is_some_and(|error| error.message.contains(&turn.secret));
    let mut problems = model_failure_problems(&turn, Some(400));
    if !echoed {
        let error = turn.error.as_ref().map(|error| excerpt(&error.to_string()));
        problems.push(format!("the provider did not echo the value: {error:?}"));
    }
    report.check("live/openai-http-400", problems);
    Ok(())
}

async fn live(capture: &Capture, report: &mut Report) -> anyhow::Result<()> {
    if std::env::var("SKIP_LIVE").is_ok_and(|value| value == "1") {
        println!("SKIP  live scenarios (SKIP_LIVE=1)");
        return Ok(());
    }
    live_openai(capture, report).await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The shell can export stale keys, so the env file overrides them. `LOG_PRIVACY_ENV`
    // names the file; the default is the repository root's `.env`.
    let env_file = std::env::var("LOG_PRIVACY_ENV")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env"));
    if env_file.exists() {
        dotenvy::from_path_override(&env_file)?;
    }

    let capture = Capture::install()?;
    let mut report = Report { failures: 0 };
    scripted(&capture, &mut report).await?;
    live(&capture, &mut report).await?;

    if report.failures > 0 {
        println!("\n{} scenario(s) failed", report.failures);
        std::process::exit(1);
    }
    println!("\nall scenarios passed");
    Ok(())
}
