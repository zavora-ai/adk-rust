//! The workflow observer over a scripted graph agent (spec 3.3 §12 developer track, "Approve,
//! rewind, fork"): what `web/e2e/workflow.spec.ts` opens.
//!
//! A runner runs the graph agent `invoice-run`: `draft` (a model step on `ScriptedLlm` that
//! writes `invoice`), then `pay` (a governed HTTP POST of the invoice to `/charges` on this
//! server, which asks first and spends money), then `receipt` (a function). `/charges` is a
//! stand-in payment provider that counts every charge; `GET /hits` reads the count. The
//! sessions, checkpoints, action ledger and approval inbox are SQLite files in the workspace,
//! so a restart keeps every run. The engine carries the observer's card hints, a heading hook
//! ("Charge Acme 42.00 USD") and its approval router; a stand-in extractor admits every request
//! as the owner, with "Confirm it's you" already done, as a passkey would on the person's own
//! machine.
//!
//! ```sh
//! cargo run -p adk-surface --features web,workflow --example scripted_workflow -- 8795 /tmp/workflow-workspace
//! # The draft step on a live model, with whichever key the repository's .env sets:
//! cargo run -p adk-surface --features web,workflow --example scripted_workflow -- 8795 /tmp/workflow-workspace --live
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_action::{
    ActionNodeConfig, Callbacks, ErrorHandling, ErrorMode, ExecutionControl, HttpAuth, HttpBody,
    HttpMethod, HttpNodeConfig, HttpResponse, InputOutputMapping, LogLevel, StandardProperties,
    Tracing,
};
use adk_agent::LlmAgentBuilder;
use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApproverSelector, Content, Event, GovernanceControl, Llm,
    RequestContext, RunConfig, SingleAgentLoader, StoreApprovalService,
};
use adk_graph::action::ActionNodeExecutor;
use adk_graph::action::http::HttpActionPolicy;
use adk_graph::agent::GraphAgent;
use adk_graph::checkpoint::SqliteCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::governed::GovernedNode;
use adk_graph::node::{AgentNode, NodeOutput};
use adk_graph::state::State;
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{SessionService, SqliteActionLedger, SqliteApprovalStore, SqliteSessionService};
use adk_surface::web::{PAIRED_LOCAL_METADATA_KEY, PRESENCE_METADATA_KEY};
use adk_surface::workflow::Workflows;
use adk_surface::{LocalSource, Place, SurfaceEngine};
use async_trait::async_trait;
use axum::extract::State as AxumState;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

const APP: &str = "adk-rust";
const OWNER: &str = "ana";
const GRAPH: &str = "invoice-run";

/// Admits every request as the owner, with "Confirm it's you" done: on the person's own
/// machine a passkey would answer the same.
struct Owner;

#[async_trait]
impl RequestContextExtractor for Owner {
    async fn extract(
        &self,
        _parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        Ok(RequestContext {
            user_id: OWNER.into(),
            scopes: vec!["owner".into(), "operator".into()],
            metadata: [
                (PRESENCE_METADATA_KEY.to_string(), "confirmed".to_string()),
                (PAIRED_LOCAL_METADATA_KEY.to_string(), "false".to_string()),
            ]
            .into(),
        })
    }
}

/// The stand-in payment provider: every charge it receives counts.
#[derive(Clone, Default)]
struct Provider {
    hits: Arc<AtomicUsize>,
}

async fn charge(AxumState(provider): AxumState<Provider>, Json(body): Json<Value>) -> Json<Value> {
    let hit = provider.hits.fetch_add(1, Ordering::SeqCst) + 1;
    Json(json!({ "id": format!("ch_{hit}"), "amountCents": body["amountCents"], "status": "paid" }))
}

async fn hits(AxumState(provider): AxumState<Provider>) -> Json<Value> {
    Json(json!({ "hits": provider.hits.load(Ordering::SeqCst) }))
}

/// The draft step's model: scripted, or with `--live` the provider whose key the repository's
/// `.env` sets.
fn model(live: bool) -> Result<Arc<dyn Llm>, Box<dyn std::error::Error>> {
    if !live {
        let invoice = r#"{"customer":"Acme","amountCents":4200}"#;
        return Ok(Arc::new(ScriptedLlm::new(
            "scripted",
            (0..64).map(|_| ScriptedResponse::text(invoice)).collect::<Vec<_>>(),
        )));
    }
    let env = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    if env.exists() {
        dotenvy::from_path_override(&env)?;
    }
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
        return Ok(Arc::new(AnthropicClient::new(AnthropicConfig::new(key, "claude-sonnet-5-5"))?));
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        use adk_model::openai::{OpenAIClient, OpenAIConfig};
        return Ok(Arc::new(OpenAIClient::new(OpenAIConfig::new(key, "gpt-5.6-terra"))?));
    }
    if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
        return Ok(Arc::new(adk_model::GeminiModel::new(key, "gemini-3.5-flash-lite")?));
    }
    Err("--live needs ANTHROPIC_API_KEY, OPENAI_API_KEY or GOOGLE_API_KEY in the environment or .env".into())
}

/// The first JSON object in `text`, which a model may wrap in prose or a code fence.
fn first_object(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

/// The invoice-run graph agent over `checkpointer`, charging at `base`.
fn invoice_run(
    model: Arc<dyn Llm>,
    base: &str,
    checkpointer: Arc<dyn adk_graph::checkpoint::Checkpointer>,
) -> Result<GraphAgent, Box<dyn std::error::Error>> {
    let drafter = LlmAgentBuilder::new("draft")
        .description("Drafts the invoice the person asked for")
        .model(model)
        .instruction(
            "You draft invoices. Reply with only a JSON object {\"customer\": <name>, \
             \"amountCents\": <integer>} for the request; Acme's October invoice is 4200 cents.",
        )
        .build()?;
    let draft = AgentNode::new(Arc::new(drafter))
        .with_input_mapper(|state: &State| {
            let note = state.get("note").and_then(Value::as_str).unwrap_or("Bill Acme");
            Content::new("user").with_text(note)
        })
        .with_output_mapper(|events: &[Event]| {
            // The model's final answer: a streamed one arrives as partial chunks, then whole.
            let text: String = events
                .iter()
                .rev()
                .filter(|event| !event.llm_response.partial)
                .filter_map(|event| event.content())
                .map(|content| {
                    content.parts.iter().filter_map(|part| part.text()).collect::<String>()
                })
                .find(|text| !text.trim().is_empty())
                .unwrap_or_default();
            first_object(&text)
                .map(|invoice| [("invoice".to_string(), invoice)].into())
                .unwrap_or_default()
        });
    let standard = StandardProperties {
        id: "pay".to_string(),
        name: "pay".to_string(),
        description: Some("Charges the invoice".to_string()),
        position: None,
        error_handling: ErrorHandling {
            mode: ErrorMode::Stop,
            retry_count: None,
            retry_delay: None,
            fallback_value: None,
        },
        tracing: Tracing { enabled: false, log_level: LogLevel::None },
        callbacks: Callbacks { on_start: None, on_complete: None, on_error: None },
        execution: ExecutionControl { timeout: 10_000, condition: None },
        mapping: InputOutputMapping { input_mapping: None, output_key: "charge".to_string() },
    };
    // The body is the invoice only: a fork that edits the note sends the same call.
    let pay = ActionNodeExecutor::new(ActionNodeConfig::Http(HttpNodeConfig {
        standard,
        method: HttpMethod::Post,
        url: format!("{base}/charges"),
        auth: HttpAuth::None,
        headers: Default::default(),
        body: HttpBody::Json {
            data: json!({
                "customer": "{{invoice.customer}}",
                "amountCents": "{{invoice.amountCents}}",
            }),
        },
        response: HttpResponse { response_type: "json".to_string(), status_validation: None },
        rate_limit: None,
    }))
    .with_http_policy(
        HttpActionPolicy::new().allow_schemes(["http"]).allow_hosts(["127.0.0.1"]).max_redirects(0),
    );
    Ok(GraphAgent::builder(GRAPH)
        .description("Drafts an invoice, charges it once a person approves, files the receipt")
        .channels(&["invoice", "note", "charge", "receipt"])
        .node(draft)
        .node(pay)
        .node_fn("receipt", |ctx| async move {
            let id =
                ctx.get("charge").and_then(|charge| charge["data"]["id"].as_str()).unwrap_or("-");
            Ok(NodeOutput::new()
                .with_update("receipt", json!(format!("R-{}", id.trim_start_matches("ch_")))))
        })
        .edge(START, "draft")
        .conditional_edge(
            "draft",
            |state: &State| {
                let empty = state.get("invoice").is_none_or(|invoice| invoice.is_null());
                if empty { END.to_string() } else { "pay".to_string() }
            },
            [("pay", "pay"), (END, END)],
        )
        .edge("pay", "receipt")
        .edge("receipt", END)
        .governed_node("pay", GovernedNode::new().with_approval().spending_money())
        .checkpointer_arc(checkpointer)
        .input_mapper(|ctx| {
            let text: String =
                ctx.user_content().parts.iter().filter_map(|part| part.text()).collect();
            if text.is_empty() { State::new() } else { [("note".to_string(), json!(text))].into() }
        })
        .output_mapper(|state: &State| {
            let receipt = state.get("receipt").and_then(Value::as_str).unwrap_or("-");
            let mut event = Event::new("invoice-run");
            event.set_content(
                Content::new("assistant")
                    .with_text(format!("Charged. Receipt {receipt} is filed.")),
            );
            vec![event]
        })
        .build()?)
}

/// "Charge Acme 42.00 USD" for a charge; the tool's name for anything else.
fn heading(request: &ApprovalRequest) -> String {
    let body = &request.args_redacted["body"];
    let cents = match &body["amountCents"] {
        Value::String(text) => text.parse::<i64>().ok(),
        other => other.as_i64(),
    };
    match (request.tool_name.as_str(), body["customer"].as_str(), cents) {
        ("pay", Some(customer), Some(cents)) => {
            format!("Charge {customer} {}.{:02} USD", cents / 100, cents % 100)
        }
        _ => request.tool_name.clone(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    adk_core::ensure_crypto_provider();
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().map_or(Ok(8795), |port| port.parse())?;
    let workspace = args.next().map_or_else(std::env::temp_dir, PathBuf::from);
    let live = args.any(|arg| arg == "--live");
    std::fs::create_dir_all(&workspace)?;
    let url = |name: &str| format!("sqlite://{}?mode=rwc", workspace.join(name).display());

    let sessions = SqliteSessionService::new(&url("sessions.db")).await?;
    sessions.migrate().await?;
    let sessions: Arc<dyn SessionService> = Arc::new(sessions);
    let checkpointer = Arc::new(SqliteCheckpointer::new(&url("checkpoints.db")).await?);
    let ledger = SqliteActionLedger::new(&url("ledger.db")).await?;
    ledger.migrate().await?;
    let inbox = SqliteApprovalStore::new(&url("approvals.db")).await?;
    inbox.migrate().await?;

    let graph =
        Arc::new(invoice_run(model(live)?, &format!("http://127.0.0.1:{port}"), checkpointer)?);
    let workflows = Workflows::new(APP).graph(Arc::clone(&graph)).sessions(Arc::clone(&sessions));
    let approvals = Arc::new(StoreApprovalService::new(Arc::new(inbox)).with_router(
        workflows.approval_router(
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        ),
    ));
    let governance = GovernanceControl::new();
    let runner = Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(graph.clone())
            .session_service(Arc::clone(&sessions))
            .governance(governance.clone())
            .run_config(
                RunConfig::builder()
                    .approval_service(approvals.clone())
                    .action_ledger(Arc::new(ledger))
                    .build(),
            )
            .build()?,
    );
    let workflows = workflows.runner(Arc::clone(&runner));

    let source = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(approvals)
        .runner(runner)
        .governance(governance)
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(source))
        .places([Place::web()])
        .agent_name(APP, GRAPH)
        .agent_name(GRAPH, GRAPH)
        .heading(heading)
        .card_hints(Arc::new(workflows.clone()))
        .audit(AuditTrail::new(Arc::new(InMemoryAuditRecorder::new())))
        .build();

    let provider = Provider::default();
    let mock = Router::new()
        .route("/charges", post(charge))
        .route("/hits", get(hits))
        .with_state(provider);
    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(graph)), sessions)
        .with_request_context(Arc::new(Owner));
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_surface::web::router(engine.clone()))
        .add_api_routes(adk_surface::web::workflow::router(workflows, engine))
        .add_root_routes(adk_surface::web::home())
        .add_root_routes(mock)
        .build();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(port, "scripted workflow listening");
    println!("scripted workflow on http://127.0.0.1:{port}/ui/developer");
    axum::serve(listener, app).await?;
    Ok(())
}
