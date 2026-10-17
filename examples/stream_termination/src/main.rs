//! Model streams that end early or carry an error fail the run.
//!
//! Every scripted scenario drives the stack a user runs — `LlmAgentBuilder`, `Runner`, an
//! in-memory session service, and a real provider client — against a local upstream on
//! 127.0.0.1 that scripts one wire fault. The scenarios assert that:
//!
//! - a stream that ends before its terminal event, ends mid tool call, or carries an error
//!   frame fails the run, persists no answer, sets no `output_key` state, and runs no tool;
//! - `data:` fields without a space and multi-byte characters split across network writes
//!   still produce the exact answer;
//! - complete answers and complete tool calls still succeed.
//!
//! Each scripted scenario runs in both `StreamingMode::SSE` and `StreamingMode::None`.
//! Live scenarios run against OpenAI Chat Completions, Groq, and OpenRouter when their keys
//! are set, to show normal streaming still succeeds. Set `SKIP_LIVE=1` to skip them.

mod scripts;
mod upstream;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AdkError, Content, FinishReason, GenerateContentConfig, Llm, LlmRequest, Part, RunConfig,
    SessionId, StreamingMode, UserId,
};
use adk_model::groq::{GroqClient, GroqConfig};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_model::openrouter::{OpenRouterApiMode, OpenRouterClient, OpenRouterConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use schemars::JsonSchema;
use scripts::{ANSWER, Fault, MULTIBYTE, TOOL_ANSWER};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use upstream::Upstream;

const APP: &str = "stream-termination";
const AGENT: &str = "assistant";
const USER: &str = "user";
const SESSION: &str = "session";

#[derive(Clone, Copy, Debug)]
enum Provider {
    /// `OpenAIClient::compatible`: the shared Chat Completions path behind OpenAI Chat,
    /// Azure OpenAI, OpenCode Chat, and every compatible preset.
    OpenAiCompatible,
    Groq,
    Ollama,
    OpenRouterChat,
    OpenRouterResponses,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai-compatible",
            Self::Groq => "groq",
            Self::Ollama => "ollama",
            Self::OpenRouterChat => "openrouter-chat",
            Self::OpenRouterResponses => "openrouter-responses",
        }
    }

    fn script(self, fault: Fault) -> upstream::Script {
        match self {
            Self::OpenAiCompatible | Self::Groq => scripts::chat(fault, false),
            Self::OpenRouterChat => scripts::chat(fault, true),
            Self::Ollama => scripts::ollama(fault),
            Self::OpenRouterResponses => scripts::responses(fault),
        }
    }

    fn client(self, base: &str) -> anyhow::Result<Arc<dyn Llm>> {
        Ok(match self {
            Self::OpenAiCompatible => {
                Arc::new(OpenAIClient::compatible("sk-scripted", format!("{base}/v1"), "scripted")?)
            }
            Self::Groq => Arc::new(GroqClient::new(
                GroqConfig::new("gsk-scripted", "openai/gpt-oss-120b").with_base_url(base),
            )?),
            Self::Ollama => Arc::new(OllamaModel::new(OllamaConfig::with_host(base, "qwen3.5"))?),
            Self::OpenRouterChat => Arc::new(OpenRouterClient::new(
                OpenRouterConfig::new("sk-or-scripted", "openai/gpt-4.1-mini")
                    .with_base_url(base)
                    .with_default_api_mode(OpenRouterApiMode::ChatCompletions),
            )?),
            Self::OpenRouterResponses => Arc::new(OpenRouterClient::new(
                OpenRouterConfig::new("sk-or-scripted", "openai/gpt-4.1-mini")
                    .with_base_url(base)
                    .with_default_api_mode(OpenRouterApiMode::Responses),
            )?),
        })
    }
}

/// What a scenario requires of the run.
#[derive(Clone, Copy, Debug)]
enum Expect {
    /// The run fails, persists no answer, sets no `output_key` state, and runs no tool.
    Fails,
    /// The run succeeds and persists exactly this answer.
    Answer(&'static str),
    /// The weather tool runs once and the run persists exactly this answer.
    ToolThenAnswer(&'static str),
}

/// What one run left behind.
#[derive(Debug, Default)]
struct Outcome {
    error: Option<String>,
    /// Text and finish reason of every non-partial model event in the session store.
    persisted: Vec<(String, Option<FinishReason>)>,
    /// `state["answer"]`, written by the agent's `output_key`.
    answer: Option<Value>,
    tool_runs: usize,
    /// Text of the partial events the run streamed to the caller.
    streamed: String,
}

impl Outcome {
    fn final_text(&self) -> Option<&str> {
        self.persisted.last().map(|(text, _)| text.as_str())
    }

    fn summary(&self) -> String {
        match &self.error {
            Some(error) => format!("run failed: {error}"),
            None => format!(
                "run ok, persisted {:?}, tool runs {}",
                self.persisted.iter().map(|(text, _)| text).collect::<Vec<_>>(),
                self.tool_runs
            ),
        }
    }
}

fn check_fails(outcome: &Outcome) -> Result<(), String> {
    if outcome.error.is_none() {
        return Err(format!("expected the run to fail; {}", outcome.summary()));
    }
    if !outcome.persisted.is_empty() {
        return Err(format!("a failed turn persisted {:?}", outcome.persisted));
    }
    if let Some(answer) = &outcome.answer {
        return Err(format!("a failed turn wrote output_key state {answer}"));
    }
    if outcome.tool_runs > 0 {
        return Err(format!("a failed turn ran the tool {} time(s)", outcome.tool_runs));
    }
    Ok(())
}

fn check_answer(outcome: &Outcome, expected: &str, tool_runs: usize) -> Result<(), String> {
    if let Some(error) = &outcome.error {
        return Err(format!("expected {expected:?}; run failed: {error}"));
    }
    if outcome.tool_runs != tool_runs {
        return Err(format!("expected {tool_runs} tool run(s); {}", outcome.summary()));
    }
    if outcome.final_text() != Some(expected) {
        return Err(format!("expected persisted {expected:?}; {}", outcome.summary()));
    }
    if outcome.answer.as_ref().and_then(Value::as_str) != Some(expected) {
        return Err(format!("expected output_key {expected:?}, got {:?}", outcome.answer));
    }
    if tool_runs == 0 && !outcome.streamed.is_empty() && outcome.streamed != expected {
        return Err(format!("streamed {:?} but persisted {expected:?}", outcome.streamed));
    }
    Ok(())
}

fn verdict(expect: Expect, outcome: &Outcome) -> Result<(), String> {
    match expect {
        Expect::Fails => check_fails(outcome),
        Expect::Answer(text) => check_answer(outcome, text, 0),
        Expect::ToolThenAnswer(text) => check_answer(outcome, text, 1),
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct WeatherArgs {
    /// City to look up.
    city: String,
}

fn weather_tool(runs: Arc<AtomicUsize>) -> FunctionTool {
    FunctionTool::new(
        "get_weather",
        "Returns the current weather for a city.",
        move |_ctx, args| {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                let city =
                    args.get("city").and_then(Value::as_str).unwrap_or("unknown").to_string();
                Ok(json!({"city": city, "forecast": "sunny"}))
            }
        },
    )
    .with_parameters_schema::<WeatherArgs>()
    .with_read_only(true)
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

async fn run_turn(
    model: Arc<dyn Llm>,
    mode: StreamingMode,
    with_tool: bool,
    prompt: &str,
    config: Option<GenerateContentConfig>,
) -> anyhow::Result<Outcome> {
    let tool_runs = Arc::new(AtomicUsize::new(0));
    let mut builder = LlmAgentBuilder::new(AGENT)
        .instruction("Answer briefly.")
        .model(model)
        .output_key("answer");
    if with_tool {
        builder = builder.tool(Arc::new(weather_tool(tool_runs.clone())));
    }
    if let Some(config) = config {
        builder = builder.generate_content_config(config);
    }
    let agent = Arc::new(builder.build()?);

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
    match runner
        .run_with_config(
            UserId::new(USER)?,
            SessionId::new(SESSION)?,
            Content::new("user").with_text(prompt),
            Some(run_config),
        )
        .await
    {
        Ok(mut stream) => {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(event) if event.llm_response.partial => {
                        let parts = event.llm_response.content.iter().flat_map(|c| &c.parts);
                        outcome.streamed.extend(parts.filter_map(Part::text));
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
        .filter_map(|event| {
            let content = event.llm_response.content.as_ref()?;
            let text: String = content.parts.iter().filter_map(Part::text).collect();
            (content.role == "model" && !text.is_empty())
                .then_some((text, event.llm_response.finish_reason))
        })
        .collect();
    outcome.answer = session.state().get("answer");
    outcome.tool_runs = tool_runs.load(Ordering::SeqCst);
    Ok(outcome)
}

struct Report {
    rows: Vec<(String, Result<(), String>)>,
}

impl Report {
    fn record(&mut self, name: String, result: Result<(), String>) {
        match &result {
            Ok(()) => println!("PASS  {name}"),
            Err(reason) => println!("FAIL  {name}\n      {reason}"),
        }
        self.rows.push((name, result));
    }

    fn failures(&self) -> usize {
        self.rows.iter().filter(|(_, result)| result.is_err()).count()
    }
}

fn mode_label(mode: StreamingMode) -> &'static str {
    match mode {
        StreamingMode::None => "none",
        StreamingMode::SSE => "sse",
        StreamingMode::Bidi => "bidi",
    }
}

const MODES: [StreamingMode; 2] = [StreamingMode::SSE, StreamingMode::None];

/// The scripted scenarios: provider, fault, whether the agent has the tool, and the
/// required outcome.
fn scripted_scenarios() -> Vec<(Provider, Fault, bool, Expect)> {
    use Expect::{Answer, Fails, ToolThenAnswer};
    use Fault::*;
    use Provider::*;
    let mut scenarios = Vec::new();
    for provider in [OpenAiCompatible, Groq, OpenRouterChat, OpenRouterResponses, Ollama] {
        scenarios.extend([
            (provider, Complete, false, Answer(ANSWER)),
            (provider, CutMidAnswer, false, Fails),
            (provider, CutMidToolCall, true, Fails),
            (provider, ErrorFrame, false, Fails),
            (provider, SplitMultibyte, false, Answer(MULTIBYTE)),
            (provider, ToolCall, true, ToolThenAnswer(TOOL_ANSWER)),
        ]);
        if !matches!(provider, Ollama) {
            scenarios.push((provider, DataWithoutSpace, false, Answer(ANSWER)));
        }
    }
    scenarios.push((OpenAiCompatible, ErrorFinishReason, false, Fails));
    scenarios.push((Groq, ErrorFinishReason, false, Fails));
    // `null` token counts are unknown usage, so the finish chunk decodes and the call runs.
    scenarios.push((Groq, ToolCallNullUsage, true, ToolThenAnswer(TOOL_ANSWER)));
    scenarios
}

async fn run_scripted(report: &mut Report) -> anyhow::Result<()> {
    for (provider, fault, with_tool, expect) in scripted_scenarios() {
        for mode in MODES {
            let upstream = Upstream::start(provider.script(fault)).await?;
            let model = provider.client(&upstream.base)?;
            let prompt = if with_tool {
                "What is the weather in Paris?"
            } else {
                "What is the capital of France?"
            };
            let outcome = run_turn(model, mode, with_tool, prompt, None).await?;
            let mut result = verdict(expect, &outcome);
            if result.is_ok() && upstream.requests() == 0 {
                result = Err("the client never reached the scripted upstream".into());
            }
            let name =
                format!("scripted {}/{}/{}", provider.label(), fault.label(), mode_label(mode));
            let passed = result.is_ok();
            report.record(name, result);
            if let (true, Some(error)) = (passed, &outcome.error) {
                println!("      {error}");
            }
        }
    }
    Ok(())
}

/// A direct non-streaming call, as a graph node or custom code makes one: a Responses body
/// with `status: "failed"` must be an error, not an empty successful turn.
async fn run_direct_failed_response(report: &mut Report) -> anyhow::Result<()> {
    let upstream = Upstream::start(scripts::responses_failed_json()).await?;
    let model = Provider::OpenRouterResponses.client(&upstream.base)?;
    let request = LlmRequest::new(
        "openai/gpt-4.1-mini",
        vec![Content::new("user").with_text("What is the capital of France?")],
    );
    let result = match model.generate_content(request, false).await {
        Err(error) => Ok(error),
        Ok(mut stream) => {
            let mut responses = Vec::new();
            let mut failure = None;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(response) => responses.push(response),
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            failure.ok_or_else(|| {
                let finish = responses.last().and_then(|response| response.finish_reason);
                format!(
                    "a failed response returned Ok ({} response(s), finish_reason {finish:?})",
                    responses.len()
                )
            })
        }
    };
    let summary = result.as_ref().ok().map(error_summary);
    report
        .record("direct openrouter-responses/status-failed/non-streaming".into(), result.map(drop));
    if let Some(summary) = summary {
        println!("      {summary}");
    }
    Ok(())
}

fn env_key(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn max_tokens(limit: i32) -> GenerateContentConfig {
    GenerateContentConfig { max_output_tokens: Some(limit), ..GenerateContentConfig::default() }
}

fn check_live_answer(outcome: &Outcome, needle: &str, tool_runs: usize) -> Result<(), String> {
    if let Some(error) = &outcome.error {
        return Err(format!("run failed: {error}"));
    }
    if outcome.tool_runs < tool_runs {
        return Err(format!("expected the tool to run; {}", outcome.summary()));
    }
    let Some(text) = outcome.final_text() else {
        return Err(format!("no answer persisted; {}", outcome.summary()));
    };
    if !text.to_lowercase().contains(needle) {
        return Err(format!("expected the answer to mention {needle:?}; got {text:?}"));
    }
    if outcome.answer.as_ref().and_then(Value::as_str) != Some(text) {
        return Err(format!("output_key {:?} differs from the persisted answer", outcome.answer));
    }
    // A turn without tools streams exactly the answer it persists.
    if tool_runs == 0 && !outcome.streamed.is_empty() && outcome.streamed != text {
        return Err(format!("streamed {:?} but persisted {text:?}", outcome.streamed));
    }
    Ok(())
}

async fn run_live(report: &mut Report) -> anyhow::Result<()> {
    if env_key("SKIP_LIVE").is_some() {
        println!("SKIP  live scenarios (SKIP_LIVE is set)");
        return Ok(());
    }
    let capital = "What is the capital of France? Answer in one short sentence.";
    let weather = "Use the get_weather tool for Paris, then tell me the forecast in one sentence.";

    match env_key("OPENAI_API_KEY") {
        Some(key) => {
            let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.6-terra".into());
            for mode in MODES {
                let client: Arc<dyn Llm> =
                    Arc::new(OpenAIClient::new(OpenAIConfig::new(key.clone(), model.clone()))?);
                let outcome = run_turn(client, mode, false, capital, None).await?;
                report.record(
                    format!("live openai-chat/{model}/answer/{}", mode_label(mode)),
                    check_live_answer(&outcome, "paris", 0),
                );
            }
        }
        None => println!("SKIP  live openai-chat (OPENAI_API_KEY is not set)"),
    }

    match env_key("GROQ_API_KEY") {
        Some(key) => {
            let model =
                std::env::var("GROQ_MODEL").unwrap_or_else(|_| "openai/gpt-oss-120b".into());
            for mode in MODES {
                let client: Arc<dyn Llm> =
                    Arc::new(GroqClient::new(GroqConfig::new(key.clone(), model.clone()))?);
                let outcome = run_turn(client, mode, true, weather, None).await?;
                report.record(
                    format!("live groq/{model}/tool-call/{}", mode_label(mode)),
                    check_live_answer(&outcome, "sunny", 1),
                );
            }
        }
        None => println!("SKIP  live groq (GROQ_API_KEY is not set)"),
    }

    match env_key("OPENROUTER_API_KEY") {
        Some(key) => {
            let model =
                std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| "openai/gpt-4.1-mini".into());
            let client = |mode: OpenRouterApiMode| -> anyhow::Result<Arc<dyn Llm>> {
                Ok(Arc::new(OpenRouterClient::new(
                    OpenRouterConfig::new(key.clone(), model.clone()).with_default_api_mode(mode),
                )?))
            };
            for (api, label) in [
                (OpenRouterApiMode::ChatCompletions, "openrouter-chat"),
                (OpenRouterApiMode::Responses, "openrouter-responses"),
            ] {
                for mode in MODES {
                    let outcome =
                        run_turn(client(api)?, mode, false, capital, Some(max_tokens(256))).await?;
                    report.record(
                        format!("live {label}/{model}/answer/{}", mode_label(mode)),
                        check_live_answer(&outcome, "paris", 0),
                    );
                }
            }
            // OpenRouter ends a Responses stream that hits `max_output_tokens` with
            // `response.incomplete`; the turn succeeds and records why it stopped.
            let outcome = run_turn(
                client(OpenRouterApiMode::Responses)?,
                StreamingMode::SSE,
                false,
                "Count from one to two hundred in words.",
                Some(max_tokens(16)),
            )
            .await?;
            let result = match (&outcome.error, outcome.persisted.last()) {
                (Some(error), _) => Err(format!("run failed: {error}")),
                (None, Some((_, Some(FinishReason::MaxTokens)))) => Ok(()),
                (None, last) => {
                    Err(format!("expected a persisted MaxTokens answer; last persisted {last:?}"))
                }
            };
            report
                .record(format!("live openrouter-responses/{model}/max-output-tokens/sse"), result);
        }
        None => println!("SKIP  live openrouter (OPENROUTER_API_KEY is not set)"),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    let _ = dotenvy::dotenv();

    let mut report = Report { rows: Vec::new() };
    run_scripted(&mut report).await?;
    run_direct_failed_response(&mut report).await?;
    run_live(&mut report).await?;

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
