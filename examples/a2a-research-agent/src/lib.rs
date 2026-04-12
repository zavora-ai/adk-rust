//! A2A v1.0.0 Research Agent — LLM-powered research and summarization.
//!
//! Showcases the full ADK framework via the A2A v1 protocol:
//! LlmAgent → Runner → Sessions → RequestHandler → JSON-RPC + REST.

use std::sync::Arc;

use a2a_protocol_types::{AgentCapabilities, AgentSkill};
use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Llm};
use adk_runner::RunnerConfig;
use adk_server::a2a::v1::card::{CachedAgentCard, build_v1_agent_card};
use adk_server::a2a::v1::executor::V1Executor;
use adk_server::a2a::v1::jsonrpc_handler::jsonrpc_handler;
use adk_server::a2a::v1::push::NoOpPushNotificationSender;
use adk_server::a2a::v1::request_handler::RequestHandler;
use adk_server::a2a::v1::rest_handler::rest_router;
use adk_server::a2a::v1::task_store::InMemoryTaskStore;
use adk_server::a2a::v1::version::version_negotiation;
use adk_session::InMemorySessionService;
use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use tokio::sync::RwLock;

pub const RESEARCH_SYSTEM_INSTRUCTION: &str = "\
You are a research assistant. When given a topic, produce a structured summary with:\n\
\n\
1. **Key Findings**: The most important discoveries or facts about the topic.\n\
2. **Main Points**: A detailed breakdown of the core aspects, organized logically.\n\
3. **Brief Conclusion**: A concise wrap-up synthesizing the key takeaways.\n\
\n\
Be thorough, factual, and well-organized. Use clear headings and bullet points where appropriate.";

/// Detects the LLM provider from environment variables.
pub fn detect_model() -> Result<(Arc<dyn Llm>, &'static str), Box<dyn std::error::Error>> {
    if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
        let model = adk_model::GeminiModel::new(&key, "gemini-3.1-flash-lite-preview")?;
        return Ok((Arc::new(model), "Gemini (gemini-3.1-flash-lite-preview)"));
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let config = adk_model::openai::OpenAIConfig::new(key, "gpt-4o-mini");
        let model = adk_model::openai::OpenAIClient::new(config)?;
        return Ok((Arc::new(model), "OpenAI (gpt-4o-mini)"));
    }
    Err("No LLM provider found. Set GOOGLE_API_KEY (Gemini) or OPENAI_API_KEY (OpenAI).".into())
}

/// Builds the LlmAgent for the research agent.
pub fn build_research_agent(
    model: Arc<dyn Llm>,
) -> Result<Arc<dyn Agent>, Box<dyn std::error::Error>> {
    let agent = LlmAgentBuilder::new("research-agent")
        .description("Researches topics and produces structured summaries")
        .model(model)
        .instruction(RESEARCH_SYSTEM_INSTRUCTION)
        .build()
        .map_err(|e| format!("Failed to build research agent: {e}"))?;
    Ok(Arc::new(agent))
}

/// Builds the Axum router with A2A v1 routes and Runner integration.
///
/// - `POST /jsonrpc` — JSON-RPC handler (all 11 v1 operations)
/// - REST routes for all operations
/// - `GET /.well-known/agent-card.json` — agent card with ETag caching
/// - Version negotiation middleware on all A2A routes
/// - `RequestHandler::with_runner` for real LLM invocation
pub fn build_server(agent: Arc<dyn Agent>, base_url: &str) -> Router {
    let session_service: Arc<dyn adk_session::SessionService> =
        Arc::new(InMemorySessionService::new());
    let task_store = Arc::new(InMemoryTaskStore::new());
    let executor = Arc::new(V1Executor::new(task_store.clone()));
    let push_sender: Arc<dyn adk_server::a2a::v1::push::PushNotificationSender> =
        Arc::new(NoOpPushNotificationSender);

    let jsonrpc_url = format!("{base_url}/jsonrpc");
    let card = build_v1_agent_card(
        "research-agent",
        "A2A v1.0.0 LLM-powered research agent that summarizes topics",
        &jsonrpc_url,
        "1.0.0",
        vec![AgentSkill {
            id: "research".to_string(),
            name: "Research & Summarize".to_string(),
            description: "Researches a topic and produces a structured summary".to_string(),
            tags: vec!["research".to_string(), "summary".to_string()],
            examples: None,
            input_modes: None,
            output_modes: None,
            security_requirements: None,
        }],
        AgentCapabilities::none()
            .with_streaming(true)
            .with_push_notifications(true),
    );
    let cached_card = Arc::new(RwLock::new(CachedAgentCard::new(card)));

    let runner_config = Arc::new(RunnerConfig {
        app_name: agent.name().to_string(),
        agent,
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    });

    let handler = Arc::new(RequestHandler::with_runner(
        executor,
        task_store,
        push_sender,
        cached_card.clone(),
        runner_config,
    ));

    let jsonrpc_route = Router::new()
        .route("/jsonrpc", post(jsonrpc_handler))
        .with_state(handler.clone());

    let a2a_routes = jsonrpc_route
        .merge(rest_router(handler))
        .layer(axum::middleware::from_fn(version_negotiation));

    let card_route = Router::new().route(
        "/.well-known/agent-card.json",
        get(move |headers: axum::http::HeaderMap| {
            let cached = cached_card.clone();
            async move { serve_agent_card(headers, cached).await }
        }),
    );

    a2a_routes.merge(card_route)
}

async fn serve_agent_card(
    headers: axum::http::HeaderMap,
    cached_card: Arc<RwLock<CachedAgentCard>>,
) -> impl IntoResponse {
    let card = cached_card.read().await;

    if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH) {
        if let Ok(value) = if_none_match.to_str() {
            if card.matches_etag(value) {
                return StatusCode::NOT_MODIFIED.into_response();
            }
        }
    }

    let etag = format!("\"{}\"", card.etag);
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("application/json")),
            (header::ETAG, HeaderValue::from_str(&etag).unwrap_or_else(|_| HeaderValue::from_static(""))),
        ],
        card.card_json.clone(),
    )
        .into_response()
}
