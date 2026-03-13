mod cli;
mod deploy;
mod setup;
mod skills;

use adk_agent::LlmAgentBuilder;
use adk_cli::{Launcher, launcher::ThinkingDisplayMode};
use adk_core::Llm;
use adk_model::ModelProvider;
use adk_tool::GoogleSearchTool;
use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, ThinkingMode};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Chat) => {
            let agent = build_agent(
                cli.provider,
                cli.model,
                cli.api_key,
                cli.instruction,
                cli.thinking_budget,
            )?;
            Launcher::new(Arc::new(agent))
                .app_name("adk-rust")
                .with_thinking_mode(map_thinking_mode(cli.thinking_mode))
                .run_console_directly()
                .await
                .map_err(Into::into)
        }
        Some(Commands::Serve { port }) => {
            let agent = build_agent(
                cli.provider,
                cli.model,
                cli.api_key,
                cli.instruction,
                cli.thinking_budget,
            )?;
            Launcher::new(Arc::new(agent))
                .app_name("adk-rust")
                .run_serve_directly(port)
                .await
                .map_err(Into::into)
        }
        Some(Commands::Skills { command }) => skills::run(command),
        Some(Commands::Deploy { command }) => deploy::run(command).await,
    }
}

fn build_agent(
    cli_provider: Option<ModelProvider>,
    cli_model: Option<String>,
    cli_api_key: Option<String>,
    cli_instruction: Option<String>,
    thinking_budget: Option<u32>,
) -> Result<adk_agent::LlmAgent> {
    let resolved = setup::resolve(cli_provider, cli_model, cli_api_key, cli_instruction)?;
    let model = create_model(
        resolved.provider,
        &resolved.model,
        resolved.api_key.as_deref(),
        thinking_budget,
    )?;

    let mut builder = LlmAgentBuilder::new("adk_agent")
        .description("Default ADK-Rust CLI agent")
        .instruction(resolved.instruction)
        .model(model);

    // Google Search grounding only works with Gemini
    if resolved.provider == ModelProvider::Gemini {
        builder = builder.tool(Arc::new(GoogleSearchTool::new()));
    }

    builder.build().map_err(Into::into)
}

fn create_model(
    provider: ModelProvider,
    model: &str,
    api_key: Option<&str>,
    thinking_budget: Option<u32>,
) -> Result<Arc<dyn Llm>> {
    match provider {
        ModelProvider::Gemini => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Gemini requires an API key"))?;
            let m = adk_model::GeminiModel::new(key, model)?;
            Ok(Arc::new(m))
        }
        ModelProvider::Openai => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("OpenAI requires an API key"))?;
            let config = adk_model::OpenAIConfig::new(key, model);
            let m = adk_model::OpenAIClient::new(config)?;
            Ok(Arc::new(m))
        }
        ModelProvider::Anthropic => {
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Anthropic requires an API key"))?;
            let mut config = adk_model::anthropic::AnthropicConfig::new(key, model);
            if let Some(budget) = thinking_budget {
                if budget == 0 {
                    return Err(anyhow::anyhow!("--thinking-budget must be greater than 0"));
                }
                config = config.with_thinking(budget);
            }
            let m = adk_model::AnthropicClient::new(config)?;
            Ok(Arc::new(m))
        }
        ModelProvider::Deepseek => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("DeepSeek requires an API key"))?;
            let config = adk_model::DeepSeekConfig::new(key, model);
            let m = adk_model::DeepSeekClient::new(config)?;
            Ok(Arc::new(m))
        }
        ModelProvider::Groq => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Groq requires an API key"))?;
            let config = adk_model::GroqConfig::new(key, model);
            let m = adk_model::GroqClient::new(config)?;
            Ok(Arc::new(m))
        }
        ModelProvider::Ollama => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let config = adk_model::OllamaConfig::new(model);
            let m = adk_model::OllamaModel::new(config)?;
            Ok(Arc::new(m))
        }
    }
}

fn reject_unsupported_thinking_budget(
    provider: ModelProvider,
    thinking_budget: Option<u32>,
) -> Result<()> {
    if thinking_budget.is_some() {
        Err(anyhow::anyhow!("--thinking-budget is not supported for provider {}", provider))
    } else {
        Ok(())
    }
}

fn map_thinking_mode(mode: ThinkingMode) -> ThinkingDisplayMode {
    match mode {
        ThinkingMode::Auto => ThinkingDisplayMode::Auto,
        ThinkingMode::Show => ThinkingDisplayMode::Show,
        ThinkingMode::Hide => ThinkingDisplayMode::Hide,
    }
}
