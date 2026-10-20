#[cfg(feature = "audit")]
mod audit;
mod cli;
mod deploy;
mod graph;
#[cfg(feature = "harness")]
mod harness_cli;
#[cfg(not(feature = "harness"))]
mod legacy;
#[cfg(all(feature = "harness", feature = "persist"))]
mod manage;
#[cfg(feature = "vertex-agent-registry")]
mod registry;
#[cfg(feature = "scripted-model")]
mod scripted;
#[cfg(feature = "persist")]
mod sessions;
mod setup;
mod skills;
mod ultra;

use adk_agent::LlmAgentBuilder;
use adk_cli::{Launcher, launcher::ThinkingDisplayMode};
use adk_core::Llm;
use adk_model::ModelProvider;
use adk_tool::GoogleSearchTool;
use anyhow::Result;
use clap::Parser;
use cli::{AgentOptions, ChatArgs, Cli, Commands, ThinkingMode};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(feature = "harness")]
    let Cli { command, options, harness } = Cli::parse();
    #[cfg(not(feature = "harness"))]
    let Cli { command, options } = Cli::parse();

    match command {
        #[cfg(feature = "harness")]
        None => {
            let model = resolve_model(&options)?;
            harness_cli::repl(model, options.instruction.clone(), harness).await
        }
        #[cfg(not(feature = "harness"))]
        None => run_chat(&options, ChatArgs::default()).await,
        Some(Commands::Chat(args)) => run_chat(&options, args).await,
        #[cfg(feature = "harness")]
        Some(Commands::Serve {
            port,
            host,
            agent: cli::ServeAgent::Harness,
            auth_token,
            harness,
        }) => {
            let model = resolve_model(&options)?;
            let auth_token = auth_token.or_else(|| std::env::var("ADK_SERVE_TOKEN").ok());
            harness_cli::serve(model, options.instruction.clone(), harness, port, host, auth_token)
                .await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Serve { port, host, agent: cli::ServeAgent::Chat, .. }) => {
            serve_chat(&options, port, host).await
        }
        #[cfg(not(feature = "harness"))]
        Some(Commands::Serve { port, host }) => serve_chat(&options, port, host).await,
        #[cfg(feature = "harness")]
        Some(Commands::Code { task, harness, read_only }) => {
            let model = resolve_model(&options)?;
            exit_with(
                harness_cli::code(model, options.instruction.clone(), harness, read_only, task)
                    .await?,
            )
        }
        #[cfg(not(feature = "harness"))]
        Some(Commands::Code { task, dir, read_only }) => {
            let model = resolve_model(&options)?;
            legacy::run_code(model, options.instruction, dir, read_only, task).await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Goal { goal, until, harness, max_iters, state, resume }) => {
            let model = resolve_model(&options)?;
            let goal = harness_cli::GoalArgs { goal, until, max_iters, state, resume };
            exit_with(harness_cli::goal(model, options.instruction.clone(), harness, goal).await?)
        }
        #[cfg(not(feature = "harness"))]
        Some(Commands::Goal { goal, until, dir, max_iters, state, resume }) => {
            let model = resolve_model(&options)?;
            legacy::run_goal(legacy::GoalArgs {
                model,
                instruction: options.instruction,
                dir,
                goal,
                until,
                max_iters,
                state,
                resume,
            })
            .await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Ultracode { task, harness, max_rounds }) => {
            let model = resolve_model(&options)?;
            exit_with(
                harness_cli::ultracode(
                    model,
                    options.instruction.clone(),
                    harness,
                    task,
                    max_rounds,
                )
                .await?,
            )
        }
        #[cfg(not(feature = "harness"))]
        Some(Commands::Ultracode { task, dir, max_rounds }) => {
            let model = resolve_model(&options)?;
            let agents = Arc::new(ultra::legacy::CodingAgents {
                model: model.llm,
                root: std::path::PathBuf::from(&dir),
                instruction: options.instruction,
            });
            ultra::run_with(agents, &model.id, &dir, task, max_rounds).await
        }
        #[cfg(all(feature = "harness", feature = "persist"))]
        Some(Commands::Sessions { command }) => manage::sessions(command).await,
        #[cfg(all(feature = "harness", feature = "persist"))]
        Some(Commands::Rules { command }) => manage::rules(command).await,
        #[cfg(all(feature = "harness", feature = "persist"))]
        Some(Commands::Memory { command }) => manage::memory(command).await,
        #[cfg(all(feature = "harness", feature = "persist"))]
        Some(Commands::Approvals { command, server, token }) => {
            let token = token.or_else(|| std::env::var("ADK_SERVE_TOKEN").ok());
            manage::approvals(command, server, token).await
        }
        Some(Commands::Skills { command }) => skills::run(command).await,
        Some(Commands::Deploy { command }) => deploy::run(command).await,
        #[cfg(feature = "vertex-agent-registry")]
        Some(Commands::Registry { command }) => registry::run(command).await,
        Some(Commands::Graph { command }) => graph::run(command).await,
        #[cfg(feature = "audit")]
        Some(Commands::Audit { command }) => audit::run(command).await,
    }
}

/// Exits with `code` when it is not success; harness commands report held calls with 3.
#[cfg(feature = "harness")]
fn exit_with(code: i32) -> Result<()> {
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// `serve` with the plain chat agent.
async fn serve_chat(options: &AgentOptions, port: u16, host: std::net::IpAddr) -> Result<()> {
    let agent = build_agent(options)?;
    Launcher::new(Arc::new(agent))
        .app_name("adk-rust")
        .with_bind_address(host)
        .run_serve_directly(port)
        .await
        .map_err(Into::into)
}

/// The model an agent command runs on, resolved through [`setup::resolve`].
pub(crate) struct ResolvedModel {
    pub(crate) llm: Arc<dyn Llm>,
    /// Display id, derived from the flags or saved config only, never from the key.
    pub(crate) id: String,
    provider: Option<ModelProvider>,
    /// The chat instruction: `--instruction`, or the default one.
    instruction: String,
    /// The provider key, for the long-term memory embedder (spec 3.1 §7.15.2).
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    api_key: Option<String>,
}

/// Resolves the model from the flags, saved config, secure storage, or first-run setup
/// (spec 3.1 §8.1).
fn resolve_model(options: &AgentOptions) -> Result<ResolvedModel> {
    #[cfg(feature = "scripted-model")]
    if let Some(script) = &options.model_script {
        return Ok(ResolvedModel {
            llm: scripted::model(script)?,
            id: "scripted".to_string(),
            provider: None,
            instruction: options.instruction.clone().unwrap_or_else(setup::default_instruction),
            api_key: None,
        });
    }
    let resolved = setup::resolve(
        options.provider,
        options.model.clone(),
        options.api_key.clone(),
        options.instruction.clone(),
    )?;
    let llm = create_model(
        resolved.provider,
        &resolved.model,
        resolved.api_key.as_deref(),
        options.thinking_budget,
    )?;
    Ok(ResolvedModel {
        llm,
        id: resolved.model,
        provider: Some(resolved.provider),
        instruction: resolved.instruction,
        api_key: resolved.api_key,
    })
}

/// The interactive console, with its conversation kept per [`ChatArgs`].
async fn run_chat(options: &AgentOptions, args: ChatArgs) -> Result<()> {
    let agent = build_agent(options)?;
    let launcher = Launcher::new(Arc::new(agent))
        .app_name("adk-rust")
        .with_thinking_mode(map_thinking_mode(options.thinking_mode));
    with_chat_sessions(launcher, args).await?.run_console_directly().await.map_err(Into::into)
}

/// Keeps the conversation in `harness.db` unless `--ephemeral` (spec 3.1 §8.1).
#[cfg(feature = "persist")]
async fn with_chat_sessions(launcher: Launcher, args: ChatArgs) -> Result<Launcher> {
    if args.ephemeral {
        return Ok(launcher);
    }
    let store = sessions::open().await?;
    let user_id = sessions::os_user();
    let session_id = match args.session {
        Some(session_id) => Some(session_id),
        None if args.continue_session => {
            let latest = sessions::latest(store.as_ref(), &user_id).await?;
            if latest.is_none() {
                println!("No earlier session to continue; starting a new one.");
            }
            latest
        }
        None => None,
    };
    let launcher = launcher.with_session_service(store).with_user_id(user_id);
    Ok(match session_id {
        Some(session_id) => launcher.with_session_id(session_id),
        None => launcher,
    })
}

#[cfg(not(feature = "persist"))]
async fn with_chat_sessions(launcher: Launcher, args: ChatArgs) -> Result<Launcher> {
    if args.session.is_some() || args.continue_session {
        anyhow::bail!(
            "persistent sessions are not compiled into this adk-cli build. Reinstall with \
             `--features persist`, or start without --session and --continue."
        );
    }
    Ok(launcher)
}

fn build_agent(options: &AgentOptions) -> Result<adk_agent::LlmAgent> {
    let model = resolve_model(options)?;

    let mut builder = LlmAgentBuilder::new("adk_agent")
        .description("Default ADK-Rust CLI agent")
        .instruction(model.instruction)
        .model(model.llm);

    // Google Search grounding only works with Gemini
    if model.provider == Some(ModelProvider::Gemini) {
        builder = builder.tool(Arc::new(GoogleSearchTool::new()));
    }

    #[cfg(feature = "scripted-model")]
    if options.approval_fixture {
        builder = builder
            .tool(Arc::new(scripted::RecordNote))
            .require_tool_confirmation(scripted::RecordNote::NAME);
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
        ModelProvider::Gemini => Err(setup::not_compiled(provider)),
        #[cfg(feature = "openai")]
        ModelProvider::Openai => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("OpenAI requires an API key"))?;
            let config = adk_model::OpenAIConfig::new(key, model);
            let m = adk_model::OpenAIClient::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "openai"))]
        ModelProvider::Openai => Err(setup::not_compiled(provider)),
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
        ModelProvider::Anthropic => Err(setup::not_compiled(provider)),
        #[cfg(feature = "deepseek")]
        ModelProvider::Deepseek => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("DeepSeek requires an API key"))?;
            let config = adk_model::DeepSeekConfig::new(key, model);
            let m = adk_model::DeepSeekClient::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "deepseek"))]
        ModelProvider::Deepseek => Err(setup::not_compiled(provider)),
        #[cfg(feature = "groq")]
        ModelProvider::Groq => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Groq requires an API key"))?;
            let config = adk_model::GroqConfig::new(key, model);
            let m = adk_model::GroqClient::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "groq"))]
        ModelProvider::Groq => Err(setup::not_compiled(provider)),
        #[cfg(feature = "ollama")]
        ModelProvider::Ollama => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let config = adk_model::OllamaConfig::new(model);
            let m = adk_model::OllamaModel::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "ollama"))]
        ModelProvider::Ollama => Err(setup::not_compiled(provider)),
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
