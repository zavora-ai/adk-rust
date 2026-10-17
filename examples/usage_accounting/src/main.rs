//! Token usage that a provider reports with `null` or missing counts, through the real stack.
//!
//! Every scenario drives `LlmAgentBuilder`, `Runner`, `InMemorySessionService` and a real
//! provider client. The scripted scenarios point the client at a local upstream on 127.0.0.1
//! that answers a `get_weather` call and then a text answer, each with a usage object that has
//! `null` or missing counts and fields the typed client does not model. They assert that the
//! turn succeeds, the tool runs once, the session's usage is what the upstream sent, and a run
//! budget counts it. The `live_*` scenarios run when the provider's key is set and assert that
//! `provider_usage` is the provider's own usage object.
//!
//! ```bash
//! cargo run --manifest-path examples/usage_accounting/Cargo.toml
//! cargo run --manifest-path examples/usage_accounting/Cargo.toml -- --scenario groq_stream,budget_ollama_unary
//! ```

mod upstream;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    BudgetTracker, Content, Event, Llm, LlmRequest, LlmResponseStream, Part, RunBudget, RunConfig,
    StreamingMode, ToolContext,
};
use adk_model::GeminiModel;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use adk_model::groq::{GroqClient, GroqConfig};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_model::retry::RetryConfig;
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use serde_json::{Value, json};
use upstream::{ANSWER, Provider, Upstream, expected};

const APP: &str = "usage-accounting";
const USER: &str = "user";
const AGENT: &str = "forecaster";

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
    error: Option<String>,
    events: Vec<Event>,
    tool_runs: usize,
}

impl RunOutcome {
    /// Usage on the agent's persisted model events, in order.
    fn usages(&self) -> Vec<&adk_core::UsageMetadata> {
        self.events
            .iter()
            .filter(|event| event.author == AGENT)
            .filter_map(|event| event.llm_response.usage_metadata.as_ref())
            .collect()
    }

    fn final_text(&self) -> String {
        self.events
            .iter()
            .rev()
            .filter(|event| event.author == AGENT)
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(Part::text)
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

fn weather_tool(runs: Arc<AtomicUsize>) -> Arc<dyn adk_core::Tool> {
    Arc::new(FunctionTool::new(
        "get_weather",
        "Returns the current weather for a city. Arguments: city (string).",
        move |_ctx: Arc<dyn ToolContext>, args: Value| {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(
                    json!({"city": args.get("city").cloned().unwrap_or(Value::Null), "sky": "sunny"}),
                )
            }
        },
    ))
}

async fn run_agent(
    model: Arc<dyn Llm>,
    streaming: bool,
    budget: Option<Arc<BudgetTracker>>,
    prompt: &str,
) -> anyhow::Result<RunOutcome> {
    let runs = Arc::new(AtomicUsize::new(0));
    let agent = LlmAgentBuilder::new(AGENT)
        .description("Answers weather questions")
        .instruction("Answer weather questions. Always call get_weather once, then answer in one short sentence.")
        .model(model)
        .tool(weather_tool(runs.clone()))
        .build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    let session_id = "session-1";
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let mut run_config = RunConfig::builder()
        .streaming_mode(if streaming { StreamingMode::SSE } else { StreamingMode::None })
        .build();
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
    match runner.run_str(USER, session_id, Content::new("user").with_text(prompt)).await {
        Ok(mut stream) => {
            while let Some(item) = stream.next().await {
                if let Err(err) = item {
                    error = Some(err.to_string());
                    break;
                }
            }
        }
        Err(err) => error = Some(err.to_string()),
    }
    let session = sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: session_id.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    Ok(RunOutcome { error, events: session.events().all(), tool_runs: runs.load(Ordering::SeqCst) })
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

/// The real client for `provider`, pointed at the scripted upstream; unary unless `stream`.
fn scripted_model(
    provider: Provider,
    base_url: &str,
    stream: bool,
) -> anyhow::Result<Arc<dyn Llm>> {
    let model = client(provider, base_url)?;
    Ok(if stream { model } else { Arc::new(NonStreaming(model)) })
}

fn client(provider: Provider, base_url: &str) -> anyhow::Result<Arc<dyn Llm>> {
    let model: Arc<dyn Llm> = match provider {
        Provider::OpenAiResponses => Arc::new(
            OpenAIResponsesClient::new(
                OpenAIResponsesConfig::new("scripted-key", "gpt-5.6-terra").with_base_url(base_url),
            )?
            .with_retry_config(RetryConfig::disabled()),
        ),
        Provider::Anthropic => Arc::new(
            AnthropicClient::new(
                AnthropicConfig::new("scripted-key", "claude-sonnet-4-6").with_base_url(base_url),
            )?
            .with_retry_config(RetryConfig::disabled()),
        ),
        Provider::Gemini => Arc::new(
            GeminiModel::new_with_base_url(
                "scripted-key",
                "gemini-3.7-flash",
                format!("{base_url}/v1beta/"),
            )?
            .with_retry_config(RetryConfig::disabled()),
        ),
        Provider::GeminiInteractions => Arc::new(
            GeminiModel::new_with_base_url(
                "scripted-key",
                "gemini-3.7-flash",
                format!("{base_url}/v1beta/"),
            )?
            .with_retry_config(RetryConfig::disabled())
            .use_interactions_api(true)?,
        ),
        Provider::DeepSeek => Arc::new(
            DeepSeekClient::new(
                DeepSeekConfig::new("scripted-key", "deepseek-chat").with_base_url(base_url),
            )?
            .with_retry_config(RetryConfig::disabled()),
        ),
        Provider::Groq => Arc::new(
            GroqClient::new(
                GroqConfig::new("scripted-key", "openai/gpt-oss-20b").with_base_url(base_url),
            )?
            .with_retry_config(RetryConfig::disabled()),
        ),
        Provider::Ollama => Arc::new(OllamaModel::new(OllamaConfig::with_host(base_url, "qwen3"))?),
    };
    Ok(model)
}

/// The usage checks for one scripted turn.
fn usage_checks(
    outcome: &RunOutcome,
    provider: Provider,
    stream: bool,
    checks: &mut Vec<(bool, String)>,
) {
    let usages = outcome.usages();
    checks.push((
        usages.len() == 2,
        format!("one usage report per model call (got {})", usages.len()),
    ));
    for (turn, usage) in usages.iter().enumerate().take(2) {
        let want = expected(provider, turn, stream);
        let got = (usage.prompt_token_count, usage.candidates_token_count, usage.total_token_count);
        checks.push((
            got == (want.prompt, want.candidates, want.total),
            format!(
                "call {turn}: prompt/candidates/total = {got:?} (want {:?})",
                (want.prompt, want.candidates, want.total)
            ),
        ));
        checks.push((
            usage.provider_usage.as_ref() == Some(&want.provider_usage),
            format!(
                "call {turn}: provider_usage = {} (want {})",
                usage.provider_usage.as_ref().map_or("none".to_string(), Value::to_string),
                want.provider_usage
            ),
        ));
    }
}

async fn scripted(provider: Provider, stream: bool) -> anyhow::Result<Verdict> {
    let upstream = Upstream::start(provider, stream).await?;
    let model = scripted_model(provider, &upstream.base_url, stream)?;
    let outcome = run_agent(model, stream, None, "What's the weather in Paris?").await?;
    let mut checks = vec![
        (
            outcome.error.is_none(),
            format!("run succeeds (error: {})", outcome.error.as_deref().unwrap_or("none")),
        ),
        (outcome.tool_runs == 1, format!("get_weather ran {} time(s), want 1", outcome.tool_runs)),
        (outcome.final_text() == ANSWER, format!("final answer {:?}", outcome.final_text())),
        (
            upstream.requests().len() == 2,
            format!("{} model request(s), want 2", upstream.requests().len()),
        ),
        (
            upstream.requests().iter().all(|(path, body)| {
                let streamed = path.contains("streamGenerateContent")
                    || body.get("stream").and_then(Value::as_bool).unwrap_or(false);
                streamed == stream
            }),
            format!("every request {} streaming", if stream { "asked for" } else { "declined" }),
        ),
    ];
    usage_checks(&outcome, provider, stream, &mut checks);
    Ok(verdict(checks))
}

/// A run budget with a token cap counts the usage the provider reported: Groq's on the
/// streamed tool-call finish chunk, and Ollama's from a final line without
/// `prompt_eval_count`. A token cap fails closed on a call with no usage, so uncounted usage
/// stops the run.
async fn budget(provider: Provider, stream: bool) -> anyhow::Result<Verdict> {
    let upstream = Upstream::start(provider, stream).await?;
    let model = scripted_model(provider, &upstream.base_url, stream)?;
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_total_tokens(1_000)));
    let outcome =
        run_agent(model, stream, Some(tracker.clone()), "What's the weather in Paris?").await?;
    let spent = tracker.usage();
    let want =
        u64::try_from(expected(provider, 0, stream).total + expected(provider, 1, stream).total)?;
    Ok(verdict(vec![
        (
            outcome.error.is_none(),
            format!("run succeeds (error: {})", outcome.error.as_deref().unwrap_or("none")),
        ),
        (
            tracker.exceeded().is_none(),
            format!(
                "budget not exceeded ({})",
                tracker.exceeded().map_or("ok".to_string(), |e| e.message().to_string())
            ),
        ),
        (outcome.tool_runs == 1, format!("get_weather ran {} time(s), want 1", outcome.tool_runs)),
        (
            spent.model_calls == 2,
            format!("budget counted {} model call(s), want 2", spent.model_calls),
        ),
        (
            spent.tool_calls == 1,
            format!("budget counted {} tool call(s), want 1", spent.tool_calls),
        ),
        (
            spent.total_tokens == want,
            format!("budget counted {} token(s), want {want}", spent.total_tokens),
        ),
    ]))
}

/// A live run whose `provider_usage` must be the provider's own usage object: present, and
/// carrying `required` plus at least one field the typed client does not model.
async fn live(
    key_var: &str,
    model: impl FnOnce(String) -> anyhow::Result<Arc<dyn Llm>>,
    required: &str,
    typed_fields: &[&str],
) -> anyhow::Result<Verdict> {
    let Ok(key) = std::env::var(key_var) else {
        return Ok(Verdict::Skip(format!("{key_var} is not set")));
    };
    let outcome =
        run_agent(model(key)?, true, None, "What's the weather in Paris? Use the tool.").await?;
    let usages = outcome.usages();
    let raw = usages.iter().filter_map(|usage| usage.provider_usage.as_ref()).collect::<Vec<_>>();
    let untyped: Vec<String> = raw
        .iter()
        .filter_map(|usage| usage.as_object())
        .flat_map(|object| object.keys())
        .filter(|key| !typed_fields.contains(&key.as_str()))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut checks = vec![
        (
            outcome.error.is_none(),
            format!("run succeeds (error: {})", outcome.error.as_deref().unwrap_or("none")),
        ),
        (outcome.tool_runs >= 1, format!("get_weather ran {} time(s)", outcome.tool_runs)),
        (
            !usages.is_empty() && raw.len() == usages.len(),
            format!("{} of {} usage report(s) carry provider_usage", raw.len(), usages.len()),
        ),
        (
            raw.iter().all(|usage| usage.get(required).is_some()),
            format!("every provider_usage has `{required}`"),
        ),
    ];
    if !typed_fields.is_empty() {
        checks.push((
            !untyped.is_empty(),
            format!("provider_usage keeps fields the typed client does not model: {untyped:?}"),
        ));
    }
    Ok(verdict(checks))
}

const SCENARIOS: &[&str] = &[
    "openai_responses_unary",
    "openai_responses_stream",
    "anthropic_unary",
    "anthropic_stream",
    "gemini_unary",
    "gemini_stream",
    "gemini_interactions_unary",
    "gemini_interactions_stream",
    "deepseek_unary",
    "deepseek_stream",
    "groq_unary",
    "groq_stream",
    "ollama_unary",
    "budget_groq_stream",
    "budget_ollama_unary",
    "live_openai_responses",
    "live_anthropic",
    "live_gemini",
];

async fn run_scenario(name: &str) -> anyhow::Result<Verdict> {
    match name {
        "openai_responses_unary" => scripted(Provider::OpenAiResponses, false).await,
        "openai_responses_stream" => scripted(Provider::OpenAiResponses, true).await,
        "anthropic_unary" => scripted(Provider::Anthropic, false).await,
        "anthropic_stream" => scripted(Provider::Anthropic, true).await,
        "gemini_unary" => scripted(Provider::Gemini, false).await,
        "gemini_stream" => scripted(Provider::Gemini, true).await,
        "gemini_interactions_unary" => scripted(Provider::GeminiInteractions, false).await,
        "gemini_interactions_stream" => scripted(Provider::GeminiInteractions, true).await,
        "deepseek_unary" => scripted(Provider::DeepSeek, false).await,
        "deepseek_stream" => scripted(Provider::DeepSeek, true).await,
        "groq_unary" => scripted(Provider::Groq, false).await,
        "groq_stream" => scripted(Provider::Groq, true).await,
        "ollama_unary" => scripted(Provider::Ollama, false).await,
        "budget_groq_stream" => budget(Provider::Groq, true).await,
        "budget_ollama_unary" => budget(Provider::Ollama, false).await,
        "live_openai_responses" => {
            let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.6-terra".into());
            live(
                "OPENAI_API_KEY",
                |key| {
                    Ok(Arc::new(OpenAIResponsesClient::new(OpenAIResponsesConfig::new(
                        key, model,
                    ))?))
                },
                "input_tokens_details",
                &[],
            )
            .await
        }
        "live_anthropic" => {
            let model =
                std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-sonnet-4-6".into());
            live(
                "ANTHROPIC_API_KEY",
                |key| Ok(Arc::new(AnthropicClient::new(AnthropicConfig::new(key, model))?)),
                "input_tokens",
                &[
                    "input_tokens",
                    "output_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                    "server_tool_use",
                    "cache_creation_input_tokens_1h",
                ],
            )
            .await
        }
        "live_gemini" => {
            let model = std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-3.7-flash".into());
            live(
                "GEMINI_API_KEY",
                |key| Ok(Arc::new(GeminiModel::new(key, model)?)),
                "promptTokenCount",
                &[
                    "promptTokenCount",
                    "candidatesTokenCount",
                    "totalTokenCount",
                    "thoughtsTokenCount",
                    "promptTokensDetails",
                    "cachedContentTokenCount",
                    "cacheTokensDetails",
                ],
            )
            .await
        }
        other => anyhow::bail!("unknown scenario {other}; known: {}", SCENARIOS.join(", ")),
    }
}

fn load_env() {
    // The repository's `.env` wins over stale keys exported in the shell.
    let env_file = std::env::var("ADK_ENV_FILE").unwrap_or_else(|_| ".env".to_string());
    let _ = dotenvy::from_path_override(&env_file);
    let _ = dotenvy::dotenv();
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

    println!("usage accounting — {} scenario(s)\n", selected.len());
    let mut failures = 0;
    for name in &selected {
        let result = tokio::time::timeout(Duration::from_secs(180), run_scenario(name)).await;
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
                ("FAIL", "timed out after 180s".into())
            }
        };
        println!("{status}  {name}\n      {evidence}");
    }
    println!("\n{} scenario(s), {failures} failed", selected.len());
    if failures == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
