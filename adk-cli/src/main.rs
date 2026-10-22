#[cfg(feature = "audit")]
mod audit;
mod cli;
mod deploy;
mod graph;
#[cfg(feature = "harness")]
mod harness_cli;
#[cfg(feature = "harness")]
mod kernel_cli;
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
#[cfg(feature = "harness")]
mod shared_store;
mod skills;
#[cfg(all(feature = "harness", feature = "persist", feature = "encrypted"))]
mod store;
mod ultra;
#[cfg(feature = "voice")]
mod voice_cli;

use adk_agent::LlmAgentBuilder;
use adk_cli::{Launcher, launcher::ThinkingDisplayMode};
use adk_core::Llm;
use adk_model::ModelProvider;
use adk_tool::GoogleSearchTool;
use anyhow::Result;
use clap::Parser;
use cli::{AgentOptions, ChatArgs, Cli, Commands, Provider, ThinkingMode};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let parsed = Cli::parse();
    #[cfg(feature = "voice")]
    let voice = parsed.voice.clone();
    #[cfg(feature = "harness")]
    let Cli { command, options, harness, .. } = parsed;
    #[cfg(not(feature = "harness"))]
    let Cli { command, options } = parsed;

    match command {
        #[cfg(feature = "harness")]
        None => {
            #[cfg(feature = "voice")]
            let in_process = voice.voice.is_some();
            #[cfg(not(feature = "voice"))]
            let in_process = false;
            // A running kernel owns the sessions, so the REPL attaches to it; --ephemeral,
            // --continue and --voice keep the in-process REPL (spec 3.2 §7.11).
            if !in_process
                && !harness.ephemeral
                && !harness.continue_session
                && let Some(client) = kernel_cli::local().await?
            {
                return kernel_cli::repl(client, harness.session.clone()).await;
            }
            let model = resolve_model(&options).await?;
            #[cfg(feature = "voice")]
            if voice.voice.is_some() {
                return exit_with(
                    harness_cli::voice(model, options.instruction.clone(), harness, voice).await?,
                );
            }
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
            store,
            harness,
        }) => {
            let model = resolve_model(&options).await?;
            let auth_token = auth_token.or_else(|| std::env::var("ADK_SERVE_TOKEN").ok());
            let instruction = options.instruction.clone();
            harness_cli::serve(model, instruction, harness, port, host, auth_token, store).await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Serve { port, host, agent: cli::ServeAgent::Chat, .. }) => {
            serve_chat(&options, port, host).await
        }
        #[cfg(not(feature = "harness"))]
        Some(Commands::Serve { port, host }) => serve_chat(&options, port, host).await,
        #[cfg(feature = "harness")]
        Some(Commands::Code { task, harness, read_only }) => {
            let model = resolve_model(&options).await?;
            exit_with(
                harness_cli::code(model, options.instruction.clone(), harness, read_only, task)
                    .await?,
            )
        }
        #[cfg(not(feature = "harness"))]
        Some(Commands::Code { task, dir, read_only }) => {
            let model = resolve_model(&options).await?;
            legacy::run_code(model, options.instruction, dir, read_only, task).await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Goal { goal, until, harness, max_iters, check_timeout, state, resume }) => {
            let model = resolve_model(&options).await?;
            let check_timeout = std::time::Duration::from_secs(check_timeout);
            let goal =
                harness_cli::GoalArgs { goal, until, max_iters, check_timeout, state, resume };
            exit_with(harness_cli::goal(model, options.instruction.clone(), harness, goal).await?)
        }
        #[cfg(not(feature = "harness"))]
        Some(Commands::Goal { goal, until, dir, max_iters, state, resume }) => {
            let model = resolve_model(&options).await?;
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
        Some(Commands::Ultracode {
            task,
            harness,
            max_rounds,
            architecture,
            router,
            max_model_calls,
            receipt,
        }) => {
            let model = resolve_model(&options).await?;
            let ultra = harness_cli::UltracodeArgs {
                task,
                max_rounds,
                architecture,
                router,
                max_model_calls,
                receipt,
            };
            exit_with(
                harness_cli::ultracode(model, options.instruction.clone(), harness, ultra).await?,
            )
        }
        #[cfg(not(feature = "harness"))]
        Some(Commands::Ultracode { task, dir, max_rounds }) => {
            let model = resolve_model(&options).await?;
            let agents = Arc::new(ultra::legacy::CodingAgents {
                model: model.llm,
                root: std::path::PathBuf::from(&dir),
                instruction: options.instruction,
            });
            ultra::run_with(agents, &model.id, &dir, task, max_rounds.unwrap_or(2)).await.map(drop)
        }
        #[cfg(feature = "harness")]
        Some(Commands::Up(mut up)) => {
            let model = resolve_model(&options).await?;
            up.auth_token = up.auth_token.or_else(|| std::env::var("ADK_SERVE_TOKEN").ok());
            harness_cli::up(model, options.instruction.clone(), *up).await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Attach { session, kernel }) => kernel_cli::attach(&kernel, session).await,
        #[cfg(feature = "harness")]
        Some(Commands::Ps { partition, state, limit, kernel }) => {
            kernel_cli::ps(&kernel, partition, state, limit).await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Top { partition, interval, kernel }) => {
            kernel_cli::top(&kernel, partition, interval).await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Kill { pid, reason, kernel }) => {
            kernel_cli::act(&kernel, &pid, kernel_cli::Action::Kill(reason)).await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Suspend { pid, kernel }) => {
            kernel_cli::act(&kernel, &pid, kernel_cli::Action::Suspend).await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Resume { pid, kernel }) => {
            kernel_cli::act(&kernel, &pid, kernel_cli::Action::Resume).await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Freeze { reason, kernel }) => {
            kernel_cli::freeze(&kernel, reason.as_deref()).await
        }
        #[cfg(feature = "harness")]
        Some(Commands::Unfreeze { kernel }) => kernel_cli::unfreeze(&kernel).await,
        #[cfg(feature = "harness")]
        Some(Commands::Events { follow, after, json, kernel }) => {
            kernel_cli::events(&kernel, follow, after, json).await
        }
        #[cfg(all(feature = "harness", feature = "persist"))]
        Some(Commands::Sessions { command }) => manage::sessions(command).await,
        #[cfg(all(feature = "harness", feature = "persist"))]
        Some(Commands::Rules { command, store }) => manage::rules(command, store).await,
        #[cfg(all(feature = "harness", feature = "persist"))]
        Some(Commands::Memory { command }) => manage::memory(command).await,
        #[cfg(all(feature = "harness", feature = "persist", feature = "encrypted"))]
        Some(Commands::Store { command, store_key }) => store::run(command, store_key).await,
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
    let agent = build_agent(options).await?;
    Launcher::new(Arc::new(agent))
        .app_name("adk-rust")
        .with_bind_address(host)
        .run_serve_directly(port)
        .await
        .map_err(Into::into)
}

/// A model client, and the same model as a `CacheCapable` when it caches its context
/// explicitly (Gemini).
#[derive(Clone)]
pub(crate) struct BuiltModel {
    pub(crate) llm: Arc<dyn Llm>,
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    pub(crate) cache_capable: Option<Arc<dyn adk_core::CacheCapable>>,
}

/// The model an agent command runs on, resolved through [`setup::resolve`].
#[derive(Clone)]
pub(crate) struct ResolvedModel {
    pub(crate) llm: Arc<dyn Llm>,
    /// Display id, derived from the flags or saved config only, never from the key.
    pub(crate) id: String,
    provider: Option<Provider>,
    /// The chat instruction: `--instruction`, or the default one.
    instruction: String,
    /// The provider key, for the long-term memory embedder (spec 3.1 §7.15.2).
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    api_key: Option<String>,
    /// The model as a `CacheCapable`, when it caches its context explicitly.
    #[cfg_attr(not(feature = "harness"), allow(dead_code))]
    cache_capable: Option<Arc<dyn adk_core::CacheCapable>>,
}

impl ResolvedModel {
    /// The model's client; the harness sets each request's effort, so one client serves
    /// every mode and role (spec 3.1 §7.23).
    #[cfg(feature = "harness")]
    pub(crate) fn built(&self) -> BuiltModel {
        BuiltModel { llm: self.llm.clone(), cache_capable: self.cache_capable.clone() }
    }

    /// The provider, for the long-term memory embedder and the local posture.
    #[cfg(feature = "harness")]
    pub(crate) fn provider(&self) -> Option<Provider> {
        self.provider
    }
}

/// Resolves the model from the flags, saved config, secure storage, or first-run setup
/// (spec 3.1 §8.1).
async fn resolve_model(options: &AgentOptions) -> Result<ResolvedModel> {
    #[cfg(feature = "scripted-model")]
    if let Some(script) = &options.model_script {
        return Ok(ResolvedModel {
            llm: scripted::model(script)?,
            id: "scripted".to_string(),
            provider: None,
            instruction: options.instruction.clone().unwrap_or_else(setup::default_instruction),
            api_key: None,
            cache_capable: None,
        });
    }
    let resolved = setup::resolve(
        options.provider,
        options.model.clone(),
        options.api_key.clone(),
        options.instruction.clone(),
    )?;
    let built = create_model(
        resolved.provider,
        &resolved.model,
        resolved.api_key.as_deref(),
        options.thinking_budget,
    )
    .await?;
    Ok(ResolvedModel {
        llm: built.llm,
        id: resolved.model,
        provider: Some(resolved.provider),
        instruction: resolved.instruction,
        api_key: resolved.api_key,
        cache_capable: built.cache_capable,
    })
}

/// The interactive console, with its conversation kept per [`ChatArgs`].
async fn run_chat(options: &AgentOptions, args: ChatArgs) -> Result<()> {
    let agent = build_agent(options).await?;
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

async fn build_agent(options: &AgentOptions) -> Result<adk_agent::LlmAgent> {
    let model = resolve_model(options).await?;

    let mut builder = LlmAgentBuilder::new("adk_agent")
        .description("Default ADK-Rust CLI agent")
        .instruction(model.instruction)
        .model(model.llm);

    // Google Search grounding only works with Gemini
    if model.provider == Some(Provider::Model(ModelProvider::Gemini)) {
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

/// Builds the client of `provider` for `model`. The harness sets each request's effort
/// (spec 3.1 §7.23), which the client maps to its own control.
async fn create_model(
    provider: Provider,
    model: &str,
    api_key: Option<&str>,
    thinking_budget: Option<u32>,
) -> Result<BuiltModel> {
    let plain = |llm: Arc<dyn Llm>| BuiltModel { llm, cache_capable: None };
    match provider {
        #[cfg(feature = "gemini")]
        Provider::Model(ModelProvider::Gemini) => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Gemini requires an API key"))?;
            let m = Arc::new(adk_model::GeminiModel::new(key, model)?);
            Ok(BuiltModel { cache_capable: Some(m.clone()), ..plain(m) })
        }
        #[cfg(not(feature = "gemini"))]
        Provider::Model(ModelProvider::Gemini) => Err(setup::not_compiled(provider)),
        #[cfg(feature = "openai")]
        Provider::Model(ModelProvider::Openai) => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("OpenAI requires an API key"))?;
            // The Responses API serves OpenAI's provider search and the Responses-only
            // features (spec 3.1 §7.20); older models stay on Chat Completions.
            if openai_responses_model(model) {
                let config = adk_model::openai::OpenAIResponsesConfig::new(key, model);
                // Chat Completions does not store responses by default; keep that.
                let store_off: adk_model::openai::RequestAdapter = Arc::new(|body, _headers| {
                    if let Some(body) = body.as_object_mut() {
                        body.entry("store").or_insert(serde_json::Value::Bool(false));
                    }
                    Ok(())
                });
                let client = adk_model::openai::OpenAIResponsesClient::new(config)?
                    .with_request_adapter(store_off);
                return Ok(plain(Arc::new(client)));
            }
            let config = adk_model::OpenAIConfig::new(key, model);
            Ok(plain(Arc::new(adk_model::OpenAIClient::new(config)?)))
        }
        #[cfg(not(feature = "openai"))]
        Provider::Model(ModelProvider::Openai) => Err(setup::not_compiled(provider)),
        #[cfg(feature = "anthropic")]
        Provider::Model(ModelProvider::Anthropic) => {
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Anthropic requires an API key"))?;
            let mut config = adk_model::anthropic::AnthropicConfig::new(key, model);
            match thinking_budget {
                Some(0) => {
                    return Err(anyhow::anyhow!("--thinking-budget must be greater than 0"));
                }
                Some(budget) => config = config.with_thinking(budget),
                None => {}
            }
            Ok(plain(Arc::new(adk_model::AnthropicClient::new(config)?)))
        }
        #[cfg(not(feature = "anthropic"))]
        Provider::Model(ModelProvider::Anthropic) => Err(setup::not_compiled(provider)),
        #[cfg(feature = "deepseek")]
        Provider::Model(ModelProvider::Deepseek) => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("DeepSeek requires an API key"))?;
            let config = adk_model::DeepSeekConfig::new(key, model);
            Ok(plain(Arc::new(adk_model::DeepSeekClient::new(config)?)))
        }
        #[cfg(not(feature = "deepseek"))]
        Provider::Model(ModelProvider::Deepseek) => Err(setup::not_compiled(provider)),
        #[cfg(feature = "groq")]
        Provider::Model(ModelProvider::Groq) => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Groq requires an API key"))?;
            let config = adk_model::GroqConfig::new(key, model);
            Ok(plain(Arc::new(adk_model::GroqClient::new(config)?)))
        }
        #[cfg(not(feature = "groq"))]
        Provider::Model(ModelProvider::Groq) => Err(setup::not_compiled(provider)),
        #[cfg(feature = "ollama")]
        Provider::Model(ModelProvider::Ollama) => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let config = adk_model::OllamaConfig::new(model);
            Ok(plain(Arc::new(adk_model::OllamaModel::new(config)?)))
        }
        #[cfg(not(feature = "ollama"))]
        Provider::Model(ModelProvider::Ollama) => Err(setup::not_compiled(provider)),
        // mistral.rs in process (spec 3.1 §8.1): a Hugging Face id, or a `.gguf` path.
        #[cfg(feature = "mistralrs")]
        Provider::MistralRs => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let m = if model.ends_with(".gguf") {
                adk_mistralrs::MistralRsModel::from_gguf(model).await?
            } else {
                adk_mistralrs::MistralRsModel::from_hf(model).await?
            };
            Ok(plain(Arc::new(m)))
        }
        #[cfg(not(feature = "mistralrs"))]
        Provider::MistralRs => Err(setup::not_compiled(provider)),
    }
}

/// Whether the CLI runs `model` on the OpenAI Responses API: GPT-5.6 and every later GPT
/// generation, the models whose tool calls `OpenAIClient` already sends there because Chat
/// Completions rejects them with reasoning on.
#[cfg(feature = "openai")]
fn openai_responses_model(model: &str) -> bool {
    let model = model.strip_prefix("ft:").unwrap_or(model);
    let Some(version) = model.strip_prefix("gpt-") else {
        return false;
    };
    let end = version.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(version.len());
    let mut numbers = version[..end].split('.').map(str::parse::<u32>);
    match (numbers.next(), numbers.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= (5, 6),
        (Some(Ok(major)), None) => major >= 6,
        _ => false,
    }
}

fn reject_unsupported_thinking_budget(
    provider: Provider,
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

#[cfg(all(test, feature = "openai"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn openai_models_on_the_responses_api_serve_the_provider_search() {
        let openai = Provider::Model(ModelProvider::Openai);
        for model in ["gpt-5.6-terra", "gpt-5.6-luna", "gpt-6-astra", "ft:gpt-5.6-terra:acme::x1"] {
            let built = create_model(openai, model, Some("sk-test"), None).await.unwrap();
            let search = built.llm.native_web_search().expect(model);
            assert_eq!(
                (search.provider, search.tool),
                ("openai", adk_core::NativeWebSearchTool::OpenAiWebSearch),
                "{model}"
            );
        }
        // Older models stay on Chat Completions, which serves no provider search.
        for model in ["gpt-5.5", "gpt-5", "gpt-4.1", "o4-mini"] {
            let built = create_model(openai, model, Some("sk-test"), None).await.unwrap();
            assert!(built.llm.native_web_search().is_none(), "{model}");
        }
    }
}
