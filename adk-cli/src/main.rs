mod cli;
mod deploy;
mod graph;
mod setup;
mod skills;

use adk_agent::LlmAgentBuilder;
use adk_agent::coding::CodingAgent;
use adk_cli::{Launcher, launcher::ThinkingDisplayMode};
use adk_core::{Content, Llm, Part, SessionId, UserId};
use adk_devtools::Workspace;
use adk_model::ModelProvider;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::GoogleSearchTool;
use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, ThinkingMode};
use futures::StreamExt;
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
        Some(Commands::Code { task, dir, read_only }) => {
            run_code(
                cli.provider,
                cli.model,
                cli.api_key,
                cli.thinking_budget,
                dir,
                read_only,
                task,
            )
            .await
        }
        Some(Commands::Skills { command }) => skills::run(command),
        Some(Commands::Deploy { command }) => deploy::run(command).await,
        Some(Commands::Graph { command }) => graph::run(command).await,
    }
}

/// Run the coding agent on a single task in a workspace directory.
#[allow(clippy::too_many_arguments)]
async fn run_code(
    cli_provider: Option<ModelProvider>,
    cli_model: Option<String>,
    cli_api_key: Option<String>,
    thinking_budget: Option<u32>,
    dir: String,
    read_only: bool,
    task: String,
) -> Result<()> {
    // Resolve provider/model/key non-interactively (no setup prompt): default to
    // a Gemini 3 model, and read the key from --api-key or the environment.
    let provider = cli_provider.unwrap_or(ModelProvider::Gemini);
    let model_id = cli_model.unwrap_or_else(|| match provider {
        ModelProvider::Gemini => "gemini-3.1-flash-lite".to_string(),
        _ => provider.default_model().to_string(),
    });
    let api_key = cli_api_key.or_else(|| env_api_key(provider));
    let model = create_model(provider, &model_id, api_key.as_deref(), thinking_budget)?;

    let workspace = if read_only { Workspace::read_only(&dir) } else { Workspace::new(&dir) };
    let coding = CodingAgent::builder().model(model).workspace(workspace).build()?;

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "adk-rust".into(),
            user_id: "user".into(),
            session_id: Some("code".into()),
            state: Default::default(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("adk-rust")
        .agent(coding.agent())
        .session_service(sessions)
        .build()?;

    println!("coding agent ({model_id}) on {dir}\ntask: {task}\n");

    let mut stream = runner
        .run(UserId::new("user")?, SessionId::new("code")?, Content::new("user").with_text(task))
        .await?;

    let mut pending = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, args, .. } => {
                        flush_text(&mut pending);
                        println!("  🔧 {name}({})", first_line(&args.to_string()));
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        flush_text(&mut pending);
                        println!("  ↩  {}", first_line(&function_response.response.to_string()));
                    }
                    Part::Text { text } if !text.is_empty() => pending.push_str(text),
                    _ => {}
                }
            }
        }
    }
    flush_text(&mut pending);

    let todos = coding.todos();
    if !todos.is_empty() {
        println!("\nplan:");
        for t in todos {
            let mark = match t.status.as_str() {
                "completed" => "✓",
                "in_progress" => "▶",
                _ => "·",
            };
            println!("  {mark} {}", t.content);
        }
    }
    Ok(())
}

/// Read the API key for a provider from its conventional environment variable.
fn env_api_key(provider: ModelProvider) -> Option<String> {
    let try_vars: &[&str] = match provider {
        ModelProvider::Gemini => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        ModelProvider::Openai => &["OPENAI_API_KEY"],
        ModelProvider::Anthropic => &["ANTHROPIC_API_KEY"],
        ModelProvider::Deepseek => &["DEEPSEEK_API_KEY"],
        ModelProvider::Groq => &["GROQ_API_KEY"],
        ModelProvider::Ollama => &[],
    };
    try_vars.iter().find_map(|v| std::env::var(v).ok())
}

fn flush_text(pending: &mut String) {
    let trimmed = pending.trim();
    if !trimmed.is_empty() {
        println!("  🤖 {trimmed}");
    }
    pending.clear();
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.len() > 160 { format!("{}…", &line[..160]) } else { line.to_string() }
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
        #[cfg(feature = "gemini")]
        ModelProvider::Gemini => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Gemini requires an API key"))?;
            let m = adk_model::GeminiModel::new(key, model)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "gemini"))]
        ModelProvider::Gemini => provider_feature_disabled(provider, "gemini"),
        #[cfg(feature = "openai")]
        ModelProvider::Openai => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("OpenAI requires an API key"))?;
            let config = adk_model::OpenAIConfig::new(key, model);
            let m = adk_model::OpenAIClient::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "openai"))]
        ModelProvider::Openai => provider_feature_disabled(provider, "openai"),
        #[cfg(feature = "anthropic")]
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
        #[cfg(not(feature = "anthropic"))]
        ModelProvider::Anthropic => provider_feature_disabled(provider, "anthropic"),
        #[cfg(feature = "deepseek")]
        ModelProvider::Deepseek => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("DeepSeek requires an API key"))?;
            let config = adk_model::DeepSeekConfig::new(key, model);
            let m = adk_model::DeepSeekClient::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "deepseek"))]
        ModelProvider::Deepseek => provider_feature_disabled(provider, "deepseek"),
        #[cfg(feature = "groq")]
        ModelProvider::Groq => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Groq requires an API key"))?;
            let config = adk_model::GroqConfig::new(key, model);
            let m = adk_model::GroqClient::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "groq"))]
        ModelProvider::Groq => provider_feature_disabled(provider, "groq"),
        #[cfg(feature = "ollama")]
        ModelProvider::Ollama => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let config = adk_model::OllamaConfig::new(model);
            let m = adk_model::OllamaModel::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "ollama"))]
        ModelProvider::Ollama => provider_feature_disabled(provider, "ollama"),
    }
}

fn provider_feature_disabled(provider: ModelProvider, feature: &str) -> Result<Arc<dyn Llm>> {
    Err(anyhow::anyhow!(
        "{} support is not compiled into this adk-cli build. Reinstall with `--features {}` or `--features all-providers`.",
        provider.display_name(),
        feature
    ))
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
