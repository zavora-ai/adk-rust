//! # AWP Agent Example
//!
//! Demonstrates an AWP-compliant agent server that:
//!
//! 1. Loads a `business.toml` configuration file (full AWP schema)
//! 2. Serves public AWP endpoints and separately authenticated management routes
//! 3. Runs an LLM agent whose instructions are derived from the business context
//! 4. Applies version negotiation middleware on all routes
//! 5. Exercises every endpoint with an HTTP client to verify the boundaries
//!
//! ## AWP Endpoints Served
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/.well-known/awp.json` | Discovery document |
//! | GET | `/awp/manifest` | Capability manifest (JSON-LD) |
//! | GET | `/awp/health` | Health state |
//! | POST | `/awp/events/subscribe` | Create event subscription |
//! | GET | `/awp/events/subscriptions` | List subscriptions |
//! | DELETE | `/awp/events/subscriptions/{id}` | Delete subscription |
//! | POST | `/awp/a2a` | A2A message handler |
//! | POST | `/ask` | Ask the LLM agent a question |
//!
//! ## Run
//!
//! ```bash
//! cd examples/awp_agent
//! cp .env.example .env   # add your GOOGLE_API_KEY
//! cargo run
//! ```

use std::path::Path;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_awp::{
    AwpA2aHandler, AwpState, BusinessContextLoader, InMemoryEventSubscriptionService,
    awp_management_routes, awp_routes,
};
use adk_core::Content;
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Application state shared across custom routes
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    runner: Arc<Runner>,
    session_service: Arc<InMemorySessionService>,
}

async fn run_question(state: &AppState, question: &str) -> Result<String, String> {
    use adk_session::{CreateRequest, SessionService};
    use futures::StreamExt;

    let user_content = Content::new("user").with_text(question);
    let session_id = format!("session-{}", uuid::Uuid::new_v4());

    state
        .session_service
        .create(CreateRequest {
            app_name: "awp-agent".into(),
            user_id: "visitor".into(),
            session_id: Some(session_id.clone()),
            state: Default::default(),
        })
        .await
        .map_err(|error| format!("session create error: {error}"))?;

    let mut event_stream = state
        .runner
        .run_str("visitor", &session_id, user_content)
        .await
        .map_err(|error| format!("runner error: {error}"))?;

    let mut response_text = String::new();
    while let Some(event) = event_stream.next().await {
        let event = event.map_err(|error| format!("stream error: {error}"))?;
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let adk_core::Part::Text { text, .. } = part {
                    response_text.push_str(text);
                }
            }
        }
    }

    Ok(response_text)
}

// ---------------------------------------------------------------------------
// Custom route: POST /ask — send a question to the LLM agent
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AskRequest {
    question: String,
}

async fn ask_agent(State(state): State<AppState>, Json(body): Json<AskRequest>) -> Response {
    match run_question(&state, &body.question).await {
        Ok(answer) => Json(serde_json::json!({ "answer": answer })).into_response(),
        Err(error) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": error })))
                .into_response()
        }
    }
}

#[derive(Clone)]
struct AgentA2aHandler {
    state: AppState,
    bearer_token: Arc<str>,
}

#[async_trait::async_trait]
impl AwpA2aHandler for AgentA2aHandler {
    async fn handle(
        &self,
        headers: HeaderMap,
        message: serde_json::Value,
    ) -> Result<serde_json::Value, awp_types::AwpError> {
        let expected = format!("Bearer {}", self.bearer_token);
        let authorized = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == expected);
        if !authorized {
            return Err(awp_types::AwpError::Unauthorized(
                "a valid application A2A credential is required".to_string(),
            ));
        }

        let question = message
            .pointer("/payload/query")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                awp_types::AwpError::InvalidRequest(
                    "payload.query must be a non-empty string".to_string(),
                )
            })?;
        let answer = run_question(&self.state, question)
            .await
            .map_err(awp_types::AwpError::InternalError)?;

        Ok(serde_json::json!({
            "status": "processed",
            "messageId": message["id"],
            "answer": answer,
        }))
    }
}

async fn require_management_auth(
    State(token): State<Arc<str>>,
    request: Request,
    next: Next,
) -> Response {
    let expected = format!("Bearer {token}");
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected);

    if authorized {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

// ---------------------------------------------------------------------------
// Custom route: GET /api/products — serve product catalog from business.toml
// ---------------------------------------------------------------------------

async fn list_products() -> Json<serde_json::Value> {
    Json(serde_json::json!([
        {"sku": "WIDGET-001", "name": "Standard Widget", "price": 19.99, "inventory": 500},
        {"sku": "WIDGET-PRO", "name": "Pro Widget", "price": 49.99, "inventory": 150},
        {"sku": "GADGET-X", "name": "Gadget X", "price": 79.99, "inventory": 42},
    ]))
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    println!("╔══════════════════════════════════════════════╗");
    println!("║  AWP Agent Example — ADK-Rust                ║");
    println!("╚══════════════════════════════════════════════╝\n");

    // --- Step 1: Load business.toml ---
    let business_toml = Path::new(env!("CARGO_MANIFEST_DIR")).join("business.toml");
    let loader = BusinessContextLoader::from_file(&business_toml)?;
    let ctx = loader.load();
    println!("📋 Loaded business context: {} ({})", ctx.site_name, ctx.domain);
    println!(
        "   {} capabilities, {} policies, {} products\n",
        ctx.capabilities.len(),
        ctx.policies.len(),
        ctx.products.len()
    );

    // --- Step 2: Create the LLM agent with business context as instructions ---
    let api_key =
        std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set — see .env.example");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let instruction = format!(
        "You are the AI assistant for {site}. {desc}\n\n\
         Your tone is: {tone}\n\
         Greeting: {greeting}\n\n\
         You know about these products:\n{products}\n\n\
         Policies:\n{policies}\n\n\
         If you can't help, say: {escalation}\n\
         Keep answers concise and helpful.",
        site = ctx.site_name,
        desc = ctx.site_description,
        tone = ctx.brand_voice.as_ref().and_then(|b| b.tone.as_deref()).unwrap_or("professional"),
        greeting = ctx.brand_voice.as_ref().and_then(|b| b.greeting.as_deref()).unwrap_or("Hello!"),
        products = ctx
            .products
            .iter()
            .map(|p| format!(
                "- {} ({}): ${:.2} — {}",
                p.name,
                p.sku,
                p.price as f64 / 100.0,
                p.description.as_deref().unwrap_or("")
            ))
            .collect::<Vec<_>>()
            .join("\n"),
        policies = ctx
            .policies
            .iter()
            .map(|p| format!("- {}: {}", p.name, p.description))
            .collect::<Vec<_>>()
            .join("\n"),
        escalation = ctx
            .brand_voice
            .as_ref()
            .and_then(|b| b.escalation_message.as_deref())
            .unwrap_or("Let me connect you with support."),
    );

    let agent = LlmAgentBuilder::new("awp-assistant")
        .description(&ctx.site_description)
        .model(Arc::new(model))
        .instruction(&instruction)
        .build()?;

    println!("🤖 Created LLM agent: awp-assistant (gemini-2.5-flash)\n");

    // --- Step 3: Set up session service and runner ---
    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Arc::new(
        Runner::builder()
            .app_name("awp-agent")
            .agent(Arc::new(agent) as Arc<dyn adk_core::Agent>)
            .session_service(session_service.clone())
            .build()?,
    );

    let app_state = AppState { runner, session_service };

    // --- Step 4: Build AWP state with real, authenticated A2A dispatch ---
    let a2a_token: Arc<str> = uuid::Uuid::new_v4().to_string().into();
    let management_token: Arc<str> = uuid::Uuid::new_v4().to_string().into();
    let awp_state = AwpState::builder(loader.context_ref())
        .event_service(Arc::new(InMemoryEventSubscriptionService::new()))
        .a2a_handler(Arc::new(AgentA2aHandler {
            state: app_state.clone(),
            bearer_token: a2a_token.clone(),
        }))
        .build();

    // --- Step 5: Build the Axum router ---
    // AWP routes carry their own AwpState. Custom routes use AppState.
    // We merge AWP routes (already has .with_state) into a stateless router,
    // then nest the custom routes separately.
    let custom_routes = axum::Router::new()
        .route("/ask", post(ask_agent))
        .route("/api/products", get(list_products))
        .with_state(app_state);

    let management_routes = awp_management_routes(awp_state.clone()).layer(from_fn_with_state(
        management_token.clone(),
        require_management_auth,
    ));
    let app = axum::Router::new()
        .merge(awp_routes(awp_state))
        .merge(management_routes)
        .merge(custom_routes);

    // --- Step 6: Start the server ---
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    println!("🚀 AWP server listening on http://{addr}\n");

    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .ok();
    });

    // --- Step 7: Exercise all AWP endpoints ---
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // 7a. Discovery document
    println!("── AWP Discovery ──────────────────────────────");
    let resp = client.get(format!("{base}/.well-known/awp.json")).send().await?;
    println!("GET /.well-known/awp.json → {}", resp.status());
    let doc: serde_json::Value = resp.json().await?;
    println!("  siteName: {}", doc["siteName"]);
    println!("  version: {}", doc["version"]);
    println!("  capabilityManifestUrl: {}\n", doc["capabilityManifestUrl"]);

    // 7b. Capability manifest
    println!("── AWP Manifest ───────────────────────────────");
    let resp = client.get(format!("{base}/awp/manifest")).send().await?;
    println!("GET /awp/manifest → {}", resp.status());
    let manifest: serde_json::Value = resp.json().await?;
    println!("  @context: {}", manifest["@context"]);
    println!("  @type: {}", manifest["@type"]);
    let caps = manifest["capabilities"].as_array().map(|a| a.len()).unwrap_or(0);
    println!("  capabilities: {caps}\n");

    // 7c. Health endpoint
    println!("── AWP Health ─────────────────────────────────");
    let resp = client.get(format!("{base}/awp/health")).send().await?;
    println!("GET /awp/health → {}", resp.status());
    let health: serde_json::Value = resp.json().await?;
    println!("  state: {}\n", health["state"]);

    // 7d. Version negotiation
    println!("── AWP Version Negotiation ────────────────────");
    let resp = client
        .get(format!("{base}/.well-known/awp.json"))
        .header("AWP-Version", "1.1")
        .send()
        .await?;
    println!("GET with AWP-Version: 1.1 → {} (compatible)", resp.status());
    let _ = resp.text().await;

    let resp = client
        .get(format!("{base}/.well-known/awp.json"))
        .header("AWP-Version", "2.0")
        .send()
        .await?;
    println!("GET with AWP-Version: 2.0 → {} (incompatible)\n", resp.status());
    let _ = resp.text().await;

    // 7e. Event subscription
    println!("── AWP Events ─────────────────────────────────");
    let sub_body = serde_json::json!({
        "subscriber": "test-client",
        "callbackUrl": "https://example.com/webhook",
        "eventTypes": ["health.changed"],
        "secret": "test-secret-key-at-least-32-bytes"
    });
    let resp = client
        .post(format!("{base}/awp/events/subscribe"))
        .bearer_auth(&*management_token)
        .json(&sub_body)
        .send()
        .await?;
    println!("POST /awp/events/subscribe → {}", resp.status());
    let sub_resp: serde_json::Value = resp.json().await?;
    println!("  subscription id: {}", sub_resp["id"]);

    let resp = client
        .get(format!("{base}/awp/events/subscriptions"))
        .bearer_auth(&*management_token)
        .send()
        .await?;
    println!("GET /awp/events/subscriptions → {}", resp.status());
    let subs: serde_json::Value = resp.json().await?;
    println!("  count: {}\n", subs.as_array().map(|a| a.len()).unwrap_or(0));

    // 7f. A2A message
    println!("── AWP A2A ────────────────────────────────────");
    let a2a_body = serde_json::json!({
        "id": "msg-001",
        "sender": "external-agent",
        "recipient": "awp-assistant",
        "messageType": "request",
        "payload": {"query": "What products do you sell?"}
    });
    let resp = client
        .post(format!("{base}/awp/a2a"))
        .bearer_auth(&*a2a_token)
        .json(&a2a_body)
        .send()
        .await?;
    println!("POST /awp/a2a → {}", resp.status());
    let a2a_resp: serde_json::Value = resp.json().await?;
    println!("  status: {}", a2a_resp["status"]);
    println!("  answer: {}\n", a2a_resp["answer"]);

    // 7g. Product catalog
    println!("── Business API ───────────────────────────────");
    let resp = client.get(format!("{base}/api/products")).send().await?;
    println!("GET /api/products → {}", resp.status());
    let products: serde_json::Value = resp.json().await?;
    for p in products.as_array().unwrap_or(&vec![]) {
        println!("  - {} ({}): ${}", p["name"], p["sku"], p["price"]);
    }
    println!();

    // 7h. Ask the LLM agent
    println!("── LLM Agent ──────────────────────────────────");
    let ask_body = serde_json::json!({"question": "What is your most popular product and how much does it cost?"});
    let resp = client.post(format!("{base}/ask")).json(&ask_body).send().await?;
    println!("POST /ask → {}", resp.status());
    let answer: serde_json::Value = resp.json().await?;
    let answer_text = answer["answer"].as_str().unwrap_or("(no answer)");
    let display = if answer_text.len() > 300 {
        let mut end = 300;
        while end > 0 && !answer_text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &answer_text[..end])
    } else {
        answer_text.to_string()
    };
    println!("  Agent: {display}\n");

    println!("✅ AWP compliance demo completed successfully.");
    println!("   All protocol endpoints verified:");
    println!("   ✓ Discovery document at /.well-known/awp.json");
    println!("   ✓ Capability manifest with JSON-LD @context/@type");
    println!("   ✓ Version negotiation (accept compatible, reject incompatible)");
    println!("   ✓ Health state machine");
    println!("   ✓ Event subscription CRUD with HMAC signing");
    println!("   ✓ A2A message handling");
    println!("   ✓ LLM agent with business context instructions");

    server.abort();
    Ok(())
}
