//! Group G: what an error response tells the client under
//! `SecurityConfig::expose_error_details`.
//!
//! A `claude-sonnet-5-5` agent whose client carries a deliberately invalid API key meets a real
//! Anthropic `401`. With the flag off, the run endpoints' Problem JSON, the SSE error event, and
//! the AG-UI `RUN_ERROR` carry a generic message instead of Anthropic's text, and the server
//! logs the full error inside the request's span with its `x-request-id`; with the flag on,
//! Anthropic's text comes back. A governance store whose database is down does the same for
//! the `503` of `POST /api/admin/freeze` and `/api/admin/unfreeze`. A message the server wrote
//! about the request is kept, and a live turn with the real key answers as before.
//!
//! The server runs on its own thread and current-thread runtime with a thread-local
//! `tracing` subscriber, so every log line it writes is captured here.

use std::io::Write;
use std::sync::{Arc, Mutex};

use adk_core::governance::GovernanceMirrorConfig;
use adk_core::governance_state::{GovernanceScope, GovernanceState, GovernanceStateStore};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, Agent, ErrorCategory, ErrorComponent, GovernanceControl, Llm, RequestContext,
    SingleAgentLoader,
};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use serde_json::{Value, json};

use crate::{
    Kind, Outcome, errored, event_text, http, outcome, plain_agent, run_body, skip, truncate,
};

const CLAUDE: &str = "claude-sonnet-5-5";
const APP: &str = "claims_assistant";
const INVALID_KEY: &str = "sk-ant-api03-scenario-g-deliberately-invalid-key";
/// Text only Anthropic's own error message carries, never the generic replacement.
const PROVIDER_MARKER: &str = "Anthropic API error";
const GENERIC_UNAUTHORIZED: &str = "Authentication failed";
const DRIVER_TEXT: &str = "error returned from database: relation \"adk_governance_state\" does \
                           not exist (db-primary.internal:5432, role adk_ops)";

/// Everything the served thread logs.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Logs {
    /// The log lines written inside the request `request_id`'s span.
    fn of_request(&self, request_id: &str) -> Vec<String> {
        let marker = format!("request.id={request_id}");
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .filter(|line| line.contains(&marker))
            .map(str::to_string)
            .collect()
    }
}

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Serves `app` on 127.0.0.1 from its own thread, whose log lines land in the returned
/// [`Logs`].
fn serve_logged(app: Router) -> anyhow::Result<(String, Logs)> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let base = format!("http://{}", listener.local_addr()?);
    let logs = Logs::default();
    let writer = logs.clone();
    std::thread::spawn(move || {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
            return;
        };
        runtime.block_on(async move {
            if let Ok(listener) = tokio::net::TcpListener::from_std(listener) {
                let _ = axum::serve(listener, app).await;
            }
        });
    });
    Ok((base, logs))
}

fn claude(key: &str) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(AnthropicClient::new(AnthropicConfig::new(key, CLAUDE).with_max_tokens(64))?))
}

fn agent(model: Arc<dyn Llm>) -> anyhow::Result<Arc<dyn Agent>> {
    plain_agent(APP, "You are claims_assistant. Reply with one short sentence.", model)
}

fn server_config(agent: Arc<dyn Agent>) -> ServerConfig {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions)
}

/// Status, `x-request-id`, and body text of a JSON `POST`.
async fn post(
    url: &str,
    body: &Value,
    token: Option<&str>,
) -> anyhow::Result<(u16, String, String)> {
    let mut request = http().post(url).json(body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Ok((status, request_id, response.text().await?))
}

fn sse_events(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str(data).ok())
        .collect()
}

fn ag_ui_body(thread: &str) -> Value {
    json!({
        "appName": APP,
        "userId": "alice",
        "sessionId": thread,
        "uiProtocol": "ag_ui",
        "uiTransport": "protocol_native",
        "input": {
            "threadId": thread,
            "runId": format!("run-{thread}"),
            "messages": [{"id": "m1", "role": "user", "content": "Hello"}],
        },
    })
}

/// What the three run endpoints answered for a failed model call.
struct RunErrors {
    run_status: u16,
    run_request_id: String,
    problem: Value,
    sse_request_id: String,
    sse_error: Value,
    ag_ui_error: Value,
}

async fn run_errors(base: &str) -> anyhow::Result<RunErrors> {
    let (run_status, run_request_id, text) =
        post(&format!("{base}/api/run"), &run_body(APP, "alice", "s-run", "Hello"), None).await?;
    let problem: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    let (_, sse_request_id, text) =
        post(&format!("{base}/api/run_sse"), &run_body(APP, "alice", "s-sse", "Hello"), None)
            .await?;
    let sse_error = sse_events(&text)
        .into_iter()
        .rfind(|event| event.get("error_code").is_some_and(|code| !code.is_null()))
        .unwrap_or(Value::Null);
    let (_, _, text) = post(&format!("{base}/api/run_sse"), &ag_ui_body("t-ag-ui"), None).await?;
    let ag_ui_error = sse_events(&text)
        .into_iter()
        .find(|event| event["type"] == "RUN_ERROR")
        .unwrap_or(Value::Null);
    Ok(RunErrors { run_status, run_request_id, problem, sse_request_id, sse_error, ag_ui_error })
}

fn message_of(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// Whether `logs` hold the full Anthropic error inside the span of `request_id`.
fn logged_in_full(logs: &Logs, request_id: &str) -> bool {
    !request_id.is_empty()
        && logs.of_request(request_id).iter().any(|line| {
            line.contains("error details withheld from the response")
                && line.contains(PROVIDER_MARKER)
        })
}

/// A governance store whose database is down.
struct UnreachableGovernanceStore;

fn store_error() -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unavailable,
        "session.governance_store.storage",
        format!("governance read failed: {DRIVER_TEXT}"),
    )
}

#[async_trait]
impl GovernanceStateStore for UnreachableGovernanceStore {
    async fn get(&self, _scope: &GovernanceScope) -> adk_core::Result<Option<GovernanceState>> {
        Err(store_error())
    }

    async fn set(
        &self,
        _scope: GovernanceScope,
        _expected_epoch: u64,
        _frozen: bool,
        _reason: Option<String>,
        _issued_by: Option<SubjectRef>,
    ) -> adk_core::Result<GovernanceState> {
        Err(store_error())
    }

    async fn list(&self) -> adk_core::Result<Vec<GovernanceState>> {
        Err(store_error())
    }
}

/// Authenticates the bearer token `ops-admin` as an administrator.
struct AdminToken;

#[async_trait]
impl RequestContextExtractor for AdminToken {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        match parts.headers.get("authorization").and_then(|value| value.to_str().ok()) {
            Some("Bearer ops-admin") => Ok(RequestContext {
                user_id: "ops-admin".to_string(),
                scopes: vec!["admin".to_string()],
                metadata: Default::default(),
            }),
            Some(_) => Err(RequestContextError::InvalidToken("unknown token".to_string())),
            None => Err(RequestContextError::MissingAuth),
        }
    }
}

/// The `error` of the freeze and the unfreeze `503`s from a server over an unreachable
/// governance store.
async fn governance_errors(agent: Arc<dyn Agent>, expose: bool) -> anyhow::Result<Vec<String>> {
    let control = GovernanceControl::mirrored(
        Arc::new(UnreachableGovernanceStore),
        GovernanceMirrorConfig::default(),
    );
    let config = server_config(agent)
        .with_request_context(Arc::new(AdminToken))
        .with_governance(control)
        .with_error_details(expose);
    let (base, _) = serve_logged(ServerBuilder::new(config).enable_governance_endpoints().build())?;
    let mut errors = Vec::new();
    for (path, body) in [("freeze", json!({"reason": "incident 7"})), ("unfreeze", json!({}))] {
        let (status, _, text) =
            post(&format!("{base}/api/admin/{path}"), &body, Some("ops-admin")).await?;
        anyhow::ensure!(status == 503, "{path} answered {status}: {text}");
        let status: Value = serde_json::from_str(&text)?;
        errors.push(message_of(&status["error"]).to_string());
    }
    Ok(errors)
}

pub async fn group_g(out: &mut Vec<Outcome>) {
    let real_key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|key| !key.trim().is_empty());

    // G1–G4: a real Anthropic 401 through each run endpoint, details hidden then exposed. The
    // invalid key needs no account, so these run without ANTHROPIC_API_KEY.
    let invalid_agent = match claude(INVALID_KEY).and_then(agent) {
        Ok(agent) => agent,
        Err(error) => {
            out.push(errored("G1 provider-text-withheld-run", Kind::Live, error));
            return;
        }
    };
    let hidden = async {
        let (base, logs) =
            serve_logged(ServerBuilder::new(server_config(invalid_agent.clone())).build())?;
        Ok::<_, anyhow::Error>((run_errors(&base).await?, logs))
    }
    .await;
    match hidden {
        Ok((errors, logs)) => {
            let message = message_of(&errors.problem["error"]["message"]);
            out.push(outcome(
                "G1 provider-text-withheld-run",
                Kind::Live,
                errors.run_status == 401
                    && message == GENERIC_UNAUTHORIZED
                    && errors.problem["error"]["code"] == "model.anthropic.api_error"
                    && errors.problem["error"]["category"] == "unauthorized"
                    && !errors.problem.to_string().contains(PROVIDER_MARKER)
                    && logged_in_full(&logs, &errors.run_request_id),
                format!(
                    "HTTP {}, message {:?}, code {}, requestId {}, full error logged under \
                     x-request-id: {}",
                    errors.run_status,
                    truncate(message, 90),
                    errors.problem["error"]["code"],
                    errors.problem["error"]["requestId"],
                    logged_in_full(&logs, &errors.run_request_id)
                ),
            ));
            let sse_message = message_of(&errors.sse_error["error_message"]);
            out.push(outcome(
                "G2 provider-text-withheld-sse",
                Kind::Live,
                sse_message == format!("model.unauthorized: {GENERIC_UNAUTHORIZED}")
                    && errors.sse_error["error_code"] == "model.anthropic.api_error"
                    && logged_in_full(&logs, &errors.sse_request_id),
                format!(
                    "error_message {:?}, full error logged under x-request-id: {}",
                    truncate(sse_message, 90),
                    logged_in_full(&logs, &errors.sse_request_id)
                ),
            ));
            let ag_ui_message = message_of(&errors.ag_ui_error["message"]);
            out.push(outcome(
                "G3 provider-text-withheld-ag-ui",
                Kind::Live,
                ag_ui_message == format!("model.unauthorized: {GENERIC_UNAUTHORIZED}")
                    && errors.ag_ui_error["code"] == "model.anthropic.api_error",
                format!(
                    "RUN_ERROR message {:?}, code {}",
                    truncate(ag_ui_message, 90),
                    errors.ag_ui_error["code"]
                ),
            ));
        }
        Err(error) => out.push(errored("G1 provider-text-withheld-run", Kind::Live, error)),
    }

    let exposed = async {
        let config = server_config(invalid_agent.clone()).with_error_details(true);
        let (base, _) = serve_logged(ServerBuilder::new(config).build())?;
        run_errors(&base).await
    }
    .await;
    out.push(match exposed {
        Ok(errors) => {
            let shown = [
                message_of(&errors.problem["error"]["message"]),
                message_of(&errors.sse_error["error_message"]),
                message_of(&errors.ag_ui_error["message"]),
            ];
            outcome(
                "G4 provider-text-exposed",
                Kind::Live,
                shown.iter().all(|message| message.contains(PROVIDER_MARKER)),
                format!("Problem JSON message {:?}", truncate(shown[0], 110)),
            )
        }
        Err(error) => errored("G4 provider-text-exposed", Kind::Live, error),
    });

    // G5: the governance 503 over a store whose database is down.
    let governance = async {
        let hidden = governance_errors(invalid_agent.clone(), false).await?;
        let exposed = governance_errors(invalid_agent.clone(), true).await?;
        Ok::<_, anyhow::Error>((hidden, exposed))
    }
    .await;
    out.push(match governance {
        Ok((hidden, exposed)) => outcome(
            "G5 store-text-withheld-503",
            Kind::Scripted,
            hidden.iter().all(|error| {
                error
                    == "session.unavailable: A service the server depends on is unavailable; \
                          retry later"
            }) && exposed.iter().all(|error| error.contains(DRIVER_TEXT)),
            format!(
                "freeze/unfreeze error {:?}; exposed {:?}",
                truncate(&hidden[0], 90),
                truncate(&exposed[0], 60)
            ),
        ),
        Err(error) => errored("G5 store-text-withheld-503", Kind::Scripted, error),
    });

    // G6: messages the server writes about the request itself are kept.
    let kept = async {
        let control = GovernanceControl::new();
        let config = server_config(invalid_agent.clone()).with_governance(control.clone());
        let (base, _) = serve_logged(ServerBuilder::new(config).build())?;
        let (missing_status, _, text) = post(
            &format!("{base}/api/run/{APP}/alice/s-missing"),
            &json!({"new_message": "Hi"}),
            None,
        )
        .await?;
        let missing: Value = serde_json::from_str(&text)?;
        let (app_status, _, text) =
            post(&format!("{base}/api/run"), &run_body("other_app", "alice", "s-1", "Hi"), None)
                .await?;
        let unknown_app: Value = serde_json::from_str(&text)?;
        control.freeze("incident 7 under review");
        let (frozen_status, _, text) =
            post(&format!("{base}/api/run"), &run_body(APP, "alice", "s-2", "Hi"), None).await?;
        let frozen: Value = serde_json::from_str(&text)?;
        Ok::<_, anyhow::Error>([
            (missing_status, missing["error"].clone()),
            (app_status, unknown_app["error"].clone()),
            (frozen_status, frozen["error"].clone()),
        ])
    }
    .await;
    out.push(match kept {
        Ok([missing, unknown_app, frozen]) => outcome(
            "G6 request-messages-kept",
            Kind::Local,
            missing.0 == 404
                && message_of(&missing.1["message"]).contains("'s-missing' was not found")
                && unknown_app.0 == 400
                && message_of(&unknown_app.1["message"]).contains("Cannot load agent 'other_app'")
                && frozen.0 == 403
                && message_of(&frozen.1["message"]).contains("incident 7 under review"),
            format!(
                "{} {:?}; {} {:?}; {} {:?}",
                missing.0,
                truncate(message_of(&missing.1["message"]), 50),
                unknown_app.0,
                truncate(message_of(&unknown_app.1["message"]), 40),
                frozen.0,
                truncate(message_of(&frozen.1["message"]), 60),
            ),
        ),
        Err(error) => errored("G6 request-messages-kept", Kind::Local, error),
    });

    // G7: a live turn with the real key answers as before.
    let Some(real_key) = real_key else {
        out.push(skip("G7 happy-path-unaffected", Kind::Live, "ANTHROPIC_API_KEY is not set"));
        return;
    };
    let happy = async {
        let (base, _) =
            serve_logged(ServerBuilder::new(server_config(agent(claude(&real_key)?)?)).build())?;
        let (status, _, text) = post(
            &format!("{base}/api/run"),
            &run_body(APP, "alice", "s-happy", "Say hello in three words."),
            None,
        )
        .await?;
        let events: Vec<adk_core::Event> = serde_json::from_str(&text).unwrap_or_default();
        let errors: Vec<_> =
            events.iter().filter_map(|event| event.llm_response.error_message.clone()).collect();
        Ok::<_, anyhow::Error>((status, event_text(&events), errors))
    }
    .await;
    out.push(match happy {
        Ok((status, text, errors)) => outcome(
            "G7 happy-path-unaffected",
            Kind::Live,
            status == 200 && !text.trim().is_empty() && errors.is_empty(),
            format!("HTTP {status}, {CLAUDE} said {:?}", truncate(&text, 60)),
        ),
        Err(error) => errored("G7 happy-path-unaffected", Kind::Live, error),
    });
}
