//! `adk-rust` on the harness: the REPL, `code`, `goal`, `ultracode` and `serve`
//! (spec 3.1 §8.1, §8.2).

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use adk_cli::console::ConsoleIo;
use adk_cli::harness::{
    BearerTokenExtractor, ConsolePrompter, HarnessArgs, HarnessLauncher, OPERATOR_SCOPE,
    ResolvedHarnessArgs, SearchArg, TurnOutcome,
};
use adk_core::{ApprovalPolicy, ApprovalService, ApproverSelector, Effort, StoreApprovalService};
use adk_devtools::Workspace;
use adk_harness::{
    ApprovalPrompter, FileWorkspaceTrustStore, GOAL_STATE_KEY, GoalSpec, GoalState, GoalStatus,
    HarnessAgent, HarnessMode, ShellIsolation,
};
use adk_session::{InMemorySessionService, SessionService};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::json;

use crate::ultra::{self, UltraAgents};
use crate::{BuiltModel, ResolvedModel};

/// Exit code of a run that left calls held for a decision (spec 3.1 §8.1).
pub const EXIT_HELD: i32 = 3;

/// The user's harness config file.
fn user_config() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|dir| dir.join("adk-rust").join("harness.toml"))
}

/// Everything a harness command needs to build its agent and launcher.
struct Setup {
    /// The session's client.
    models: Models,
    model_id: String,
    instruction: Option<String>,
    args: HarnessArgs,
    resolved: ResolvedHarnessArgs,
    user: String,
    #[cfg(feature = "persist")]
    db: Option<Arc<crate::sessions::HarnessDb>>,
    /// The embedder long-term memory recalls with, and its description.
    embedder: Option<(Arc<adk_harness::RagEmbedder>, String)>,
    /// How `harness.db` is sealed, for the banner (spec 3.1 §7.12).
    #[cfg(feature = "persist")]
    store_note: Option<String>,
}

/// The client of one session (spec 3.1 §7.23): the harness, its members and the
/// distiller share it, and the harness sets each request's effort from the policy.
#[derive(Clone)]
struct Models {
    harness: BuiltModel,
}

/// Installs the process's tracing subscriber once, so a `warn!` or `error!` from the
/// harness — a failed session-end distillation, a stale rule, a refused sandbox — reaches
/// stderr. `RUST_LOG` sets the filter; the default is `warn`.
fn install_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    // A second harness command in one process keeps the first subscriber.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .try_init();
}

impl Setup {
    async fn new(
        model: ResolvedModel,
        instruction: Option<String>,
        args: HarnessArgs,
        default_mode: HarnessMode,
    ) -> Result<Self> {
        install_tracing();
        let resolved = args.resolve(default_mode, user_config().as_deref())?;
        // The payments pack takes a merchant backend only a program can supply
        // (spec 3.1 §7.16): no flag or config file carries one.
        if resolved.packs.contains(&adk_harness::Pack::Payments) {
            bail!(
                "--pack payments: the payments pack needs a merchant checkout service, a \
                 transaction store and a mandate, which no flag or config file can supply. \
                 Build a binary that calls HarnessAgentBuilder::pack_payments(PaymentsPack) — \
                 `cargo adk new <name> --template harness` scaffolds one — and see \
                 docs/official_docs/harness/payments.md (spec 3.1 §7.16)"
            );
        }
        #[cfg(feature = "persist")]
        let (db, store_note) = if args.ephemeral {
            (None, None)
        } else {
            #[cfg(feature = "encrypted")]
            let (keyring, store_note) =
                crate::store::resolve_keyring(resolved.store_key.as_deref()).await?;
            #[cfg(feature = "encrypted")]
            let db = crate::sessions::open_harness_db(keyring.as_ref()).await?;
            #[cfg(not(feature = "encrypted"))]
            let (db, store_note) = (crate::sessions::open_harness_db().await?, None::<String>);
            (Some(Arc::new(db)), store_note)
        };
        #[cfg(not(feature = "persist"))]
        if args.session.is_some() || args.continue_session {
            bail!(
                "persistent sessions are not compiled into this adk-cli build. Reinstall with \
                 `--features persist`, or start without --session and --continue."
            );
        }
        let models = Models { harness: model.built() };
        let embedder = if resolved.memory { memory_embedder(&model) } else { None };
        Ok(Self {
            embedder,
            model_id: model.id.clone(),
            models,
            instruction,
            args,
            resolved,
            user: crate::setup::os_user(),
            #[cfg(feature = "persist")]
            db,
            #[cfg(feature = "persist")]
            store_note,
        })
    }

    fn sessions(&self) -> Arc<dyn SessionService> {
        #[cfg(feature = "persist")]
        if let Some(db) = &self.db {
            return db.sessions.clone();
        }
        Arc::new(InMemorySessionService::new())
    }

    /// The harness agent named `name` in `mode`, with the OS sandbox when `sandbox`.
    ///
    /// A served harness (`approval_service` set) applies standing rules and Auto only to
    /// authenticated callers (spec 3.1 §9.2 I6).
    fn agent(
        &self,
        name: &str,
        mode: HarnessMode,
        sandbox: bool,
        prompter: Option<Arc<dyn ApprovalPrompter>>,
        approval_service: Option<Arc<dyn ApprovalService>>,
    ) -> Result<HarnessAgent> {
        self.agent_with(self.resolved.effort, name, mode, sandbox, prompter, approval_service)
    }

    /// [`agent`](Self::agent) at the effort `policy`, which `/effort` replaces.
    fn agent_with(
        &self,
        effort: adk_harness::EffortPolicy,
        name: &str,
        mode: HarnessMode,
        sandbox: bool,
        prompter: Option<Arc<dyn ApprovalPrompter>>,
        approval_service: Option<Arc<dyn ApprovalService>>,
    ) -> Result<HarnessAgent> {
        let workspace = Workspace::new(&self.resolved.workspace).allow_bash(true);
        let mut builder = HarnessAgent::builder(name)
            .model(self.models.harness.llm.clone())
            .effort(effort)
            .workspace(workspace)
            .mode(mode)
            .confirmation_timeout(adk_cli::launcher::CONSOLE_CONFIRMATION_TIMEOUT);
        // Hooks (spec 3.1 §7.22, §8.1): the user config's run before the workspace's, which
        // the harness reads at build; --no-hooks runs none.
        if !self.resolved.hooks {
            builder = builder.without_hooks();
        } else if !self.resolved.user_hooks.is_empty() {
            builder = builder.hooks(self.resolved.user_hooks.clone());
        }
        // Explicit context caching for a model that supports it (spec 3.1 §7.23).
        if let Some(cache) = &self.models.harness.cache_capable {
            builder = builder.cache_capable(cache.clone());
        }
        if let Ok(trust) = FileWorkspaceTrustStore::in_home() {
            builder = builder.workspace_trust(Arc::new(trust));
        }
        if let Some(instruction) = &self.instruction {
            builder = builder.instruction(instruction.clone());
        }
        #[cfg(feature = "persist")]
        if let Some(db) = &self.db {
            builder = builder
                .mode_store(db.modes.clone())
                .standing_rules(db.rules.clone())
                .shell_ledger(db.ledger.clone())
                .approval_store(db.approvals.clone())
                .goal_store(db.goals.clone());
            // Long-term memory in harness.db; off when served (spec 3.1 §7.15.2).
            if self.resolved.memory && approval_service.is_none() {
                builder = builder
                    .long_term_memory(adk_harness::LongTermMemoryConfig::default())
                    .fact_store(db.facts.clone());
                if let Some((embedder, _)) = &self.embedder {
                    builder = builder.memory_embedder(embedder.clone());
                }
            }
        }
        if sandbox {
            builder = builder.shell(ShellIsolation::OsSandbox(Default::default()));
            if self.resolved.fail_if_unavailable {
                builder = builder.fail_if_unavailable();
            }
        }
        if let Some(prompter) = prompter {
            builder = builder.prompter(prompter);
        }
        if let Some(service) = approval_service {
            builder = builder.approval_service(service).require_authenticated_identity();
        }
        for pack in &self.resolved.packs {
            builder = builder.pack(*pack);
        }
        // `--pack browser` (spec 3.1 §7.24): the WebDriver endpoint from ADK_WEBDRIVER_URL, else
        // adk-browser's default `http://localhost:4444`; `--allow-domain` is the allowlist.
        #[cfg(feature = "browser")]
        if self.resolved.packs.contains(&adk_harness::Pack::Browser) {
            let mut config = adk_browser::BrowserConfig::default();
            if let Ok(url) = std::env::var("ADK_WEBDRIVER_URL")
                && !url.trim().is_empty()
            {
                config = config.webdriver_url(url.trim());
            }
            let session = Arc::new(adk_browser::BrowserSession::new(config));
            builder = builder.pack_browser(adk_harness::BrowserPack::new(session));
        }
        for member in &self.resolved.members {
            builder = builder.member(member.to_spec()?);
        }
        if self.resolved.web {
            builder = builder.web(adk_harness::WebConfig {
                allowed_domains: self.resolved.allow_domains.clone(),
                ..adk_harness::WebConfig::default()
            });
            // Web search (spec 3.1 §7.20): the model provider's built-in search unless the
            // user chose a search provider, which replaces it with the governed web_search.
            builder = match self.resolved.search {
                SearchArg::Provider => builder,
                SearchArg::Off => builder.provider_web_tools(adk_harness::ProviderWebTools::off()),
                SearchArg::Searxng => {
                    let url = self
                        .resolved
                        .searxng_url
                        .clone()
                        .or_else(|| std::env::var("SEARXNG_URL").ok())
                        .filter(|url| !url.trim().is_empty())
                        .context(
                            "--search searxng needs the instance: set `searxng_url` under [web] \
                             in your user config, or SEARXNG_URL",
                        )?;
                    builder.search_provider(Arc::new(adk_harness::SearxngSearch::new(url)?))
                }
                SearchArg::DeepSeek => {
                    builder.search_provider(Arc::new(adk_harness::DeepSeekSearch::from_env()?))
                }
            };
        }
        // Servers the workspace declares in `.mcp.json` start once the workspace is trusted
        // (spec 3.1 §7.21, I3).
        if let Some(servers) = workspace_mcp_servers(&self.resolved.workspace)? {
            builder = builder
                .mcp_servers(adk_harness::McpServerSet::workspace(servers))
                .mcp_interaction(adk_harness::McpInteractionConfig::default());
        }
        Ok(builder.build()?)
    }

    /// The session `--session`, `--continue` or the goal lookup names, if any.
    async fn session_id(&self) -> Result<Option<String>> {
        if let Some(session) = &self.args.session {
            return Ok(Some(session.clone()));
        }
        if self.args.continue_session {
            let latest = latest_in_workspace(
                self.sessions().as_ref(),
                &self.user,
                &self.resolved.workspace,
                |_| true,
            )
            .await?;
            if latest.is_none() {
                println!("No earlier session in this workspace; starting a new one.");
            }
            return Ok(latest);
        }
        Ok(None)
    }

    /// A launcher for `agent` over this setup's sessions, user and session choice.
    async fn launcher(&self, agent: HarnessAgent) -> Result<HarnessLauncher> {
        let hooks = match (self.resolved.hooks, agent.runtime().hooks()) {
            (false, _) => Some("off (--no-hooks)".to_string()),
            (true, Some(set)) => Some(format!("{} (/hooks lists them)", set.hooks.len())),
            (true, None) => None,
        };
        let search = agent.info().web_search.to_string();
        let mut launcher = HarnessLauncher::new(agent)
            .app_name(APP_NAME)
            .with_session_service(self.sessions())
            .with_user_id(self.user.clone())
            .trust_workspace(self.resolved.trust)
            .hooks_disabled(!self.resolved.hooks);
        if let Some(hooks) = hooks {
            launcher = launcher.with_status_line("Hooks", hooks);
        }
        launcher = launcher.with_status_line("Search", search);
        if self.resolved.mode_explicit {
            launcher = launcher.with_mode(self.resolved.mode);
        }
        if let Some(session) = self.session_id().await? {
            launcher = launcher.with_session_id(session);
        }
        launcher = launcher.with_status_line(
            "Effort",
            format!(
                "{} now; per mode: {}",
                self.resolved.effort.for_mode(self.resolved.mode),
                self.resolved.effort.describe()
            ),
        );
        #[cfg(feature = "persist")]
        if let Some(db) = &self.db {
            let store = match (&self.store_note, &db.key_id) {
                (Some(note), _) => note.clone(),
                (None, Some(id)) => format!("encrypted under key {id}"),
                (None, None) => {
                    "harness.db in plaintext (--store-key keychain:adk-rust seals it)".into()
                }
            };
            launcher = launcher.with_status_line("Store", store);
        }
        for note in &self.resolved.notes {
            launcher = launcher.with_banner_note(note.clone());
        }
        Ok(launcher)
    }

    /// The prompter of a one-shot command: `--yes`, the terminal, or none, which holds
    /// calls for a decision.
    fn unattended_prompter(&self, console: &ConsoleIo) -> Option<Arc<dyn ApprovalPrompter>> {
        if self.resolved.yes {
            let prompter =
                ConsolePrompter::new(console.clone()).approve_all(self.resolved.allow_unsandboxed);
            Some(Arc::new(prompter))
        } else if console.is_terminal() {
            Some(Arc::new(ConsolePrompter::new(console.clone())))
        } else {
            None
        }
    }

    fn print_notes(&self) {
        for note in &self.resolved.notes {
            println!("note: {note}");
        }
    }

    /// Says once, on the first run with long-term memory, what is stored and where
    /// (spec 3.1 §7.15.2).
    fn memory_notice(&self) {
        if !self.memory_on() {
            return;
        }
        let Some(dir) = dirs::config_dir().map(|dir| dir.join("adk-rust")) else {
            return;
        };
        let marker = dir.join("memory-notice");
        if marker.exists() {
            return;
        }
        let recall = match &self.embedder {
            Some((_, name)) => format!("Facts are embedded with {name} so related ones are found."),
            None => "Facts are found by shared words; nothing leaves this machine for it.".into(),
        };
        println!(
            "Long-term memory is on. The harness keeps short facts from your sessions: the \
             preferences you state, how a project builds and tests, what fixed a failure. They \
             live in {} on this machine, with personal data redacted and secrets refused, and \
             are recalled in later sessions. What you said is \"known\"; what the agent \
             inferred is \"assumed\" until you confirm it. {recall}\n/memory lists them, \
             /memory forget ID removes one, `adk-rust memory export` writes them out, and \
             --no-memory turns memory off. This notice is shown once.\n",
            dir.join("harness.db").display()
        );
        let _ = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&marker, "shown\n"));
    }

    fn memory_on(&self) -> bool {
        #[cfg(feature = "persist")]
        return self.resolved.memory && self.db.is_some();
        #[cfg(not(feature = "persist"))]
        false
    }
}

/// The embedder long-term memory recalls with (spec 3.1 §7.15.2): the local ONNX model for
/// a local provider (nothing leaves the machine, H10), else the model's provider when it
/// embeds (Gemini, OpenAI), else Gemini or OpenAI when its key is set; with none, recall
/// matches shared words.
fn memory_embedder(model: &ResolvedModel) -> Option<(Arc<adk_harness::RagEmbedder>, String)> {
    use crate::cli::Provider;
    use adk_model::ModelProvider;
    const GEMINI: &str = "Gemini (gemini-embedding-2)";
    const OPENAI: &str = "OpenAI (text-embedding-3-small)";
    const LOCAL: &str = "a local ONNX model (BAAI/bge-small-en-v1.5); nothing leaves this machine";
    if model.provider().is_some_and(Provider::is_local) {
        #[cfg(feature = "memory-local")]
        match adk_harness::RagEmbedder::local() {
            Ok(embedder) => return Some((Arc::new(embedder), LOCAL.into())),
            Err(error) => println!(
                "note: the local embedding model is unavailable ({error}); facts are found by \
                 shared words"
            ),
        }
        let _ = LOCAL;
        return None;
    }
    let env = |names: &[&str]| {
        names.iter().find_map(|name| std::env::var(name).ok().filter(|key| !key.trim().is_empty()))
    };
    let own = match (model.provider(), model.api_key.clone()) {
        (Some(Provider::Model(ModelProvider::Gemini)), Some(key)) => Some((key, true)),
        (Some(Provider::Model(ModelProvider::Openai)), Some(key)) => Some((key, false)),
        _ => None,
    };
    let (key, gemini) = own
        .or_else(|| env(&["GOOGLE_API_KEY", "GEMINI_API_KEY"]).map(|key| (key, true)))
        .or_else(|| env(&["OPENAI_API_KEY"]).map(|key| (key, false)))?;
    let built = if gemini {
        adk_harness::RagEmbedder::gemini(&key).map(|embedder| (embedder, GEMINI))
    } else {
        adk_harness::RagEmbedder::openai(&key).map(|embedder| (embedder, OPENAI))
    };
    built.ok().map(|(embedder, name)| (Arc::new(embedder), name.to_string()))
}

const APP_NAME: &str = "adk-rust";

/// The most recently updated session of `user` in `workspace` whose state passes `keep`.
async fn latest_in_workspace(
    sessions: &dyn SessionService,
    user: &str,
    workspace: &Path,
    keep: impl Fn(&dyn adk_session::State) -> bool,
) -> Result<Option<String>> {
    let mut listed = sessions
        .list(adk_session::ListRequest {
            app_name: APP_NAME.to_string(),
            user_id: user.to_string(),
            limit: None,
            offset: None,
        })
        .await?;
    listed.sort_by_key(|session| std::cmp::Reverse(session.last_update_time()));
    let recorded = workspace.display().to_string();
    for session in listed {
        let full = sessions
            .get(adk_session::GetRequest {
                app_name: APP_NAME.to_string(),
                user_id: user.to_string(),
                session_id: session.id().to_string(),
                num_recent_events: Some(1),
                after: None,
            })
            .await?;
        let state = full.state();
        if state.get("harness:workspace").and_then(|value| value.as_str().map(str::to_string))
            == Some(recorded.clone())
            && keep(state)
        {
            return Ok(Some(full.id().to_string()));
        }
    }
    Ok(None)
}

/// Prints what to do about held calls and returns [`EXIT_HELD`].
fn held_exit(outcome: &TurnOutcome, persistent: bool) -> i32 {
    println!(
        "\n{} call(s) are held for a decision in session {}.",
        outcome.held.len(),
        outcome.session_id
    );
    for request in &outcome.held {
        println!("  {} {}", request.tool_name, first_line(&request.args.to_string()));
    }
    if persistent {
        println!(
            "Decide with `adk-rust approvals list` and `adk-rust approvals approve ID`, then \
             continue with `adk-rust --session {}`. Or rerun with --yes, or in a terminal.",
            outcome.session_id
        );
    } else {
        println!("Rerun in a terminal to decide them, or with --yes to approve them.");
    }
    EXIT_HELD
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let cut: String = line.chars().take(160).collect();
    if cut.len() < line.len() { format!("{cut}…") } else { cut }
}

/// `adk-rust` with no subcommand: the harness REPL (spec 3.1 §8.1).
pub async fn repl(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
) -> Result<()> {
    let setup = Arc::new(Setup::new(model, instruction, args, HarnessMode::Ask).await?);
    setup.memory_notice();
    let console = ConsoleIo::terminal()?;
    let prompter = |console: &ConsoleIo| -> Arc<dyn ApprovalPrompter> {
        let prompter = ConsolePrompter::new(console.clone());
        Arc::new(if setup.resolved.yes {
            prompter.approve_all(setup.resolved.allow_unsandboxed)
        } else {
            prompter
        })
    };
    let agent = setup.agent(
        APP_NAME,
        setup.resolved.mode,
        setup.resolved.sandbox,
        Some(prompter(&console)),
        None,
    )?;
    let rebuild = {
        let setup = setup.clone();
        let console = console.clone();
        Arc::new(move |sandbox: bool| {
            let prompter = ConsolePrompter::new(console.clone());
            let prompter: Arc<dyn ApprovalPrompter> = Arc::new(if setup.resolved.yes {
                prompter.approve_all(setup.resolved.allow_unsandboxed)
            } else {
                prompter
            });
            setup
                .agent(APP_NAME, setup.resolved.mode, sandbox, Some(prompter), None)
                .map_err(|error| adk_core::AdkError::config(error.to_string()))
        })
    };
    // `/effort` sets every mode and role to the level from the next model call (spec 3.1
    // §7.23); the session keeps its client.
    let effort_rebuild: adk_cli::harness::HarnessEffortRebuild = {
        let setup = setup.clone();
        let console = console.clone();
        Arc::new(move |effort: Effort| {
            let setup = setup.clone();
            let console = console.clone();
            Box::pin(async move {
                let prompter = ConsolePrompter::new(console.clone());
                let prompter: Arc<dyn ApprovalPrompter> = Arc::new(if setup.resolved.yes {
                    prompter.approve_all(setup.resolved.allow_unsandboxed)
                } else {
                    prompter
                });
                setup
                    .agent_with(
                        adk_harness::EffortPolicy::uniform(effort),
                        APP_NAME,
                        setup.resolved.mode,
                        setup.resolved.sandbox,
                        Some(prompter),
                        None,
                    )
                    .map_err(|error| adk_core::AdkError::config(error.to_string()))
            })
        })
    };
    setup
        .launcher(agent)
        .await?
        .with_console(console)
        .with_rebuild(rebuild)
        .with_effort_rebuild(effort_rebuild)
        .run_console_directly()
        .await
        .map_err(Into::into)
}

/// `adk-rust code`: one harness turn; [`EXIT_HELD`] when calls are held (spec 3.1 §8.1).
pub async fn code(
    model: ResolvedModel,
    instruction: Option<String>,
    mut args: HarnessArgs,
    read_only: bool,
    task: String,
) -> Result<i32> {
    if read_only {
        args.mode = Some(HarnessMode::ReadOnly);
    }
    let setup = Setup::new(model, instruction, args, HarnessMode::Ask).await?;
    setup.print_notes();
    setup.memory_notice();
    let console = ConsoleIo::terminal()?;
    let prompter = setup.unattended_prompter(&console);
    let agent =
        setup.agent(APP_NAME, setup.resolved.mode, setup.resolved.sandbox, prompter, None)?;
    let info = agent.info().clone();
    let mut launcher = setup.launcher(agent).await?.with_console(console);
    println!(
        "harness ({}) on {} · mode {} · shell {}\ntask: {task}\n",
        setup.model_id,
        setup.resolved.workspace.display(),
        setup.resolved.mode,
        info.isolation
    );
    let outcome = launcher.run_once(&task).await?;
    println!();
    // The turn is distilled into long-term memory before the process ends (spec 3.1 §7.15.2).
    launcher.close().await;
    let todos = launcher.todos().await?;
    if !todos.items.is_empty() {
        println!("todos:");
        for item in &todos.items {
            let mark = match item.status {
                adk_harness::TodoStatus::Completed => "✓",
                adk_harness::TodoStatus::InProgress => "▶",
                adk_harness::TodoStatus::Cancelled => "✗",
                adk_harness::TodoStatus::Pending => "·",
            };
            println!("  {mark} {}", adk_harness::escape_control(&item.content));
        }
        println!();
    }
    if !outcome.held.is_empty() {
        return Ok(held_exit(&outcome, persistent(&setup)));
    }
    if outcome.ended_by_denial {
        println!("The turn ended after a denial (session {}).", outcome.session_id);
    }
    println!("session: {}", outcome.session_id);
    Ok(if outcome.interrupted { 130 } else { 0 })
}

fn persistent(setup: &Setup) -> bool {
    #[cfg(feature = "persist")]
    return setup.db.is_some();
    #[cfg(not(feature = "persist"))]
    {
        let _ = setup;
        false
    }
}

/// Arguments of `adk-rust goal`.
pub struct GoalArgs {
    pub goal: String,
    pub until: String,
    pub max_iters: u32,
    /// How long one run of the check may take (`--check-timeout`).
    pub check_timeout: std::time::Duration,
    pub state: Option<String>,
    pub resume: bool,
}

/// `adk-rust goal`: the harness goal engine, iterating until `until` passes
/// (spec 3.1 §8.1, §7.11).
///
/// `Harness::start_goal` starts the goal; with `harness.db` it is a durable run that parks
/// on a held call and continues with `--resume` once the call is decided, in the same
/// session and iteration.
pub async fn goal(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
    goal: GoalArgs,
) -> Result<i32> {
    let setup = Setup::new(model, instruction, args, HarnessMode::Ask).await?;
    setup.print_notes();
    setup.memory_notice();
    if !matches!(setup.resolved.mode, HarnessMode::Ask | HarnessMode::Auto) {
        bail!(
            "a goal needs ask or auto mode, not {}: it edits files and runs commands",
            setup.resolved.mode
        );
    }
    let console = ConsoleIo::terminal()?;
    let prompter = setup.unattended_prompter(&console);
    let agent =
        setup.agent(APP_NAME, setup.resolved.mode, setup.resolved.sandbox, prompter, None)?;
    let sessions = setup.sessions();
    let workspace = setup.resolved.workspace.clone();

    let resumed = if goal.resume && setup.args.session.is_none() {
        latest_in_workspace(sessions.as_ref(), &setup.user, &workspace, |state| {
            state
                .get(GOAL_STATE_KEY)
                .and_then(|value| serde_json::from_value::<GoalState>(value).ok())
                .is_some_and(|state| state.status != GoalStatus::Done)
        })
        .await?
    } else {
        None
    };
    let mut launcher = setup.launcher(agent).await?.with_console(console);
    #[cfg(feature = "persist")]
    if let Some(db) = &setup.db {
        launcher = launcher.with_run_store(db.runs.clone());
    }
    if let Some(session) = &resumed {
        launcher = launcher.with_session_id(session.clone());
    }

    println!("goal mode ({}) on {}", setup.model_id, workspace.display());
    let outcome = if goal.resume && (resumed.is_some() || setup.args.session.is_some()) {
        let session = resumed.as_deref().or(setup.args.session.as_deref()).unwrap_or_default();
        println!("resuming goal in session {session}");
        launcher.resume_goal(Some(goal.max_iters)).await?
    } else {
        if goal.resume {
            println!("no unfinished goal in this workspace; starting fresh.");
        }
        println!("goal:  {}", goal.goal);
        println!("until: {}  (budget: {} iterations)\n", goal.until, goal.max_iters);
        let spec = GoalSpec {
            goal: goal.goal.clone(),
            until: goal.until.clone(),
            max_iterations: goal.max_iters,
            check_timeout: goal.check_timeout,
        };
        launcher.run_goal(spec).await?
    };
    let state = launcher.goal().await?;
    if let (Some(path), Some(state)) = (goal.state.as_deref(), &state) {
        write_goal_mirror(Path::new(path), state)?;
    }
    let code = if !outcome.held.is_empty() {
        goal_held_exit(&outcome, persistent(&setup))
    } else if outcome.interrupted {
        println!("\ninterrupted; continue with --resume.");
        130
    } else {
        goal_exit(state.as_ref(), &outcome)
    };
    launcher.close().await;
    Ok(code)
}

/// Reports how a goal stands and returns the exit code: `0` met, `1` otherwise.
fn goal_exit(state: Option<&GoalState>, outcome: &TurnOutcome) -> i32 {
    match state {
        Some(state) if state.status == GoalStatus::Done => {
            println!(
                "\ngoal met after {} iteration(s): `{}` passed.",
                state.iteration, state.until
            );
            0
        }
        Some(state) if state.status == GoalStatus::Exhausted => {
            println!(
                "\nbudget exhausted after {} iteration(s); `{}` still failing. Resume later \
                 with --resume and a higher --max-iters.",
                state.iteration, state.until
            );
            1
        }
        Some(state) => {
            if outcome.ended_by_denial {
                println!("\nThe turn ended after a denial.");
            }
            println!(
                "\nthe goal is {:?} at iteration {}; continue with --resume.",
                state.status, state.iteration
            );
            1
        }
        None => 1,
    }
}

/// Prints what to do about calls a goal holds and returns [`EXIT_HELD`].
fn goal_held_exit(outcome: &TurnOutcome, persistent: bool) -> i32 {
    if !persistent {
        return held_exit(outcome, persistent);
    }
    println!(
        "\n{} call(s) are held for a decision in session {}.",
        outcome.held.len(),
        outcome.session_id
    );
    for request in &outcome.held {
        println!("  {} {}", request.tool_name, first_line(&request.args.to_string()));
    }
    println!(
        "The goal waits for them. Decide with `adk-rust approvals list` and `adk-rust \
         approvals approve ID`, then continue with `adk-rust goal --resume`."
    );
    EXIT_HELD
}

/// Writes the `--state` JSON mirror of the goal.
fn write_goal_mirror(path: &Path, state: &GoalState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(&json!({
            "goal": state.goal,
            "until": state.until,
            "iteration": state.iteration,
            "status": serde_json::to_value(state.status)?,
            "lastCheck": state.last_check,
        }))?,
    )
    .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `adk-rust ultracode` on the harness (spec 3.1 §8.1).
pub async fn ultracode(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
    task: String,
    max_rounds: i64,
) -> Result<i32> {
    let setup = Arc::new(Setup::new(model, instruction, args, HarnessMode::Ask).await?);
    setup.print_notes();
    let console = ConsoleIo::terminal()?;
    let prompter = setup.unattended_prompter(&console);
    let agent =
        setup.agent(APP_NAME, setup.resolved.mode, setup.resolved.sandbox, prompter, None)?;
    let launcher = setup.launcher(agent).await?.with_console(console);
    let agents = Arc::new(HarnessUltra {
        setup: setup.clone(),
        implementer: tokio::sync::Mutex::new(launcher),
        held: std::sync::Mutex::new(None),
    });
    ultra::run_with(
        agents.clone(),
        &setup.model_id,
        &setup.resolved.workspace.display().to_string(),
        task,
        max_rounds,
    )
    .await?;
    let held = agents.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    Ok(match held {
        Some(outcome) => held_exit(&outcome, persistent(&setup)),
        None => 0,
    })
}

/// Implements in the user's mode in one session; reviews in read-only mode.
struct HarnessUltra {
    setup: Arc<Setup>,
    implementer: tokio::sync::Mutex<HarnessLauncher>,
    held: std::sync::Mutex<Option<TurnOutcome>>,
}

#[async_trait]
impl UltraAgents for HarnessUltra {
    async fn implement(&self, prompt: &str) {
        let mut launcher = self.implementer.lock().await;
        match launcher.run_once(prompt).await {
            Ok(outcome) if !outcome.held.is_empty() => {
                println!("  {} call(s) held for a decision", outcome.held.len());
                *self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(outcome);
            }
            Ok(_) => {}
            Err(error) => println!("  implement failed: {error}"),
        }
    }

    async fn review(&self, focus: &str, desc: &str, prompt: &str) -> String {
        let reviewer = async {
            let instruction =
                ultra::review_instruction(focus, desc, self.setup.instruction.as_deref());
            let workspace = Workspace::new(&self.setup.resolved.workspace);
            let agent = HarnessAgent::builder(format!("review-{focus}"))
                .model(self.setup.models.harness.llm.clone())
                .effort(self.setup.resolved.effort)
                .workspace(workspace)
                .mode(HarnessMode::ReadOnly)
                .without_shell()
                .instruction(instruction)
                .build()?;
            let mut launcher = HarnessLauncher::new(agent)
                .app_name(APP_NAME)
                .with_user_id(self.setup.user.clone())
                .with_session_service(Arc::new(InMemorySessionService::new()))
                .with_console(ConsoleIo::scripted([], false))
                .quiet();
            launcher.run_once(prompt).await.map(|outcome| outcome.text)
        };
        reviewer.await.unwrap_or_else(|error: adk_core::AdkError| format!("review failed: {error}"))
    }
}

/// `adk-rust serve` on the harness (spec 3.1 §8.2).
pub async fn serve(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
    port: u16,
    host: IpAddr,
    auth_token: Option<String>,
) -> Result<()> {
    let setup = Setup::new(model, instruction, args, HarnessMode::Plan).await?;
    setup.print_notes();
    let loopback = host.is_loopback();
    let token = auth_token.filter(|token| !token.trim().is_empty());
    if !loopback && token.is_none() {
        bail!(
            "serving the harness on {host} needs authentication: pass --auth-token or set \
             ADK_SERVE_TOKEN, or listen on 127.0.0.1"
        );
    }
    if !loopback && setup.resolved.mode == HarnessMode::Auto && !setup.resolved.sandbox {
        bail!(
            "--mode auto beyond loopback needs --sandbox: edits would run unattended on this host"
        );
    }

    #[cfg(feature = "persist")]
    let (approval_store, run_store): (
        Arc<dyn adk_core::ApprovalStore>,
        Arc<dyn adk_core::DurableRunStore>,
    ) = match &setup.db {
        Some(db) => (db.approvals.clone(), db.runs.clone()),
        None => (
            Arc::new(adk_core::InMemoryApprovalStore::new()),
            Arc::new(adk_core::InMemoryRunStore::new()),
        ),
    };
    #[cfg(not(feature = "persist"))]
    let (approval_store, run_store): (
        Arc<dyn adk_core::ApprovalStore>,
        Arc<dyn adk_core::DurableRunStore>,
    ) = (
        Arc::new(adk_core::InMemoryApprovalStore::new()),
        Arc::new(adk_core::InMemoryRunStore::new()),
    );
    // The person running the server is the operator: their own run's calls are theirs to
    // approve, so self-approval is allowed.
    let routing =
        ApprovalPolicy::new([ApproverSelector::group(OPERATOR_SCOPE)]).allow_self_approval();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(approval_store)
            .with_router(routing)
            .with_run_store(run_store.clone()),
    );
    let agent = setup.agent(
        APP_NAME,
        setup.resolved.mode,
        setup.resolved.sandbox,
        None,
        Some(service.clone()),
    )?;

    // A turn can run as a durable background run of the `adk-rust` target (spec 3.1 §7.12).
    let runner = adk_runner::Runner::builder()
        .app_name(APP_NAME)
        .agent(agent.agent())
        .session_service(setup.sessions())
        .approval_store(agent.runtime().approval_store().clone())
        .build()?;
    // The runner's root is the harness agent, which runs a session's goal while one runs, so
    // it also serves the durable goal target (spec 3.1 §7.11).
    let runner = Arc::new(runner);
    let background = adk_server::background::BackgroundState::new()
        .with_run_store(run_store)
        .with_app_name(APP_NAME)
        .with_agent(APP_NAME, runner.clone())
        .with_agent(adk_harness::GOAL_TARGET, runner);

    let mut launcher = HarnessLauncher::new(agent)
        .app_name(APP_NAME)
        .with_session_service(setup.sessions())
        .with_user_id(setup.user.clone())
        .with_approval_service(service)
        .with_background_runs(background)
        .with_bind_address(host);
    if let Some(token) = token {
        launcher = launcher.with_request_context_extractor(Arc::new(BearerTokenExtractor::new(
            token,
            setup.user.clone(),
        )));
    }
    launcher.run_serve_directly(port).await.map_err(Into::into)
}

/// The servers `.mcp.json` in `workspace` declares (the Kiro `mcpServers` format
/// `McpServerManager::from_json` reads), or `None` without the file.
fn workspace_mcp_servers(
    workspace: &Path,
) -> Result<Option<std::collections::HashMap<String, adk_tool::mcp::McpServerConfig>>> {
    #[derive(serde::Deserialize)]
    struct McpFile {
        #[serde(rename = "mcpServers", default)]
        mcp_servers: std::collections::HashMap<String, adk_tool::mcp::McpServerConfig>,
    }
    let path = workspace.join(".mcp.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => bail!("failed to read {}: {error}", path.display()),
    };
    let file: McpFile = serde_json::from_str(&text).map_err(|error| {
        anyhow::anyhow!("{} is not valid MCP configuration: {error}", path.display())
    })?;
    Ok(Some(file.mcp_servers))
}
