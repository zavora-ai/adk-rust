//! Shared helpers for the embedded runtime UI showcase.

use std::sync::Arc;

use adk_core::{Agent, Llm, SingleAgentLoader};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_server::{SecurityConfig, ServerConfig, create_app};
use adk_session::InMemorySessionService;

/// Creates the OpenAI model used by every showcase binary.
pub fn openai_model() -> anyhow::Result<Arc<dyn Llm>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow::anyhow!("set OPENAI_API_KEY to run this example"))?;
    let model_id = std::env::var("RUNTIME_UI_MODEL").unwrap_or_else(|_| "gpt-5-mini".to_string());
    println!("OpenAI model: {model_id}");
    Ok(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, model_id))?))
}

/// Serves one executable agent root through the built-in ADK runtime UI.
pub async fn serve(agent: Arc<dyn Agent>, example: &str) -> anyhow::Result<()> {
    adk_core::ensure_crypto_provider();
    let loader = Arc::new(SingleAgentLoader::new(agent));
    let sessions = Arc::new(InMemorySessionService::new());
    let span_exporter = adk_telemetry::init_with_adk_exporter(example)?;
    let config = ServerConfig::new(loader, sessions)
        .with_security(SecurityConfig::development())
        .with_span_exporter(span_exporter);
    let address = std::env::var("ADK_UI_ADDRESS").unwrap_or_else(|_| "127.0.0.1:8088".to_string());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("Runtime UI: http://{address}/ui/");
    axum::serve(listener, create_app(config)).await?;
    Ok(())
}
