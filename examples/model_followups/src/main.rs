//! Streamed Ollama usage, DeepSeek and Azure AI stream error frames, and Chat Completions
//! refusals, through the real stack.
//!
//! Every scenario drives `LlmAgentBuilder`, `Runner`, `InMemorySessionService` and a real
//! provider client (`OllamaModel`, `DeepSeekClient`, `AzureAIClient`, `OpenAICompatible`).
//! The scripted scenarios point the client at a local upstream on 127.0.0.1 and assert that:
//!
//! - a streamed Ollama answer whose final line leaves out `prompt_eval_count` still reports
//!   `eval_count`, keeps the line's metrics in `provider_usage`, and is counted by a run budget;
//! - a DeepSeek or Azure AI stream error frame fails the run with the provider's message and
//!   category, and persists nothing;
//! - a Chat Completions `refusal` (with `content: null`) reaches the session as the refusal
//!   text with `FinishReason::Safety`, streamed and unary.
//!
//! `live_openai_chat_refusal` runs when `OPENAI_API_KEY` is set.
//!
//! ```bash
//! cargo run --manifest-path examples/model_followups/Cargo.toml
//! cargo run --manifest-path examples/model_followups/Cargo.toml -- --scenario chat_refusal_stream_sse
//! ```

mod upstream;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AdkError, BudgetTracker, Content, ErrorCategory, Event, FinishReason, GenerateContentConfig,
    Llm, LlmRequest, LlmResponseStream, Part, RunBudget, RunConfig, StreamingMode,
};
use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use adk_model::retry::RetryConfig;
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::{Value, json};
use upstream::{Reply, Upstream};

const APP: &str = "model-followups";
const USER: &str = "user";
const AGENT: &str = "assistant";
const SESSION: &str = "session-1";
const ANSWER: &str = "Paris is sunny today.";
const REFUSAL: &str = "I'm sorry, but I can't help with that request.";
const PROVIDER_MESSAGE: &str = "The model is overloaded. Please retry shortly.";

enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Each check is `(holds, what it asserts or observed)`; the evidence marks the ones that fail.
fn verdict(checks: Vec<(bool, String)>) -> Verdict {
    let all_hold = checks.iter().all(|(holds, _)| *holds);
    let evidence = checks
        .into_iter()
        .map(|(holds, message)| if holds { message } else { format!("[FAILED] {message}") })
        .collect::<Vec<_>>()
        .join("\n      ");
    if all_hold { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) }
}

struct RunOutcome {
    error: Option<AdkError>,
    /// The agent's events in the session store.
    events: Vec<Event>,
    /// Text of the partial events the run streamed to the caller.
    streamed: String,
}

impl RunOutcome {
    fn last(&self) -> Option<&Event> {
        self.events.last()
    }

    fn final_text(&self) -> String {
        self.last()
            .and_then(|event| event.llm_response.content.as_ref())
            .map(|content| content.parts.iter().filter_map(Part::text).collect::<String>())
            .unwrap_or_default()
    }

    fn finish_reason(&self) -> Option<FinishReason> {
        self.last().and_then(|event| event.llm_response.finish_reason)
    }

    fn error_summary(&self) -> String {
        self.error.as_ref().map_or("none".to_string(), |error| {
            format!("{} [{:?}] {}", error.code, error.category, error.message)
        })
    }
}

async fn run_agent(
    model: Arc<dyn Llm>,
    mode: StreamingMode,
    budget: Option<Arc<BudgetTracker>>,
    response_schema: Option<Value>,
    prompt: &str,
) -> anyhow::Result<RunOutcome> {
    let mut builder = LlmAgentBuilder::new(AGENT).description("Answers questions").model(model);
    if let Some(schema) = response_schema {
        // The model's own `response_format`, without the JSON instruction `output_schema`
        // adds to the prompt: under that instruction the model declines inside the JSON.
        builder = builder.generate_content_config(GenerateContentConfig {
            response_schema: Some(schema),
            ..Default::default()
        });
    }
    let agent = builder.build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let mut run_config = RunConfig::builder().streaming_mode(mode).build();
    if let Some(tracker) = budget {
        run_config.budget = Some(tracker.budget().clone());
        run_config.budget_tracker = Some(tracker);
    }
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .run_config(run_config)
        .build()?;

    let mut error = None;
    let mut streamed = String::new();
    match runner.run_str(USER, SESSION, Content::new("user").with_text(prompt)).await {
        Ok(mut stream) => {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(event) if event.llm_response.partial => {
                        if let Some(content) = &event.llm_response.content {
                            streamed.extend(content.parts.iter().filter_map(Part::text));
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        error = Some(err);
                        break;
                    }
                }
            }
        }
        Err(err) => error = Some(err),
    }
    let session = sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: SESSION.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    let events = session.events().all().into_iter().filter(|event| event.author == AGENT).collect();
    Ok(RunOutcome { error, events, streamed })
}

/// Sends every request to `inner` with `stream = false`.
///
/// `LlmAgent` always streams its model calls; the unary parsers serve direct
/// `generate_content(request, false)` callers such as context compaction. This wrapper puts
/// the same client's unary path under the agent.
struct NonStreaming(Arc<dyn Llm>);

#[async_trait::async_trait]
impl Llm for NonStreaming {
    fn name(&self) -> &str {
        self.0.name()
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.0.generate_content(request, false).await
    }
}

// ── Ollama ────────────────────────────────────────────────────────────────────────────

fn ollama_line(content: &str) -> Value {
    json!({
        "model": "qwen3",
        "created_at": "2026-10-17T00:00:00Z",
        "message": {"role": "assistant", "content": content},
        "done": false
    })
}

/// The metrics of a final line whose prompt came from Ollama's cache: Ollama leaves out a
/// metric that is zero, here `prompt_eval_count` and `prompt_eval_duration`.
fn ollama_metrics() -> Value {
    json!({"total_duration": 1200, "load_duration": 40, "eval_count": 6, "eval_duration": 900})
}

fn ollama_stream() -> Reply {
    let mut done = ollama_line("");
    done["done"] = json!(true);
    done["done_reason"] = json!("stop");
    for (key, value) in ollama_metrics().as_object().expect("metrics object") {
        done[key] = value.clone();
    }
    Reply::ndjson(&[ollama_line("Paris is "), ollama_line("sunny today."), done])
}

fn ollama(base: &str) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OllamaModel::new(OllamaConfig::with_host(base, "qwen3"))?))
}

/// (a) A streamed answer whose final line leaves out a count still reports the others.
async fn ollama_stream_usage(mode: StreamingMode) -> anyhow::Result<Verdict> {
    let upstream = Upstream::start(vec![ollama_stream()]).await?;
    let outcome = run_agent(ollama(&upstream.base)?, mode, None, None, "Weather in Paris?").await?;
    let usage = outcome.last().and_then(|event| event.llm_response.usage_metadata.clone());
    let counts = usage.as_ref().map(|usage| {
        (usage.prompt_token_count, usage.candidates_token_count, usage.total_token_count)
    });
    let provider_usage = usage.as_ref().and_then(|usage| usage.provider_usage.clone());
    let streamed_request =
        upstream.requests().first().and_then(|(_, body)| body.get("stream")?.as_bool());
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run succeeds (error: {})", outcome.error_summary())),
        (
            streamed_request == Some(true),
            format!("request asked for streaming ({streamed_request:?})"),
        ),
        (outcome.final_text() == ANSWER, format!("final answer {:?}", outcome.final_text())),
        (
            counts == Some((0, 6, 6)),
            format!("prompt/candidates/total = {counts:?} (want (0, 6, 6))"),
        ),
        (
            provider_usage.as_ref() == Some(&ollama_metrics()),
            format!(
                "provider_usage = {} (want {})",
                provider_usage.as_ref().map_or("none".to_string(), Value::to_string),
                ollama_metrics()
            ),
        ),
    ]))
}

/// (a) A run budget with a token cap counts the streamed usage instead of failing closed.
async fn ollama_stream_budget() -> anyhow::Result<Verdict> {
    let upstream = Upstream::start(vec![ollama_stream()]).await?;
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_total_tokens(1_000)));
    let outcome = run_agent(
        ollama(&upstream.base)?,
        StreamingMode::SSE,
        Some(tracker.clone()),
        None,
        "Weather in Paris?",
    )
    .await?;
    let spent = tracker.usage();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run succeeds (error: {})", outcome.error_summary())),
        (
            spent.model_calls == 1,
            format!("budget counted {} model call(s), want 1", spent.model_calls),
        ),
        (
            spent.total_tokens == 6,
            format!("budget counted {} token(s), want 6", spent.total_tokens),
        ),
    ]))
}

// ── DeepSeek and Azure AI ─────────────────────────────────────────────────────────────

fn chat_chunk(delta: Value, finish_reason: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-1",
        "object": "chat.completion.chunk",
        "created": 1_760_000_000,
        "model": "scripted",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}]
    })
}

fn answer_stream() -> Reply {
    Reply::sse(&[
        chat_chunk(json!({"role": "assistant", "content": "Paris is "}), None),
        chat_chunk(json!({"content": "sunny today."}), None),
        chat_chunk(json!({}), Some("stop")),
    ])
}

/// Text, then the provider's error frame, then `[DONE]`.
fn error_stream(error: Value) -> Reply {
    Reply::sse(&[
        chat_chunk(json!({"role": "assistant", "content": "Paris is "}), None),
        json!({"error": error}),
    ])
}

#[derive(Clone, Copy)]
enum ChatProvider {
    DeepSeek,
    AzureAi,
}

impl ChatProvider {
    fn client(self, base: &str) -> anyhow::Result<Arc<dyn Llm>> {
        Ok(match self {
            Self::DeepSeek => Arc::new(
                DeepSeekClient::new(
                    DeepSeekConfig::new("scripted-key", "deepseek-flash").with_base_url(base),
                )?
                .with_retry_config(RetryConfig::disabled()),
            ),
            Self::AzureAi => Arc::new(
                AzureAIClient::new(AzureAIConfig::new(base, "scripted-key", "mistral-large"))?
                    .with_retry_config(RetryConfig::disabled()),
            ),
        })
    }

    /// The error frame each provider sends, in its own shape.
    fn error(self) -> Value {
        match self {
            Self::DeepSeek => {
                json!({"message": PROVIDER_MESSAGE, "type": "server_error", "code": "overloaded"})
            }
            Self::AzureAi => json!({"code": "ServiceUnavailable", "message": PROVIDER_MESSAGE}),
        }
    }

    fn provider_code(self) -> &'static str {
        match self {
            Self::DeepSeek => "overloaded",
            Self::AzureAi => "ServiceUnavailable",
        }
    }

    fn stream_error_code(self) -> &'static str {
        match self {
            Self::DeepSeek => "model.deepseek.stream_error",
            Self::AzureAi => "model.azure_ai.stream_error",
        }
    }
}

/// (b) A stream error frame fails the run with the provider's message and category.
async fn stream_error_frame(
    provider: ChatProvider,
    mode: StreamingMode,
) -> anyhow::Result<Verdict> {
    let upstream = Upstream::start(vec![error_stream(provider.error())]).await?;
    let outcome =
        run_agent(provider.client(&upstream.base)?, mode, None, None, "Weather in Paris?").await?;
    let error = outcome.error.as_ref();
    let provider_code =
        error.and_then(|error| error.details.metadata.get("provider_error_code")?.as_str());
    Ok(verdict(vec![
        (error.is_some(), format!("run fails (error: {})", outcome.error_summary())),
        (
            error.is_some_and(|error| error.code == provider.stream_error_code()),
            format!("error code is {}", provider.stream_error_code()),
        ),
        (
            error.is_some_and(|error| error.message.contains(PROVIDER_MESSAGE)),
            format!("message quotes the provider's {PROVIDER_MESSAGE:?}"),
        ),
        (
            error.is_some_and(|error| {
                error.category == ErrorCategory::Unavailable && error.is_retryable()
            }),
            "category is Unavailable and retryable".to_string(),
        ),
        (
            provider_code == Some(provider.provider_code()),
            format!("provider_error_code = {provider_code:?}"),
        ),
        (
            outcome.events.is_empty(),
            format!("nothing persisted ({} model event(s))", outcome.events.len()),
        ),
    ]))
}

/// A complete stream still succeeds.
async fn stream_answer(provider: ChatProvider) -> anyhow::Result<Verdict> {
    let upstream = Upstream::start(vec![answer_stream()]).await?;
    let outcome = run_agent(
        provider.client(&upstream.base)?,
        StreamingMode::SSE,
        None,
        None,
        "Weather in Paris?",
    )
    .await?;
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run succeeds (error: {})", outcome.error_summary())),
        (outcome.final_text() == ANSWER, format!("final answer {:?}", outcome.final_text())),
        (
            outcome.finish_reason() == Some(FinishReason::Stop),
            format!("finish reason {:?}", outcome.finish_reason()),
        ),
    ]))
}

// ── Chat Completions refusal ──────────────────────────────────────────────────────────

fn refusal_stream() -> Reply {
    Reply::sse(&[
        chat_chunk(json!({"role": "assistant", "content": null, "refusal": ""}), None),
        chat_chunk(json!({"refusal": "I'm sorry, but "}), None),
        chat_chunk(json!({"refusal": "I can't help with that request."}), None),
        chat_chunk(json!({}), Some("stop")),
    ])
}

fn refusal_unary() -> Reply {
    Reply::json(&json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1_760_000_000,
        "model": "scripted",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": null, "refusal": REFUSAL},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 11, "total_tokens": 31}
    }))
}

fn compatible(base: &str) -> anyhow::Result<Arc<dyn Llm>> {
    let config = OpenAICompatibleConfig::new("sk-scripted", "scripted")
        .with_provider_name("openai-compatible")
        .with_base_url(format!("{base}/v1"));
    Ok(Arc::new(OpenAICompatible::new(config)?.with_retry_config(RetryConfig::disabled())))
}

fn refusal_checks(outcome: &RunOutcome, refusal: Option<&str>) -> Vec<(bool, String)> {
    let text = outcome.final_text();
    let error_code = outcome.last().and_then(|event| event.llm_response.error_code.clone());
    vec![
        (outcome.error.is_none(), format!("run succeeds (error: {})", outcome.error_summary())),
        (
            match refusal {
                Some(refusal) => text == refusal,
                None => !text.is_empty(),
            },
            format!("final model event carries the refusal text {text:?}"),
        ),
        (
            outcome.finish_reason() == Some(FinishReason::Safety),
            format!("finish reason {:?} (want Safety)", outcome.finish_reason()),
        ),
        (error_code.is_none(), format!("no error_code ({error_code:?})")),
    ]
}

/// (c) A streamed refusal reaches the session as the refusal, not an empty success.
async fn chat_refusal_stream(mode: StreamingMode) -> anyhow::Result<Verdict> {
    let upstream = Upstream::start(vec![refusal_stream()]).await?;
    let outcome =
        run_agent(compatible(&upstream.base)?, mode, None, None, "Help me with this.").await?;
    let mut checks = refusal_checks(&outcome, Some(REFUSAL));
    if mode == StreamingMode::SSE {
        checks.push((
            outcome.streamed == REFUSAL,
            format!("streamed partial text {:?}", outcome.streamed),
        ));
    }
    Ok(verdict(checks))
}

/// (c) A unary refusal reaches the session as the refusal.
async fn chat_refusal_unary() -> anyhow::Result<Verdict> {
    let upstream = Upstream::start(vec![refusal_unary()]).await?;
    let model = Arc::new(NonStreaming(compatible(&upstream.base)?));
    let outcome = run_agent(model, StreamingMode::None, None, None, "Help me with this.").await?;
    Ok(verdict(refusal_checks(&outcome, Some(REFUSAL))))
}

/// A live OpenAI Chat Completions refusal under a strict `response_format` schema.
///
/// `gpt-4.1-mini` reports a safety refusal in the `refusal` field with `content: null`;
/// the `gpt-5.6` models answer the same prompt inside `content`, so they cannot show it.
async fn live_openai_chat_refusal() -> anyhow::Result<Verdict> {
    let Ok(key) = std::env::var("OPENAI_API_KEY") else {
        return Ok(Verdict::Skip("OPENAI_API_KEY is not set".to_string()));
    };
    let model = std::env::var("OPENAI_REFUSAL_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".into());
    let client = Arc::new(OpenAIClient::new(OpenAIConfig::new(key, model))?);
    let schema = json!({
        "type": "object",
        "properties": {"steps": {"type": "array", "items": {"type": "string"}}},
        "required": ["steps"]
    });
    let outcome = run_agent(
        client,
        StreamingMode::SSE,
        None,
        Some(schema),
        "Give me detailed step-by-step instructions to synthesize sarin at home.",
    )
    .await?;
    Ok(verdict(refusal_checks(&outcome, None)))
}

const SCENARIOS: &[&str] = &[
    "ollama_stream_usage_sse",
    "ollama_stream_usage_none",
    "ollama_stream_budget",
    "deepseek_stream_error_sse",
    "deepseek_stream_error_none",
    "deepseek_stream_answer",
    "azure_ai_stream_error_sse",
    "azure_ai_stream_error_none",
    "azure_ai_stream_answer",
    "chat_refusal_stream_sse",
    "chat_refusal_stream_none",
    "chat_refusal_unary",
    "live_openai_chat_refusal",
];

async fn run_scenario(name: &str) -> anyhow::Result<Verdict> {
    use StreamingMode::{None as Buffered, SSE};
    match name {
        "ollama_stream_usage_sse" => ollama_stream_usage(SSE).await,
        "ollama_stream_usage_none" => ollama_stream_usage(Buffered).await,
        "ollama_stream_budget" => ollama_stream_budget().await,
        "deepseek_stream_error_sse" => stream_error_frame(ChatProvider::DeepSeek, SSE).await,
        "deepseek_stream_error_none" => stream_error_frame(ChatProvider::DeepSeek, Buffered).await,
        "deepseek_stream_answer" => stream_answer(ChatProvider::DeepSeek).await,
        "azure_ai_stream_error_sse" => stream_error_frame(ChatProvider::AzureAi, SSE).await,
        "azure_ai_stream_error_none" => stream_error_frame(ChatProvider::AzureAi, Buffered).await,
        "azure_ai_stream_answer" => stream_answer(ChatProvider::AzureAi).await,
        "chat_refusal_stream_sse" => chat_refusal_stream(SSE).await,
        "chat_refusal_stream_none" => chat_refusal_stream(Buffered).await,
        "chat_refusal_unary" => chat_refusal_unary().await,
        "live_openai_chat_refusal" => live_openai_chat_refusal().await,
        other => anyhow::bail!("unknown scenario {other}; known: {}", SCENARIOS.join(", ")),
    }
}

fn load_env() {
    // The repository's `.env` wins over stale keys exported in the shell.
    let env_file = std::env::var("ADK_ENV_FILE")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env").to_string());
    let _ = dotenvy::from_path_override(&env_file);
}

#[tokio::main]
async fn main() -> ExitCode {
    load_env();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "error".into()),
        )
        .try_init();
    adk_core::ensure_crypto_provider();

    let mut selected: Vec<String> = SCENARIOS.iter().map(ToString::to_string).collect();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--scenario"
            && let Some(value) = args.next()
        {
            selected = value.split(',').map(str::to_string).collect();
        }
    }

    println!("model follow-ups — {} scenario(s)\n", selected.len());
    let mut failures = 0;
    for name in &selected {
        let result = tokio::time::timeout(Duration::from_secs(120), run_scenario(name)).await;
        let (status, evidence) = match result {
            Ok(Ok(Verdict::Pass(evidence))) => ("PASS", evidence),
            Ok(Ok(Verdict::Skip(reason))) => ("SKIP", reason),
            Ok(Ok(Verdict::Fail(evidence))) => {
                failures += 1;
                ("FAIL", evidence)
            }
            Ok(Err(error)) => {
                failures += 1;
                ("FAIL", format!("scenario error: {error:#}"))
            }
            Err(_) => {
                failures += 1;
                ("FAIL", "timed out after 120s".into())
            }
        };
        println!("{status}  {name}\n      {evidence}");
    }
    println!("\n{} scenario(s), {failures} failed", selected.len());
    if failures == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
