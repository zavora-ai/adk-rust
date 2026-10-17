//! What a `#[tool]` tool's schema promises the model, and whether the agent enforces it.
//!
//! Every scenario drives the real stack — `LlmAgentBuilder`, `Runner`, `InMemorySessionService`,
//! and the provider clients `OpenAIClient` and `GeminiModel` — and asserts on what reached the
//! model, what reached the wire, and how often the tool ran, never on model prose.
//!
//! Deterministic bad-argument turns come from a scripted OpenAI-compatible server on 127.0.0.1,
//! and the Gemini wire schema is captured by a scripted Gemini server; the real clients are
//! pointed at both through their base URLs. The `live_*` scenarios run against OpenAI and Gemini
//! when their keys are set.
//!
//! ```bash
//! cargo run --manifest-path examples/tool_schemas/Cargo.toml
//! cargo run --manifest-path examples/tool_schemas/Cargo.toml -- --scenario unknown_argument
//! ```

mod doubles;
mod gemini_upstream;
mod openai_upstream;
mod tools;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Content, Event, Llm, Part, RunConfig, SchemaAdapter, StreamingMode, Tool};
use adk_gemini::GeminiSchemaAdapter;
use adk_model::GeminiModel;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use gemini_upstream::GeminiUpstream;
use openai_upstream::{OpenAiUpstream, Reply, calls, tool_message};
use serde_json::{Value, json};
use tools::{BookSlots, bookings, reset_bookings};

const APP: &str = "tool-schemas";
const USER: &str = "user";
const OPENAI_MODEL: &str = "gpt-5.6-terra";
const GEMINI_MODEL: &str = "gemini-3.5-flash-lite";
const CONFIRM_TO_DESCRIPTION: &str = "Where to post the booking confirmation.";

pub enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Each check is `(holds, what it asserts or observed)`; the evidence marks the ones that fail.
pub fn verdict(checks: Vec<(bool, String)>) -> Verdict {
    let all_hold = checks.iter().all(|(holds, _)| *holds);
    let evidence = checks
        .into_iter()
        .map(|(holds, message)| if holds { message } else { format!("[FAILED] {message}") })
        .collect::<Vec<_>>()
        .join("\n      ");
    if all_hold { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) }
}

pub struct RunOutcome {
    pub error: Option<String>,
    pub session_events: Vec<Event>,
}

impl RunOutcome {
    pub fn final_text(&self) -> String {
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

    fn function_responses(&self) -> Vec<Value> {
        self.session_events
            .iter()
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, .. } => {
                    Some(function_response.response.clone())
                }
                _ => None,
            })
            .collect()
    }
}

pub async fn run_agent(agent: LlmAgent, prompt: &str) -> anyhow::Result<RunOutcome> {
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
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .run_config(RunConfig::builder().streaming_mode(StreamingMode::SSE).build())
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
    Ok(RunOutcome { error, session_events: session.events().all() })
}

fn booker(model: Arc<dyn Llm>) -> anyhow::Result<LlmAgent> {
    Ok(LlmAgentBuilder::new("booker")
        .model(model)
        .instruction(
            "You book meeting rooms with the book_slots tool. Call it once with the details the \
             user gives. If the tool reports invalid arguments, fix them and call it again. Then \
             confirm the booking in one short sentence.",
        )
        .tool(Arc::new(BookSlots))
        .build()?)
}

const VALID_ARGS: &str =
    r#"{"room":"A1","count":2,"confirm_to":{"street":"1 Main St","city":"Nairobi"}}"#;

/// Two calls whose arguments carry keys the schema does not declare — one at the top level, one
/// inside `confirm_to` — then the corrected call.
async fn unknown_argument() -> anyhow::Result<Verdict> {
    reset_bookings();
    let upstream = OpenAiUpstream::start(Box::new(|turn| match turn {
        0 => calls(&[
            (
                "call_top",
                "book_slots",
                r#"{"room":"A1","count":2,"confirm_to":{"street":"1 Main St","city":"Nairobi"},"dry_run":true}"#,
            ),
            (
                "call_nested",
                "book_slots",
                r#"{"room":"A1","count":2,"confirm_to":{"street":"1 Main St","city":"Nairobi","zip":"00100"}}"#,
            ),
        ]),
        1 => calls(&[("call_fixed", "book_slots", VALID_ARGS)]),
        _ => Reply::Text("Booked room A1.".to_string()),
    }))
    .await?;
    let model = Arc::new(OpenAIClient::compatible("scripted-key", &upstream.base_url, "scripted")?);
    let outcome =
        run_agent(booker(model)?, "Book two slots in A1, confirmation to Nairobi.").await?;

    let requests = upstream.requests();
    let answer = |id: &str| {
        requests.iter().find_map(|request| tool_message(request, id)).unwrap_or_default()
    };
    let (top, nested, fixed) = (answer("call_top"), answer("call_nested"), answer("call_fixed"));
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (
            top.contains("invalid") && top.contains("dry_run"),
            format!("model was told about `dry_run`: {top}"),
        ),
        (
            nested.contains("invalid") && nested.contains("zip"),
            format!("model was told about `confirm_to.zip`: {nested}"),
        ),
        (fixed.contains("\"booked\":true"), format!("corrected call answered: {fixed}")),
        (
            bookings().len() == 1,
            format!("book_slots ran {} time(s): {:?}", bookings().len(), bookings()),
        ),
    ]))
}

/// A call whose `count` is above the schema's maximum and whose `room` breaks its pattern, then
/// the corrected call.
async fn out_of_range_argument() -> anyhow::Result<Verdict> {
    reset_bookings();
    let upstream = OpenAiUpstream::start(Box::new(|turn| match turn {
        0 => calls(&[(
            "call_bad",
            "book_slots",
            r#"{"room":"lobby","count":50,"confirm_to":{"street":"1 Main St","city":"Nairobi"}}"#,
        )]),
        1 => calls(&[("call_fixed", "book_slots", VALID_ARGS)]),
        _ => Reply::Text("Booked room A1.".to_string()),
    }))
    .await?;
    let model = Arc::new(OpenAIClient::compatible("scripted-key", &upstream.base_url, "scripted")?);
    let outcome = run_agent(booker(model)?, "Book fifty slots in the lobby.").await?;

    let requests = upstream.requests();
    let answer = |id: &str| {
        requests.iter().find_map(|request| tool_message(request, id)).unwrap_or_default()
    };
    let (bad, fixed) = (answer("call_bad"), answer("call_fixed"));
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (
            bad.contains("invalid") && bad.contains("/count") && bad.contains("10"),
            format!("model was told `count` exceeds 10: {bad}"),
        ),
        (bad.contains("/room"), "model was told `room` breaks its pattern".to_string()),
        (fixed.contains("\"booked\":true"), format!("corrected call answered: {fixed}")),
        (
            bookings()
                == vec![
                    json!({"room": "A1", "count": 2, "street": "1 Main St", "city": "Nairobi", "attendees": 0}),
                ],
            format!("book_slots ran with: {:?}", bookings()),
        ),
    ]))
}

/// The function declaration `GeminiModel` sends, captured by a scripted Gemini server.
async fn gemini_wire_schema() -> anyhow::Result<Verdict> {
    let upstream = GeminiUpstream::start().await?;
    let model =
        Arc::new(GeminiModel::new_with_base_url("scripted-key", GEMINI_MODEL, &upstream.base_url)?);
    let outcome = run_agent(booker(model)?, "Hello.").await?;
    let parameters = upstream
        .requests()
        .first()
        .and_then(|request| gemini_upstream::declared_parameters(request, "book_slots"))
        .unwrap_or_default();
    let mut checks = vec![(outcome.error.is_none(), format!("run error: {:?}", outcome.error))];
    checks.extend(schema_checks(&parameters));
    Ok(verdict(checks))
}

/// What the Gemini function declaration must keep from the `#[tool]` schema.
fn schema_checks(parameters: &Value) -> Vec<(bool, String)> {
    let properties = &parameters["properties"];
    vec![
        (
            properties["confirm_to"]["description"] == CONFIRM_TO_DESCRIPTION,
            format!("confirm_to description: {}", properties["confirm_to"]["description"]),
        ),
        (
            properties["confirm_to"]["properties"]["city"]["type"] == "string",
            "confirm_to is inlined with its fields".to_string(),
        ),
        (
            properties["count"]["minimum"] == json!(1)
                && properties["count"]["maximum"] == json!(10),
            format!(
                "count bounds: minimum {} maximum {}",
                properties["count"]["minimum"], properties["count"]["maximum"]
            ),
        ),
        (
            properties["room"]["minLength"] == json!(2)
                && properties["room"]["maxLength"] == json!(4)
                && properties["room"]["pattern"].is_string(),
            format!(
                "room bounds: minLength {} maxLength {} pattern {}",
                properties["room"]["minLength"],
                properties["room"]["maxLength"],
                properties["room"]["pattern"]
            ),
        ),
        (
            properties["attendees"]["maxItems"] == json!(3),
            format!("attendees maxItems: {}", properties["attendees"]["maxItems"]),
        ),
    ]
}

/// Two different tools registered under one name.
async fn duplicate_tool_names() -> anyhow::Result<Verdict> {
    let impostor =
        FunctionTool::new("book_slots", "Pretends to book rooms.", |_ctx, _args| async {
            Ok(json!({ "booked": false }))
        });
    let model = Arc::new(OpenAIClient::compatible("scripted-key", "http://127.0.0.1:9/v1", "m")?);
    let built = LlmAgentBuilder::new("booker")
        .model(model)
        .tool(Arc::new(BookSlots))
        .tool(Arc::new(impostor))
        .build();
    let error = built.err().map(|err| err.to_string()).unwrap_or_default();
    Ok(verdict(vec![(
        error.contains("book_slots"),
        format!(
            "build() result: {}",
            if error.is_empty() { "Ok(agent)".to_string() } else { error }
        ),
    )]))
}

fn env_key(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| std::env::var(name).ok().filter(|key| !key.trim().is_empty()))
}

fn live_checks(outcome: &RunOutcome) -> Vec<(bool, String)> {
    let booked = bookings();
    vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (!booked.is_empty(), format!("book_slots ran with: {booked:?}")),
        (
            booked.iter().all(|booking| (1..=10).contains(&booking["count"].as_u64().unwrap_or(0))),
            "every call that ran had count within 1..=10".to_string(),
        ),
        (
            outcome.function_responses().iter().any(|response| response["booked"] == json!(true)),
            "a booking succeeded".to_string(),
        ),
    ]
}

const LIVE_PROMPT: &str =
    "Book 3 slots in room B2 and post the confirmation to 5 River Rd, Mombasa.";

/// OpenAI accepts the `#[tool]` schema with its bounds and a happy-path call runs.
async fn live_openai_schema() -> anyhow::Result<Verdict> {
    let Some(key) = env_key(&["OPENAI_API_KEY"]) else {
        return Ok(Verdict::Skip("OPENAI_API_KEY not set".to_string()));
    };
    reset_bookings();
    let model = Arc::new(OpenAIClient::new(OpenAIConfig::new(key, OPENAI_MODEL))?);
    let outcome = run_agent(booker(model)?, LIVE_PROMPT).await?;
    Ok(verdict(live_checks(&outcome)))
}

/// Gemini accepts the declaration with the `$ref` sibling description and the bounds kept.
async fn live_gemini_schema() -> anyhow::Result<Verdict> {
    let Some(key) = env_key(&["GOOGLE_API_KEY", "GEMINI_API_KEY"]) else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY / GEMINI_API_KEY not set".to_string()));
    };
    reset_bookings();
    let model = Arc::new(GeminiModel::new(key, GEMINI_MODEL)?);
    let outcome = run_agent(booker(model)?, LIVE_PROMPT).await?;
    // The same reduction `GeminiModel` applies before the request leaves the process.
    let sent = GeminiSchemaAdapter::new()
        .normalize_schema(BookSlots.parameters_schema().unwrap_or_default());
    let mut checks = live_checks(&outcome);
    checks.extend(schema_checks(&sent));
    Ok(verdict(checks))
}

fn load_env() {
    let env_file = std::env::var("ADK_ENV_FILE")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env").to_string());
    let path = std::fs::canonicalize(&env_file).unwrap_or_else(|_| env_file.into());
    // Override, so stale keys exported by the shell do not shadow the file.
    let _ = dotenvy::from_path_override(path);
}

#[tokio::main]
async fn main() -> ExitCode {
    adk_core::ensure_crypto_provider();
    load_env();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "error".into()),
        )
        .try_init();

    let mut selected: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--scenario"
            && let Some(value) = args.next()
        {
            selected = value.split(',').map(str::to_string).collect();
        }
    }

    type Scenario = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Verdict>>>>;
    type Entry = (&'static str, fn() -> Scenario);
    let scenarios: Vec<Entry> = vec![
        ("unknown_argument", || Box::pin(unknown_argument())),
        ("out_of_range_argument", || Box::pin(out_of_range_argument())),
        ("gemini_wire_schema", || Box::pin(gemini_wire_schema())),
        ("duplicate_tool_names", || Box::pin(duplicate_tool_names())),
        ("scripted_llm_records_requests", || Box::pin(doubles::scripted_llm_records_requests())),
        ("live_openai_schema", || Box::pin(live_openai_schema())),
        ("live_gemini_schema", || Box::pin(live_gemini_schema())),
    ];

    let mut failed = 0;
    for (name, scenario) in scenarios {
        if !selected.is_empty() && !selected.iter().any(|s| s == name) {
            continue;
        }
        match scenario().await {
            Ok(Verdict::Pass(evidence)) => println!("PASS {name}\n      {evidence}"),
            Ok(Verdict::Skip(reason)) => println!("SKIP {name}: {reason}"),
            Ok(Verdict::Fail(evidence)) => {
                failed += 1;
                println!("FAIL {name}\n      {evidence}");
            }
            Err(err) => {
                failed += 1;
                println!("FAIL {name}\n      error: {err:#}");
            }
        }
    }
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
