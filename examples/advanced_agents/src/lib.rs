//! Shared model and process helpers for the advanced agent gallery.

use std::sync::Arc;

use adk_core::Llm;
use adk_model::{OpenAIClient, OpenAIConfig};

/// Loads the environment and returns the OpenAI API key.
pub fn openai_api_key() -> anyhow::Result<String> {
    dotenvy::dotenv().ok();
    std::env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow::anyhow!("set OPENAI_API_KEY to run the advanced agent gallery"))
}

/// Creates the OpenAI chat model shared by the request/response examples.
pub fn openai_chat_model(api_key: &str) -> anyhow::Result<Arc<dyn Llm>> {
    let model_id =
        std::env::var("ADVANCED_CHAT_MODEL").unwrap_or_else(|_| "gpt-5-mini".to_string());
    Ok(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, model_id))?))
}

/// Resolves the MCP helper binary built next to the advanced runtime binary.
pub fn mcp_server_command() -> anyhow::Result<tokio::process::Command> {
    let mut binary = std::env::current_exe()?;
    binary.pop();
    binary.push("advanced-mcp-server");
    if !binary.is_file() {
        return Err(anyhow::anyhow!(
            "MCP helper not found at {}. Build both binaries first with: cargo build --manifest-path examples/advanced_agents/Cargo.toml --bins",
            binary.display()
        ));
    }
    Ok(tokio::process::Command::new(binary))
}
