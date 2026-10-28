//! Escalated payments resumed from the approval store, SSO decisions refused when the audit sink
//! fails, and typed artifact and session identifier errors.
//!
//! Every scenario drives the stack a deployment uses — `LlmAgentBuilder`, `Runner`, the session,
//! artifact, and approval services, `OpenAIClient`, the `adk-payments` toolset, and the SSO
//! `JwtValidator` — and asserts the behaviour a caller relies on. A scripted OpenAI-compatible
//! server on 127.0.0.1 plays the model, a local issuer on 127.0.0.1 serves the JWKS, and the
//! Redis checks run against `REDIS_URL`. The live scenario runs against OpenAI when
//! `OPENAI_API_KEY` is set.
//!
//! ```bash
//! cargo run --manifest-path examples/security_sessions_contracts/Cargo.toml
//! cargo run --manifest-path examples/security_sessions_contracts/Cargo.toml -- --scenario sso_audit_failure_refuses_access
//! ```

mod issuer;
mod jwt_claims;
mod merchant;
mod scripted;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_artifact::{ArtifactService, FileArtifactService, InMemoryArtifactService};
use adk_auth::sso::{ClaimsMapper, JwtValidator, SsoAccessControl};
use adk_auth::{
    AccessControl, AuditEvent, AuditSink, AuthError, InMemoryAuditSink, Permission, Role,
    ScopeGuard, StaticScopeResolver,
};
use adk_core::identity::{SessionId, UserId};
use adk_core::{
    AdkError, Agent, ApprovalScope, ApprovalStore, Content, ErrorCategory, ErrorComponent, Event,
    InMemoryApprovalStore, Llm, Part, RunConfig, Tool, ToolApproval, ToolConfirmationRequest,
    ToolContext,
};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_payments::auth::ALL_PAYMENT_SCOPES;
use adk_payments::guardrail::{AmountThresholdGuardrail, PaymentPolicySet};
use adk_payments::tools::PaymentToolsetBuilder;
use adk_runner::Runner;
use adk_session::{
    CreateRequest, EncryptedSession, EncryptionKey, GetRequest, InMemorySessionService,
    ListRequest, RedisSessionConfig, RedisSessionService, SessionService, SqliteSessionService,
};
use async_trait::async_trait;
use futures::StreamExt;
use issuer::Issuer;
use merchant::{Merchant, checkout_args};
use scripted::{Reply, Script, ScriptedServer, call};
use serde_json::{Value, json};

const APP: &str = "security-sessions";
const USER: &str = "user-1";
const LIVE_MODEL: &str = "gpt-5.6-terra";
const CHECKOUT: &str = "payments_checkout_create";

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

/// `category/code` of an error, for evidence lines.
fn kind(error: &AdkError) -> String {
    format!("{:?}/{}", error.category, error.code)
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// What one run produced.
struct Turn {
    events: Vec<Event>,
    error: Option<String>,
}

impl Turn {
    fn held(&self) -> Vec<ToolConfirmationRequest> {
        self.events.iter().filter_map(|event| event.actions.tool_confirmation.clone()).collect()
    }

    /// The response that answered `call_id` last in this run, if any.
    fn answer_to(&self, call_id: &str) -> Option<Value> {
        self.events
            .iter()
            .flat_map(|event| event.llm_response.content.iter().flat_map(|content| &content.parts))
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, id, .. }
                    if id.as_deref() == Some(call_id) =>
                {
                    Some(function_response.response.clone())
                }
                _ => None,
            })
            .next_back()
    }
}

async fn run_turn(
    runner: &Runner,
    session_id: &str,
    text: &str,
    config: Option<RunConfig>,
) -> anyhow::Result<Turn> {
    let mut events = Vec::new();
    let mut error = None;
    let content = Content::new("user").with_text(text);
    let user = UserId::try_from(USER)?;
    let session = SessionId::try_from(session_id)?;
    match runner.run_with_config(user, session, content, config).await {
        Ok(mut stream) => {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(event) => events.push(event),
                    Err(err) => {
                        error = Some(err.to_string());
                        break;
                    }
                }
            }
        }
        Err(err) => error = Some(err.to_string()),
    }
    Ok(Turn { events, error })
}

async fn new_session(sessions: &dyn SessionService, session_id: &str) -> anyhow::Result<()> {
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    Ok(())
}

fn last_role(request: &Value) -> Option<String> {
    request.get("messages")?.as_array()?.last()?.get("role")?.as_str().map(str::to_string)
}

fn last_user_text(request: &Value) -> String {
    request
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .rfind(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .map(Value::to_string)
        .unwrap_or_default()
}

fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OpenAIClient::compatible("scripted-key", &server.base_url, "scripted-model")?))
}

// ---------------------------------------------------------------------------
// Escalated payments
// ---------------------------------------------------------------------------

/// What the three runs of an escalated checkout produced.
struct PaymentRun {
    held_in_run1: Vec<ToolConfirmationRequest>,
    created_after_run1: usize,
    pending_after_run1: Vec<String>,
    created_after_run2: usize,
    answer: Option<Value>,
    held_in_run2: usize,
    decision_left: bool,
    created_after_run3: usize,
    errors: Vec<String>,
}

/// Run 1 asks for a $750 desk, which the payment policy sends for review above $50. The approver
/// decides the held call in the approval store; run 2 resumes it; run 3 is an unrelated turn.
async fn escalated_checkout(llm: Arc<dyn Llm>, prompts: [&str; 3]) -> anyhow::Result<PaymentRun> {
    let merchant = Arc::new(Merchant::default());
    let review_above_fifty = PaymentPolicySet::new()
        .with(AmountThresholdGuardrail::new(Some(5_000), None).with_currency("USD", 2));
    let tools = PaymentToolsetBuilder::new(merchant.clone(), merchant.clone())
        .with_payment_policies(review_above_fifty)
        .with_scope_guard(ScopeGuard::new(StaticScopeResolver::new(ALL_PAYMENT_SCOPES.to_vec())))
        .build()
        .tools();
    let checkout = tools.into_iter().find(|tool| tool.name() == CHECKOUT).expect("checkout tool");

    let agent = LlmAgentBuilder::new("shopper")
        .instruction(format!(
            "You buy office furniture. When the user asks to buy the desk, call {CHECKOUT} once \
             with exactly these arguments: {}. Do not call any other tool.",
            checkout_args(75_000)
        ))
        .model(llm)
        .tool(checkout)
        .build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    let store = Arc::new(InMemoryApprovalStore::new());
    let session_id = "checkout-1";
    new_session(sessions.as_ref(), session_id).await?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(sessions)
        .run_config(RunConfig::builder().approval_store(store.clone()).build())
        .build()?;
    let scope = ApprovalScope::new(APP, USER, session_id);
    let mut errors = Vec::new();

    let run1 = run_turn(&runner, session_id, prompts[0], None).await?;
    errors.extend(run1.error.clone());
    let held_in_run1 = run1.held();
    let created_after_run1 = merchant.created();
    let pending = store.pending(&scope).await?;
    let pending_after_run1 =
        pending.iter().map(|entry| entry.request.tool_name.clone()).collect::<Vec<_>>();

    // The approver's inbox: the pending entry, or the held request the run surfaced.
    let fingerprint = match (pending.first(), held_in_run1.first()) {
        (Some(entry), _) => Some(entry.fingerprint.clone()),
        (None, Some(request)) => Some(request.fingerprint()),
        (None, None) => None,
    };
    let call_id = held_in_run1.first().and_then(|request| request.function_call_id.clone());
    if let Some(fingerprint) = &fingerprint {
        store.decide(&scope, fingerprint, ToolApproval::approve()).await?;
    }

    let run2 = run_turn(&runner, session_id, prompts[1], None).await?;
    errors.extend(run2.error.clone());
    let created_after_run2 = merchant.created();
    let answer = call_id.as_deref().and_then(|id| run2.answer_to(id));
    let decision_left = match &fingerprint {
        Some(fingerprint) => store.has_decision(&scope, fingerprint).await?,
        None => false,
    };

    let run3 = run_turn(&runner, session_id, prompts[2], None).await?;
    errors.extend(run3.error.clone());

    Ok(PaymentRun {
        held_in_run1,
        created_after_run1,
        pending_after_run1,
        created_after_run2,
        answer,
        held_in_run2: run2.held().len(),
        decision_left,
        created_after_run3: merchant.created(),
        errors,
    })
}

fn payment_checks(label: &str, run: &PaymentRun) -> Vec<(bool, String)> {
    let held: Vec<&str> = run.held_in_run1.iter().map(|r| r.tool_name.as_str()).collect();
    let status = run.answer.as_ref().and_then(|answer| answer.get("status")).cloned();
    vec![
        (held == [CHECKOUT], format!("{label}: run 1 held {held:?} (expected [{CHECKOUT}])")),
        (
            run.created_after_run1 == 0,
            format!("{label}: checkouts before approval: {}", run.created_after_run1),
        ),
        (
            run.pending_after_run1 == [CHECKOUT],
            format!(
                "{label}: approval store pending after run 1: {:?} (expected [{CHECKOUT}])",
                run.pending_after_run1
            ),
        ),
        (
            run.created_after_run2 == 1,
            format!(
                "{label}: checkouts after the approved run: {} (expected 1)",
                run.created_after_run2
            ),
        ),
        (
            status == Some(json!("ok")),
            format!("{label}: the held call's answer in run 2: {:?}", run.answer),
        ),
        (
            run.held_in_run2 == 0,
            format!("{label}: calls held again in run 2: {}", run.held_in_run2),
        ),
        (!run.decision_left, format!("{label}: approval left unconsumed: {}", run.decision_left)),
        (
            run.created_after_run3 == 1,
            format!("{label}: checkouts after run 3: {} (expected 1)", run.created_after_run3),
        ),
        (run.errors.is_empty(), format!("{label}: run errors: {:?}", run.errors)),
    ]
}

/// Buys when asked, answers a tool result with text, and says nothing else.
fn shopper_script() -> Script {
    Box::new(|request, _| {
        if last_role(request).as_deref() == Some("tool") {
            return Reply::Text("The desk is ordered.".into());
        }
        if last_user_text(request).contains("Buy the desk") {
            return call("call_desk_1", CHECKOUT, &checkout_args(75_000).to_string());
        }
        Reply::Text("Anything else?".into())
    })
}

async fn escalated_payment_resumes_from_approval_store() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(shopper_script()).await?;
    let run = escalated_checkout(
        scripted_model(&server)?,
        ["Buy the desk.", "Approved, go ahead.", "Thanks."],
    )
    .await?;
    Ok(verdict(payment_checks("scripted", &run)))
}

async fn live_escalated_payment_resumes() -> anyhow::Result<Verdict> {
    let Ok(key) = std::env::var("OPENAI_API_KEY") else {
        return Ok(Verdict::Skip("OPENAI_API_KEY is not set".into()));
    };
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| LIVE_MODEL.to_string());
    let llm: Arc<dyn Llm> = Arc::new(OpenAIClient::new(OpenAIConfig::new(key, model.as_str()))?);
    let run = escalated_checkout(
        llm,
        ["Buy the desk.", "The purchase was approved; tell me the result.", "Thanks."],
    )
    .await?;
    if run.held_in_run1.is_empty() && run.created_after_run1 == 0 {
        return Ok(Verdict::Skip(format!("{model} did not call {CHECKOUT} in run 1")));
    }
    Ok(verdict(payment_checks(&model, &run)))
}

// ---------------------------------------------------------------------------
// SSO and the audit sink
// ---------------------------------------------------------------------------

/// An audit store that is down: every write fails, as a database sink does during an outage.
struct UnavailableAuditStore;

#[async_trait]
impl AuditSink for UnavailableAuditStore {
    async fn log(&self, _event: AuditEvent) -> Result<(), AuthError> {
        Err(AuthError::AuditError("audit store unavailable: connection refused".into()))
    }
}

fn sso(issuer: &Issuer, sink: impl AuditSink + 'static) -> anyhow::Result<SsoAccessControl> {
    let validator = JwtValidator::builder()
        .issuer(&issuer.url)
        .audience(issuer::AUDIENCE)
        .jwks_uri(issuer.jwks_uri())
        .algorithm(jsonwebtoken::Algorithm::ES256)
        .build()?;
    let access = AccessControl::builder()
        .role(Role::new("engineer").allow(Permission::Tool("deploy".into())))
        .build()?;
    SsoAccessControl::builder()
        .validator(validator)
        .mapper(ClaimsMapper::builder().map_group("Engineering", "engineer").build())
        .access_control(access)
        .audit_sink(sink)
        .build()
        .map_err(|error| anyhow::anyhow!(error))
}

async fn sso_audit_failure_refuses_access() -> anyhow::Result<Verdict> {
    let issuer = Issuer::start().await?;
    let engineer = issuer.token("alice", &["Engineering"])?;
    let outsider = issuer.token("mallory", &["Sales"])?;
    let deploy = Permission::Tool("deploy".into());

    let recorded = Arc::new(InMemoryAuditSink::new());
    let working = sso(&issuer, recorded.clone())?;
    let granted = working.check_token(&engineer, &deploy).await;
    let events = recorded.len().await;

    let failing = sso(&issuer, UnavailableAuditStore)?;
    let unaudited = failing.check_token(&engineer, &deploy).await;
    let denied = failing.check_token(&outsider, &deploy).await;

    Ok(verdict(vec![
        (
            granted.as_ref().is_ok_and(|claims| claims.sub == "alice"),
            format!(
                "working sink: engineer granted: {:?}",
                granted.as_ref().map(|claims| &claims.sub)
            ),
        ),
        (events == 1, format!("working sink: audit events recorded: {events}")),
        (
            unaudited.is_err(),
            format!(
                "failing sink: engineer's unrecorded grant refused: {}",
                match &unaudited {
                    Ok(claims) => format!("granted to {} without a record", claims.sub),
                    Err(error) => format!("refused ({error})"),
                }
            ),
        ),
        (
            denied.as_ref().is_err_and(|error| error.to_string().contains("Access denied")),
            format!(
                "failing sink: outsider still denied: {:?}",
                denied.as_ref().err().map(ToString::to_string)
            ),
        ),
    ]))
}

// ---------------------------------------------------------------------------
// Artifact errors
// ---------------------------------------------------------------------------

/// Opens a session artifact and reports how a load failed.
struct OpenArtifact;

#[async_trait]
impl Tool for OpenArtifact {
    fn name(&self) -> &str {
        "open_artifact"
    }

    fn description(&self) -> &str {
        "Opens one of the session's artifacts by name."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let name = args["name"].as_str().unwrap_or_default().to_string();
        let artifacts = ctx.artifacts().ok_or_else(|| AdkError::tool("no artifact service"))?;
        Ok(match artifacts.load(&name).await {
            Ok(_) => json!({ "name": name, "loaded": true }),
            Err(error) => json!({
                "name": name,
                "loaded": false,
                "category": format!("{:?}", error.category),
                "code": error.code,
                "not_found": error.is_not_found(),
            }),
        })
    }
}

fn artifact_script() -> Script {
    Box::new(|request, _| {
        if last_role(request).as_deref() == Some("tool") {
            return Reply::Text("checked".into());
        }
        Reply::Calls(vec![
            scripted::ScriptedCall {
                id: "call_missing".into(),
                name: "open_artifact".into(),
                arguments: r#"{"name":"quarterly-report.pdf"}"#.into(),
            },
            scripted::ScriptedCall {
                id: "call_escape".into(),
                name: "open_artifact".into(),
                arguments: r#"{"name":"../../etc/passwd"}"#.into(),
            },
        ])
    })
}

async fn artifact_errors_are_typed() -> anyhow::Result<Verdict> {
    let dir = tempfile::tempdir()?;
    let backends: Vec<(&str, Arc<dyn ArtifactService>)> = vec![
        ("memory", Arc::new(InMemoryArtifactService::new())),
        ("file", Arc::new(FileArtifactService::new(dir.path())?)),
    ];
    let mut checks = Vec::new();
    for (label, artifacts) in backends {
        let server = ScriptedServer::start(artifact_script()).await?;
        let agent = LlmAgentBuilder::new("librarian")
            .model(scripted_model(&server)?)
            .tool(Arc::new(OpenArtifact))
            .build()?;
        let sessions = Arc::new(InMemorySessionService::new());
        new_session(sessions.as_ref(), "library-1").await?;
        let runner = Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent) as Arc<dyn Agent>)
            .session_service(sessions)
            .artifact_service(artifacts)
            .build()?;
        let turn = run_turn(&runner, "library-1", "Open the report.", None).await?;
        let missing = turn.answer_to("call_missing").unwrap_or(Value::Null);
        let escape = turn.answer_to("call_escape").unwrap_or(Value::Null);
        checks.push((
            missing["category"] == "NotFound" && missing["not_found"] == true,
            format!(
                "{label}: missing artifact fails as {}/{} (expected NotFound)",
                missing["category"], missing["code"]
            ),
        ));
        checks.push((
            escape["category"] == "InvalidInput",
            format!(
                "{label}: '../' name fails as {}/{} (expected InvalidInput)",
                escape["category"], escape["code"]
            ),
        ));
        checks.push((
            missing["code"] != "artifact.legacy" && escape["code"] != "artifact.legacy",
            format!("{label}: no legacy code: {} / {}", missing["code"], escape["code"]),
        ));
        checks.push((turn.error.is_none(), format!("{label}: run error: {:?}", turn.error)));
    }
    Ok(verdict(checks))
}

// ---------------------------------------------------------------------------
// Session identifier errors
// ---------------------------------------------------------------------------

/// Whether `result` failed as a typed session identifier error.
fn identifier_check(label: &str, operation: &str, result: adk_core::Result<()>) -> (bool, String) {
    match result {
        Ok(()) => (false, format!("{label}: {operation} accepted an invalid identifier")),
        Err(error) => (
            error.category == ErrorCategory::InvalidInput
                && error.component == ErrorComponent::Session
                && error.code == "session.invalid_identifier",
            format!(
                "{label}: {operation} fails as {}/{:?} (expected session.invalid_identifier)",
                kind(&error),
                error.component
            ),
        ),
    }
}

async fn identifier_checks(label: &str, service: &dyn SessionService) -> Vec<(bool, String)> {
    let empty_user = service
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: String::new(),
            session_id: None,
            state: HashMap::new(),
        })
        .await
        .map(drop);
    let nul_session = service
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: "bad\0id".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .map(drop);
    let long_app = service
        .list(ListRequest {
            app_name: "a".repeat(adk_core::identity::MAX_ID_LEN + 1),
            user_id: USER.to_string(),
            limit: None,
            offset: None,
        })
        .await
        .map(drop);
    vec![
        identifier_check(label, "create with an empty user_id", empty_user),
        identifier_check(label, "get with a NUL session_id", nul_session),
        identifier_check(label, "list with an oversized app_name", long_app),
    ]
}

async fn session_identifier_errors_are_typed() -> anyhow::Result<Verdict> {
    let mut checks = Vec::new();
    checks.extend(identifier_checks("memory", &InMemorySessionService::new()).await);
    let encrypted =
        EncryptedSession::new(InMemorySessionService::new(), EncryptionKey::generate(), vec![]);
    checks.extend(identifier_checks("encrypted", &encrypted).await);

    let dir = tempfile::tempdir()?;
    let sqlite = SqliteSessionService::new(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("sessions.db").display()
    ))
    .await?;
    sqlite.migrate().await?;
    checks.extend(identifier_checks("sqlite", &sqlite).await);

    match std::env::var("REDIS_URL") {
        Ok(url) => {
            let redis = RedisSessionService::new(RedisSessionConfig {
                url,
                ttl: None,
                cluster_nodes: None,
            })
            .await?;
            checks.extend(identifier_checks("redis", &redis).await);
        }
        Err(_) => checks.push((true, "redis: skipped (REDIS_URL is not set)".to_string())),
    }

    // The runner parses the identifiers of a string-addressed run the same way.
    let server = ScriptedServer::start(Box::new(|_, _| Reply::Text("hi".into()))).await?;
    let agent = LlmAgentBuilder::new("greeter").model(scripted_model(&server)?).build()?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(Arc::new(InMemorySessionService::new()))
        .build()?;
    let run = runner.run_str("", "session-1", Content::new("user").with_text("hi")).await.map(drop);
    checks.push(identifier_check("runner", "run_str with an empty user_id", run));
    Ok(verdict(checks))
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

const SCENARIOS: &[&str] = &[
    "escalated_payment_resumes_from_approval_store",
    "live_escalated_payment_resumes",
    "sso_audit_failure_refuses_access",
    "artifact_errors_are_typed",
    "session_identifier_errors_are_typed",
    "jwt_validator_requires_iss_and_aud",
    "live_jwt_claims_server_run",
];

async fn run_scenario(name: &str) -> anyhow::Result<Verdict> {
    match name {
        "escalated_payment_resumes_from_approval_store" => {
            escalated_payment_resumes_from_approval_store().await
        }
        "live_escalated_payment_resumes" => live_escalated_payment_resumes().await,
        "sso_audit_failure_refuses_access" => sso_audit_failure_refuses_access().await,
        "artifact_errors_are_typed" => artifact_errors_are_typed().await,
        "session_identifier_errors_are_typed" => session_identifier_errors_are_typed().await,
        "jwt_validator_requires_iss_and_aud" => {
            jwt_claims::jwt_validator_requires_iss_and_aud().await
        }
        "live_jwt_claims_server_run" => jwt_claims::live_jwt_claims_server_run().await,
        other => anyhow::bail!("unknown scenario {other}; known: {}", SCENARIOS.join(", ")),
    }
}

fn load_env() {
    // The first dotenv file found wins over stale shell exports.
    let candidates = [
        std::env::var("ADK_ENV_FILE").ok(),
        Some(".env".to_string()),
        Some(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env").to_string()),
    ];
    for path in candidates.into_iter().flatten() {
        if dotenvy::from_path_override(&path).is_ok() {
            break;
        }
    }
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

    println!("security and sessions contracts — {} scenario(s)\n", selected.len());
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
