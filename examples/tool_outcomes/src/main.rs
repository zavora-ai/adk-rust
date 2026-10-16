//! What an `LlmAgent` tells the model about a tool call's outcome.
//!
//! Every scenario drives the real stack — `LlmAgentBuilder`, `Runner`, `InMemorySessionService`,
//! and `OpenAIClient` — and asserts on what reached the model and the session, and how often each
//! tool ran. Faults a live provider cannot be made to produce (malformed tool-call JSON) come from
//! a scripted OpenAI-compatible server on 127.0.0.1 that the real client is pointed at. The live
//! scenarios run when `OPENAI_API_KEY` is set.
//!
//! ```bash
//! cargo run --manifest-path examples/tool_outcomes/Cargo.toml
//! cargo run --manifest-path examples/tool_outcomes/Cargo.toml -- --scenario malformed_args_stream
//! ```

mod scripted;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{
    ActionLedger, ActionOutcome, AdkError, BeforeModelResult, CallbackContext, Content,
    ErrorCategory, ErrorComponent, Event, InMemoryActionLedger, Llm, LlmRequest, Part, RunConfig,
    StreamingMode, Tool, ToolContext, ToolEffect,
};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_plugin::{
    AfterToolCallResult, EnhancedPlugin, Plugin, PluginConfig, PluginContext, PluginManager,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use async_trait::async_trait;
use futures::StreamExt;
use scripted::{Reply, ScriptedServer, call, last_tool_message, tool_message};
use serde_json::{Value, json};

const APP: &str = "tool-outcomes";
const USER: &str = "user";
const SECRET: &str = "hunter2";
const SECRET_HOST: &str = "db.internal";
const LIVE_MODEL: &str = "gpt-5.6-terra";

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
    session_events: Vec<Event>,
}

impl RunOutcome {
    fn session_json(&self) -> String {
        serde_json::to_string(&self.session_events).unwrap_or_default()
    }

    fn function_responses(&self) -> Vec<(Option<String>, Value)> {
        self.session_events
            .iter()
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, id, .. } => {
                    Some((id.clone(), function_response.response.clone()))
                }
                _ => None,
            })
            .collect()
    }

    fn final_text(&self) -> String {
        self.session_events
            .iter()
            .rev()
            .filter(|event| event.author != "user")
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(Part::text)
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

struct Setup {
    plugin: Option<Plugin>,
    ledger: Option<Arc<InMemoryActionLedger>>,
    streaming: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self { plugin: None, ledger: None, streaming: true }
    }
}

async fn run_agent(agent: LlmAgent, setup: Setup, prompt: &str) -> anyhow::Result<RunOutcome> {
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
    let mut run_config = RunConfig::builder().streaming_mode(if setup.streaming {
        StreamingMode::SSE
    } else {
        StreamingMode::None
    });
    if let Some(ledger) = setup.ledger {
        run_config = run_config.action_ledger(ledger as Arc<dyn ActionLedger>);
    }
    let mut builder = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .run_config(run_config.build());
    if let Some(plugin) = setup.plugin {
        builder = builder.plugin_manager(Arc::new(PluginManager::new(vec![plugin])));
    }
    let runner = builder.build()?;

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
    Ok(RunOutcome { error, session_events: session.events().all() })
}

fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OpenAIClient::compatible("scripted-key", &server.base_url, "scripted-model")?))
}

/// A non-idempotent charge tool that counts how often it runs.
fn charge_tool(charges: Arc<AtomicUsize>) -> Arc<dyn Tool> {
    Arc::new(
        FunctionTool::new(
            "charge_card",
            "Charges the customer's card on file for an order. Arguments: order_id (string), amount_cents (integer).",
            move |_ctx: Arc<dyn ToolContext>, args: Value| {
                let charges = charges.clone();
                async move {
                    let n = charges.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok(json!({
                        "charged": true,
                        "charge_id": format!("ch_{n}"),
                        "order_id": args.get("order_id").cloned().unwrap_or(Value::Null),
                    }))
                }
            },
        )
        .with_effect(ToolEffect::NonIdempotent),
    )
}

fn audit_error() -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Unavailable,
        "audit.sink_unavailable",
        "audit sink https://audit:s3cr3t-token@audit.internal/v1 unavailable",
    )
}

/// A model that retries a call whose response does not tell it the call already ran, as a real
/// model does when it reads an error.
fn retrying_charge_script() -> scripted::Script {
    Box::new(|request, turn| {
        if turn == 0 {
            return call("call_1", "charge_card", r#"{"order_id":"ORD-1","amount_cents":500}"#);
        }
        let last = last_tool_message(request).unwrap_or_default().to_lowercase();
        let told_it_ran = last.contains("ran") && last.contains("do not call it again");
        let succeeded = last.contains("\"charged\":true");
        if told_it_ran || succeeded || turn >= 3 {
            Reply::Text("The order is paid.".into())
        } else {
            call(
                &format!("call_{}", turn + 1),
                "charge_card",
                r#"{"order_id":"ORD-1","amount_cents":500}"#,
            )
        }
    })
}

#[derive(Clone, Copy)]
enum AfterStage {
    Callback,
    CallbackFull,
    RunnerPlugin,
    EnhancedPlugin,
}

struct FailingAuditPlugin;

#[async_trait]
impl EnhancedPlugin for FailingAuditPlugin {
    fn name(&self) -> &str {
        "audit"
    }

    async fn after_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        _args: &Value,
        _result: Value,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> adk_core::Result<AfterToolCallResult> {
        Err(audit_error())
    }
}

async fn after_stage_failure(stage: AfterStage) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(retrying_charge_script()).await?;
    let charges = Arc::new(AtomicUsize::new(0));
    let mut builder = LlmAgentBuilder::new("payments")
        .model(scripted_model(&server)?)
        .tool(charge_tool(charges.clone()));
    let mut setup = Setup::default();
    match stage {
        AfterStage::Callback => {
            builder = builder
                .after_tool_callback(Box::new(|_ctx| Box::pin(async move { Err(audit_error()) })));
        }
        AfterStage::CallbackFull => {
            builder = builder.after_tool_callback_full(Box::new(|_ctx, _tool, _args, _resp| {
                Box::pin(async move { Err(audit_error()) })
            }));
        }
        AfterStage::RunnerPlugin => {
            setup.plugin = Some(Plugin::new(PluginConfig {
                name: "audit".into(),
                after_tool: Some(Box::new(|_ctx| Box::pin(async move { Err(audit_error()) }))),
                ..Default::default()
            }));
        }
        AfterStage::EnhancedPlugin => {
            builder = builder.enhanced_plugin(Arc::new(FailingAuditPlugin));
        }
    }
    let ledger = Arc::new(InMemoryActionLedger::new());
    setup.ledger = Some(ledger.clone());
    let outcome = run_agent(builder.build()?, setup, "Charge order ORD-1 5.00 USD.").await?;

    let requests = server.requests();
    let first_answer =
        requests.get(1).and_then(|request| tool_message(request, "call_1")).unwrap_or_default();
    let all_requests = serde_json::to_string(&requests)?;
    let ran = charges.load(Ordering::SeqCst);
    let completed = first_call_ledger_outcome(&ledger, &outcome).await;
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (ran == 1, format!("charge_card ran {ran} time(s)")),
        (
            first_answer.contains("ran") && first_answer.contains("do not call it again"),
            format!("model was told: {first_answer}"),
        ),
        (
            !all_requests.contains("s3cr3t-token")
                && !outcome.session_json().contains("s3cr3t-token"),
            "callback error detail kept out of model requests and session".into(),
        ),
        (
            matches!(completed, Some(ActionOutcome::Succeeded { .. })),
            format!("ledger outcome for call_1: {completed:?}"),
        ),
    ]))
}

/// The ledger record for the first function response's call.
async fn first_call_ledger_outcome(
    ledger: &InMemoryActionLedger,
    outcome: &RunOutcome,
) -> Option<ActionOutcome> {
    let invocation = outcome
        .session_events
        .iter()
        .find(|event| event.author != "user")
        .map(|event| event.invocation_id.clone())?;
    let key = format!("{APP}/{USER}/session-1/{invocation}/call_1");
    ledger.get(&key).await.ok().flatten().and_then(|record| record.outcome)
}

fn leaky_lookup_tool(calls: Arc<AtomicUsize>) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        "lookup_order",
        "Looks up an order by id. Arguments: order_id (string).",
        move |_ctx: Arc<dyn ToolContext>, _args: Value| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<Value, _>(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unavailable,
                    "tool.orders_db.unavailable",
                    format!(
                        "connect postgres://admin:{SECRET}@{SECRET_HOST}:5432/prod failed: connection refused"
                    ),
                ))
            }
        },
    ))
}

async fn tool_error_redacted() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_request, turn| {
        if turn == 0 {
            call("call_1", "lookup_order", r#"{"order_id":"ORD-7"}"#)
        } else {
            Reply::Text("The order database is unavailable.".into())
        }
    }))
    .await?;
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = LlmAgentBuilder::new("support")
        .model(scripted_model(&server)?)
        .tool(leaky_lookup_tool(calls.clone()))
        .build()?;
    let outcome = run_agent(agent, Setup::default(), "Where is order ORD-7?").await?;
    let requests = serde_json::to_string(&server.requests())?;
    let answer =
        server.requests().get(1).and_then(|r| tool_message(r, "call_1")).unwrap_or_default();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (
            !requests.contains(SECRET) && !requests.contains(SECRET_HOST),
            "credential and host kept out of every model request".into(),
        ),
        (!outcome.session_json().contains(SECRET), "credential kept out of the session".into()),
        (
            answer.contains("error") && answer.contains("unavailable"),
            format!("model was told: {answer}"),
        ),
    ]))
}

async fn exposed_error_messages_reach_model() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_request, turn| {
        if turn == 0 {
            call("call_1", "lookup_order", r#"{"order_id":"ORD-7"}"#)
        } else {
            Reply::Text("The order database is unavailable.".into())
        }
    }))
    .await?;
    let agent = LlmAgentBuilder::new("support")
        .model(scripted_model(&server)?)
        .tool(leaky_lookup_tool(Arc::new(AtomicUsize::new(0))))
        .expose_tool_error_messages(true)
        .build()?;
    let outcome = run_agent(agent, Setup::default(), "Where is order ORD-7?").await?;
    let answer =
        server.requests().get(1).and_then(|r| tool_message(r, "call_1")).unwrap_or_default();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (answer.contains("connection refused"), format!("model was told: {answer}")),
    ]))
}

async fn on_tool_error_text_reaches_model() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_request, turn| {
        if turn == 0 {
            call("call_1", "lookup_order", r#"{"order_id":"ORD-7"}"#)
        } else {
            Reply::Text("Try again later.".into())
        }
    }))
    .await?;
    let agent = LlmAgentBuilder::new("support")
        .model(scripted_model(&server)?)
        .tool(leaky_lookup_tool(Arc::new(AtomicUsize::new(0))))
        .on_tool_error(Box::new(|_ctx, _tool, _args, _error| {
            Box::pin(async move { Ok(Some(json!({"error": "orders are offline until 14:00"}))) })
        }))
        .build()?;
    let outcome = run_agent(agent, Setup::default(), "Where is order ORD-7?").await?;
    let answer =
        server.requests().get(1).and_then(|r| tool_message(r, "call_1")).unwrap_or_default();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (answer.contains("offline until 14:00"), format!("model was told: {answer}")),
    ]))
}

async fn sub_second_timeout() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_request, turn| {
        if turn == 0 {
            call("call_1", "slow_report", "{}")
        } else {
            Reply::Text("The report timed out.".into())
        }
    }))
    .await?;
    let agent = LlmAgentBuilder::new("reports")
        .model(scripted_model(&server)?)
        .tool(Arc::new(
            FunctionTool::new(
                "slow_report",
                "Builds a report.",
                |_ctx: Arc<dyn ToolContext>, _args: Value| async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    Ok(json!({"report": "done"}))
                },
            )
            .with_effect(ToolEffect::ReadOnly),
        ))
        .tool_timeout(Duration::from_millis(50))
        .build()?;
    let outcome = run_agent(agent, Setup::default(), "Build the report.").await?;
    let answer =
        server.requests().get(1).and_then(|r| tool_message(r, "call_1")).unwrap_or_default();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (
            answer.contains("50ms") && !answer.contains("0 seconds"),
            format!("model was told: {answer}"),
        ),
    ]))
}

async fn malformed_args(streaming: bool) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|request, turn| match turn {
        // Truncated mid-object, as a model cut off by its output limit sends it.
        0 => call("call_1", "get_weather", r#"{"city": "Nair"#),
        1 => {
            let answer = tool_message(request, "call_1").unwrap_or_default();
            if answer.contains("error") {
                call("call_2", "get_weather", r#"{"city":"Nairobi"}"#)
            } else {
                Reply::Text("unexpected answer to call_1".into())
            }
        }
        _ => Reply::Text("It is sunny in Nairobi.".into()),
    }))
    .await?;
    let seen = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let recorded = seen.clone();
    let agent = LlmAgentBuilder::new("weather")
        .model(scripted_model(&server)?)
        .tool(Arc::new(
            FunctionTool::new(
                "get_weather",
                "Gets the weather for a city. Arguments: city (string).",
                move |_ctx: Arc<dyn ToolContext>, args: Value| {
                    let recorded = recorded.clone();
                    async move {
                        recorded.lock().unwrap().push(args);
                        Ok(json!({"forecast": "sunny"}))
                    }
                },
            )
            .with_effect(ToolEffect::ReadOnly),
        ))
        .build()?;
    let outcome =
        run_agent(agent, Setup { streaming, ..Setup::default() }, "Weather in Nairobi?").await?;
    let requests = server.requests();
    let answer = requests.get(1).and_then(|r| tool_message(r, "call_1")).unwrap_or_default();
    let executed = seen.lock().unwrap().clone();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (requests.len() == 3, format!("{} model requests", requests.len())),
        (answer.contains("error"), format!("call_1 was answered: {answer}")),
        (
            executed == vec![json!({"city": "Nairobi"})],
            format!("get_weather executed with {executed:?}"),
        ),
        (outcome.final_text().contains("sunny"), format!("final text: {:?}", outcome.final_text())),
    ]))
}

fn live_model() -> anyhow::Result<Option<Arc<dyn Llm>>> {
    let Ok(key) = std::env::var("OPENAI_API_KEY") else { return Ok(None) };
    if key.trim().is_empty() {
        return Ok(None);
    }
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| LIVE_MODEL.to_string());
    Ok(Some(Arc::new(OpenAIClient::new(OpenAIConfig::new(key, model))?)))
}

/// Records every request the agent sends the model.
fn capture_requests(
    builder: LlmAgentBuilder,
    sink: Arc<std::sync::Mutex<Vec<LlmRequest>>>,
) -> LlmAgentBuilder {
    builder.before_model_callback(Box::new(move |_ctx, request| {
        let sink = sink.clone();
        Box::pin(async move {
            sink.lock().unwrap().push(request.clone());
            Ok(BeforeModelResult::Continue(request))
        })
    }))
}

async fn live_after_callback_failure() -> anyhow::Result<Verdict> {
    let Some(model) = live_model()? else {
        return Ok(Verdict::Skip("OPENAI_API_KEY not set".into()));
    };
    let charges = Arc::new(AtomicUsize::new(0));
    let agent = LlmAgentBuilder::new("payments")
        .instruction("You process payments with the charge_card tool, then confirm the outcome to the user in one sentence.")
        .model(model)
        .tool(charge_tool(charges.clone()))
        .after_tool_callback(Box::new(|_ctx| Box::pin(async move { Err(audit_error()) })))
        .max_iterations(6)
        .build()?;
    let setup = Setup { ledger: Some(Arc::new(InMemoryActionLedger::new())), ..Setup::default() };
    let outcome = run_agent(agent, setup, "Charge order ORD-42 for 500 cents.").await?;
    let ran = charges.load(Ordering::SeqCst);
    let responses = outcome.function_responses();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (ran == 1, format!("charge_card ran {ran} time(s) across the run")),
        (
            responses
                .first()
                .is_some_and(|(_, response)| response.to_string().contains("do not call it again")),
            format!(
                "first response: {}",
                responses.first().map(|(_, r)| r.to_string()).unwrap_or_default()
            ),
        ),
        (true, format!("final text: {:?}", outcome.final_text())),
    ]))
}

async fn live_tool_error_redacted() -> anyhow::Result<Verdict> {
    let Some(model) = live_model()? else {
        return Ok(Verdict::Skip("OPENAI_API_KEY not set".into()));
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = capture_requests(
        LlmAgentBuilder::new("support")
            .instruction("Look orders up with lookup_order. If the tool fails, quote its error message to the user verbatim.")
            .model(model)
            .tool(leaky_lookup_tool(calls.clone()))
            .max_iterations(4),
        sink.clone(),
    )
    .build()?;
    let outcome = run_agent(agent, Setup::default(), "Where is order ORD-7?").await?;
    let requests: Vec<LlmRequest> = sink.lock().unwrap().clone();
    let request_text = requests
        .iter()
        .map(|request| serde_json::to_string(&request.contents).unwrap_or_default())
        .collect::<String>();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (calls.load(Ordering::SeqCst) >= 1, "lookup_order ran".into()),
        (
            !request_text.contains(SECRET) && !request_text.contains(SECRET_HOST),
            format!("credential kept out of {} model request(s)", requests.len()),
        ),
        (!outcome.session_json().contains(SECRET), "credential kept out of the session".into()),
        (!outcome.final_text().contains(SECRET), format!("final text: {:?}", outcome.final_text())),
    ]))
}

const SCENARIOS: &[&str] = &[
    "after_callback_error",
    "after_callback_full_error",
    "runner_plugin_after_tool_error",
    "enhanced_plugin_after_tool_error",
    "tool_error_redacted",
    "expose_error_messages",
    "on_tool_error_text",
    "sub_second_timeout",
    "malformed_args_stream",
    "malformed_args_unary",
    "live_after_callback_error",
    "live_tool_error_redacted",
];

async fn run_scenario(name: &str) -> anyhow::Result<Verdict> {
    match name {
        "after_callback_error" => after_stage_failure(AfterStage::Callback).await,
        "after_callback_full_error" => after_stage_failure(AfterStage::CallbackFull).await,
        "runner_plugin_after_tool_error" => after_stage_failure(AfterStage::RunnerPlugin).await,
        "enhanced_plugin_after_tool_error" => after_stage_failure(AfterStage::EnhancedPlugin).await,
        "tool_error_redacted" => tool_error_redacted().await,
        "expose_error_messages" => exposed_error_messages_reach_model().await,
        "on_tool_error_text" => on_tool_error_text_reaches_model().await,
        "sub_second_timeout" => sub_second_timeout().await,
        "malformed_args_stream" => malformed_args(true).await,
        "malformed_args_unary" => malformed_args(false).await,
        "live_after_callback_error" => live_after_callback_failure().await,
        "live_tool_error_redacted" => live_tool_error_redacted().await,
        other => anyhow::bail!("unknown scenario {other}; known: {}", SCENARIOS.join(", ")),
    }
}

fn load_env() {
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

    println!("tool outcomes — {} scenario(s)\n", selected.len());
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
