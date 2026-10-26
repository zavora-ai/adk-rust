//! `adk-rust` on the harness: the REPL, `code`, `goal`, `ultracode` and `serve`
//! (spec 3.1 §8.1, §8.2).

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use adk_cli::console::ConsoleIo;
use adk_cli::harness::ultracode::{
    ArchitectureChoice, BoardStatus, ChangeSignals, Focus, IndependentReviewer, ROUTER_VERSION,
    RepoSignals, ReviewBoard, ReviewPlan, RouteDecision, RouteInputs, RouterMode, Rule,
    SNAPSHOT_MAX_FILES, SettingSource, TaskFeatures, TurnStatus, UltracodeAudit, UltracodeFlags,
    UltracodeLimits, Verdict, WorkspaceSnapshot, route,
};
use adk_cli::harness::{
    ConsolePrompter, HarnessArgs, HarnessLauncher, KernelSettings, OPERATOR_SCOPE,
    ResolvedHarnessArgs, SearchArg, TurnOutcome,
};
use adk_core::audit_log::AuditRecorder;
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, Effort, Llm, RunBudget, StoreApprovalService,
};
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

pub(crate) mod agents;
mod books;
mod embedder;
mod manifest;
mod service;
#[cfg(feature = "tui")]
pub mod surface;
mod up;
#[cfg(windows)]
pub use service::run_windows_service;
pub use service::{install_service, print_service, uninstall_service};
pub use up::up;

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
    /// Every store in one backend the replicas share, from `serve --store` (spec 3.1 §13 Q2).
    shared: Option<Arc<crate::shared_store::SharedStores>>,
    /// Where plans and goal graphs are checkpointed, and its name for the banner (spec 3.1
    /// §7.11, §13 Q5); `None` keeps them in memory.
    plans: Option<(Arc<dyn adk_graph::Checkpointer>, String)>,
    /// `--preset personal`: the first-run answers (spec 3.3 §10 H1, §13 Q7).
    personal: Option<PersonalSetup>,
}

/// `--preset personal`: the first-run answers and the limits in force (spec 3.3 §10 H1,
/// §13 Q7).
struct PersonalSetup {
    settings: adk_harness::personal::PersonalSettings,
    /// Quiet time as `adk-rust up` keeps it: the user config's `[kernel]` table wins.
    quiet: Option<adk_kernel::QuietHours>,
    /// The daily spending limit as `adk-rust up` keeps it: the user config's `[kernel]`
    /// table wins, and the workspace's `.adk/harness.toml` may only lower it.
    daily_spend_cap: Option<u64>,
}

impl PersonalSetup {
    /// Both limits, shown on every start (spec 3.3 §13 Q7). Only `adk-rust up` runs work on
    /// its own, so only it keeps them.
    fn status_lines(&self) -> [(&'static str, String); 2] {
        [
            ("Quiet time", format!("{} (kept by adk-rust up)", up::quiet_time_label(self.quiet))),
            (
                "Spending",
                format!(
                    "daily spending limit {} (kept by adk-rust up)",
                    up::spending_limit_label(self.daily_spend_cap)
                ),
            ),
        ]
    }
}

/// Where `bash` runs, or `none` when the harness offers no shell (spec 3.3 §10 H1).
fn shell_label(info: &adk_harness::HarnessInfo) -> String {
    info.shell().map_or_else(|| "none".to_string(), ToString::to_string)
}

/// `--preset personal` (spec 3.3 §10 H1, §13 Q7): the answers kept in `home`, asked for on
/// this terminal at the first run.
fn personal_settings(home: &Path) -> Result<adk_harness::personal::PersonalSettings> {
    let (settings, _) = adk_harness::personal::PersonalSettings::load_or_ask(
        home,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout(),
    )?;
    Ok(settings)
}

/// The client of one session (spec 3.1 §7.23): the harness, its members and the
/// distiller share it, and the harness sets each request's effort from the policy.
#[derive(Clone)]
struct Models {
    harness: BuiltModel,
}

/// What an `ultracode` implementer adds to a harness agent (spec 3.1 §7.10): its turn's
/// `RunBudget` and the run's audit log; and what `up` adds for a partition's harness (spec
/// 3.2 §7.8).
#[derive(Default)]
struct AgentExtras {
    budget: Option<RunBudget>,
    audit: Option<Arc<dyn AuditRecorder>>,
    /// Long-term memory with an approval service: `adk-rust up`, the user's own kernel,
    /// keeps it, where `serve` turns it off (spec 3.2 §8.1).
    memory_with_inbox: bool,
    /// The kernel's drivers, classed by their manifests and under the kernel's driver
    /// policy (spec 3.2 §7.7): those whose manifests list the default partition, or for a
    /// partition's harness, those its own driver list names (§7.8).
    drivers: Option<adk_kernel::drivers::DriverSupervisor>,
    /// The partition the harness runs, the table its policy reads, and whether it is the
    /// coordinator.
    partition: Option<(adk_kernel::Partition, adk_kernel::Partitions, bool)>,
    /// The control store whose area mode, changed while the kernel runs, governs the
    /// harness's sessions from their next step, the harness's area, and whether Go ahead is
    /// refused there (spec 3.3 §10 K3).
    area_modes: Option<(Arc<dyn adk_kernel::control::ControlStore>, adk_kernel::PartitionId, bool)>,
    /// The person at this console runs the harness and answers its inbox (the terminal
    /// surface in process, spec 3.3 §5.2): its runs are theirs, so standing rules and Auto
    /// apply without a server's authentication.
    #[cfg(feature = "tui")]
    local_console: bool,
    /// For the coordinator: the partitions whose drivers it reaches as crossings (spec 3.2
    /// §7.8, K-I15).
    driver_crossings: Vec<adk_kernel::PartitionId>,
    /// The kernel's agent table `propose_agent` suggests agents to: the primary agent only
    /// (spec 3.3 §5.7).
    agents: Option<adk_kernel::agents::AgentsHandle>,
    /// The kernel target the harness stamps its sessions with (spec 3.3 §5.7 "Threads").
    session_stamp: Option<String>,
    /// The agent a person added that this harness is built for (spec 3.3 §5.7, §10 L1).
    added: Option<agents::Added>,
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
        // `serve` opens without this subscriber: its launcher installs the telemetry one,
        // and a second global subscriber panics at start.
        install_tracing();
        Self::open(model, instruction, args, default_mode, None, None).await
    }

    /// [`new`](Self::new), with every store in the backend at `store` instead of
    /// `harness.db` when one is given (`serve --store`; spec 3.1 §8.2, §13 Q2), and
    /// `harness.db` in `home` when one is given (`up --home`; spec 3.2 §7.2).
    async fn open(
        model: ResolvedModel,
        instruction: Option<String>,
        mut args: HarnessArgs,
        default_mode: HarnessMode,
        store: Option<&str>,
        home: Option<&Path>,
    ) -> Result<Self> {
        // The personal preset reads `[kernel]` for its home and the limits it shows; only
        // `adk-rust up` runs a kernel, so anything else notes an error there and goes on
        // (spec 3.3 §10 H1).
        let personal_preset = args.preset == Some(adk_harness::Preset::Personal);
        let mut kernel_notes = Vec::new();
        let user_kernel = if personal_preset {
            KernelSettings::resolve_user(user_config().as_deref())
                .map_err(|error| {
                    kernel_notes.push(format!(
                        "{error}; the limits shown are the first-run answers, and `adk-rust up` \
                         will not start until it is fixed"
                    ));
                })
                .ok()
        } else {
            None
        };
        // The personal preset works in a folder under the home, not the current directory,
        // and asks its first-run questions before anything starts (spec 3.3 §10 H1, §13 Q7).
        let personal = if personal_preset {
            // The home `adk-rust up` uses, so both read one `personal.toml`.
            let home = match home {
                Some(home) => home.to_path_buf(),
                None => match user_kernel.as_ref().and_then(|user| user.home.clone()) {
                    Some(home) => home,
                    None => up::default_home()?,
                },
            };
            let workspace = adk_harness::personal::PersonalWorkspace::create(&home)?;
            if args.dir.is_none() {
                args.dir = Some(workspace.root().to_path_buf());
            }
            Some(personal_settings(&home)?)
        } else {
            None
        };
        let mut resolved = args.resolve(default_mode, user_config().as_deref())?;
        let personal = match personal {
            Some(settings) => {
                let mut in_force = match &user_kernel {
                    Some(user) => {
                        KernelSettings::resolve(user_config().as_deref(), &resolved.workspace)
                            .unwrap_or_else(|error| {
                                kernel_notes.push(format!(
                                    "{error}; the workspace's limits are not shown, and \
                                     `adk-rust up` will not start until it is fixed"
                                ));
                                user.clone()
                            })
                    }
                    None => KernelSettings::default(),
                };
                let user = user_kernel.unwrap_or_default();
                up::apply_personal(&mut in_force, &user, &settings);
                Some(PersonalSetup {
                    settings,
                    quiet: in_force.quiet_hours,
                    daily_spend_cap: in_force.quotas.daily_spend_cap,
                })
            }
            None => None,
        };
        resolved.notes.extend(kernel_notes);
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
        let shared = match store {
            // The mode and rule stores of a shared backend are not sealed by a store key, so
            // a configured key refuses rather than write them in plaintext (spec 3.1 I57).
            Some(url) if resolved.store_key.is_some() => bail!(
                "--store keeps rules and modes in {} without the store key's envelope; serve \
                 without --store-key and [store] key and encrypt the shared backend at rest, or \
                 serve from harness.db",
                adk_harness::redact_store_url(url)
            ),
            Some(url) => Some(Arc::new(crate::shared_store::open(url).await?)),
            None => None,
        };
        #[cfg(feature = "persist")]
        let (db, store_note) = if args.ephemeral || shared.is_some() {
            (None, None)
        } else {
            #[cfg(feature = "encrypted")]
            let (keyring, store_note) =
                crate::store::resolve_keyring(resolved.store_key.as_deref()).await?;
            let dir = match home {
                Some(home) => home.to_path_buf(),
                None => crate::sessions::default_dir()?,
            };
            #[cfg(feature = "encrypted")]
            let db = crate::sessions::open_harness_db_in(&dir, keyring.as_ref()).await?;
            #[cfg(not(feature = "encrypted"))]
            let (db, store_note) =
                (crate::sessions::open_harness_db_in(&dir).await?, None::<String>);
            (Some(Arc::new(db)), store_note)
        };
        #[cfg(not(feature = "persist"))]
        let _ = home;
        #[cfg(not(feature = "persist"))]
        if args.session.is_some() || args.continue_session {
            bail!(
                "persistent sessions are not compiled into this adk-cli build. Reinstall with \
                 `--features persist`, or start without --session and --continue."
            );
        }
        let models = Models { harness: model.built() };
        let embedder =
            if resolved.memory { embedder::build(&model, resolved.embedder)? } else { None };
        #[cfg(feature = "persist")]
        let plans =
            plan_checkpointer(&resolved, args.ephemeral, shared.as_deref(), db.as_deref()).await?;
        #[cfg(not(feature = "persist"))]
        let plans = plan_checkpointer(&resolved, args.ephemeral, shared.as_deref()).await?;
        Ok(Self {
            shared,
            plans,
            personal,
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
        if let Some(shared) = &self.shared {
            return shared.sessions.clone();
        }
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
        self.agent_with(
            self.resolved.effort,
            name,
            mode,
            sandbox,
            prompter,
            approval_service,
            AgentExtras::default(),
        )
    }

    /// [`agent`](Self::agent) at the effort `policy`, which `/effort` replaces, with
    /// `extras`.
    #[allow(clippy::too_many_arguments)]
    fn agent_with(
        &self,
        effort: adk_harness::EffortPolicy,
        name: &str,
        mode: HarnessMode,
        sandbox: bool,
        prompter: Option<Arc<dyn ApprovalPrompter>>,
        approval_service: Option<Arc<dyn ApprovalService>>,
        extras: AgentExtras,
    ) -> Result<HarnessAgent> {
        let workspace = Workspace::new(&self.resolved.workspace).allow_bash(true);
        let mut builder = HarnessAgent::builder(name);
        // An added agent runs on its own model when it names one (spec 3.3 decision 21).
        let model = extras
            .added
            .as_ref()
            .and_then(|added| added.model.clone())
            .unwrap_or_else(|| self.models.harness.clone());
        // A preset first, so the flags and config files override what it sets (spec 3.3 §10
        // H1).
        if let Some(preset) = self.args.preset {
            builder = builder.preset(preset);
        }
        let mut builder = builder
            .model(model.llm.clone())
            .effort(effort)
            .workspace(workspace)
            .mode(mode)
            .confirmation_timeout(adk_cli::launcher::CONSOLE_CONFIRMATION_TIMEOUT)
            // What the person can type, so the model names it (spec 3.1 §7.25); `up` keeps
            // the inbox-backed memory, so it marks a harness under the kernel.
            .surface(manifest::cli_surface(extras.memory_with_inbox));
        // Hooks (spec 3.1 §7.22, §8.1): the user config's run before the workspace's, which
        // the harness reads at build; --no-hooks runs none.
        if !self.resolved.hooks {
            builder = builder.without_hooks();
        } else if !self.resolved.user_hooks.is_empty() {
            builder = builder.hooks(self.resolved.user_hooks.clone());
        }
        // Explicit context caching for a model that supports it (spec 3.1 §7.23).
        if let Some(cache) = &model.cache_capable {
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
                .goal_store(db.goals.clone())
                .read_records(db.reads.clone());
            // Long-term memory in harness.db; off when served (spec 3.1 §7.15.2), on in
            // `adk-rust up`, the user's own kernel (spec 3.2 §8.1).
            if self.resolved.memory && (approval_service.is_none() || extras.memory_with_inbox) {
                builder = builder
                    .long_term_memory(adk_harness::LongTermMemoryConfig::default())
                    .fact_store(db.facts.clone());
                if let Some((embedder, _)) = &self.embedder {
                    builder = builder.memory_embedder(embedder.clone());
                }
            }
        }
        if let Some(shared) = &self.shared {
            builder = builder
                .mode_store(shared.modes.clone())
                .standing_rules(shared.rules.clone())
                .approval_store(shared.approvals.clone())
                .read_records(shared.reads.clone());
        }
        // Snapshots before each turn (spec 3.1 §7.27, §13 Q11): records in harness.db and
        // objects beside it, in memory with --ephemeral; a harness served from a shared store
        // keeps none, since the replicas share no workspace.
        if let Some(config) = &self.resolved.snapshots
            && self.shared.is_none()
        {
            builder = builder.snapshots(config.clone());
            #[cfg(feature = "persist")]
            if let Some(db) = &self.db {
                builder = builder
                    .snapshot_store(db.snapshots.clone())
                    .snapshot_objects(Arc::new(db.snapshot_objects(&self.resolved.workspace)));
            }
        }
        // An area's mode the person changes while the kernel runs governs the harness's
        // sessions from their next step (spec 3.3 §10 K3).
        if let Some((control, area, auto_refused)) = &extras.area_modes {
            let mut modes: Arc<dyn adk_harness::ModeStore> =
                Arc::new(adk_harness::InMemoryModeStore::new());
            #[cfg(feature = "persist")]
            if let Some(db) = &self.db {
                modes = db.modes.clone();
            }
            if let Some(shared) = &self.shared {
                modes = shared.modes.clone();
            }
            builder = builder.mode_store(Arc::new(books::AreaModeStore::new(
                modes,
                control.clone(),
                area.clone(),
                mode,
                *auto_refused,
            )));
        }
        if let Some((checkpointer, _)) = &self.plans {
            builder = builder.plan_checkpointer(checkpointer.clone());
            // Goals run on the loop engine in the CLI and on the checkpointed graph when
            // served (spec 3.1 §7.11).
            if approval_service.is_none() {
                builder = builder.goal_engine(adk_harness::GoalEngine::Loop);
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
            builder = builder.approval_service(service);
            #[cfg(feature = "tui")]
            let local_console = extras.local_console;
            #[cfg(not(feature = "tui"))]
            let local_console = false;
            if !local_console {
                builder = builder.require_authenticated_identity();
            }
        }
        // An added agent has the packs its definition names, never the primary agent's
        // (spec 3.3 §5.7).
        let packs = match &extras.added {
            Some(added) => agents::packs_of(&added.record),
            None => self.resolved.packs.clone(),
        };
        for pack in &packs {
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
        for member in self.resolved.members.iter().filter(|_| extras.added.is_none()) {
            builder = builder.member(member.to_spec()?);
        }
        if self.resolved.web {
            // The fetch allowlist and the search filter are separate (spec 3.1 §7.32).
            builder = builder.web(adk_harness::WebConfig {
                allowed_domains: self.resolved.allow_domains.clone(),
                search_domains: self.resolved.search_domains.clone(),
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
        // `ultracode` bounds each implement turn and records into its run's log (spec 3.1
        // §7.10, I63).
        if let Some(budget) = extras.budget {
            builder = builder.budget(budget);
        }
        if let Some(recorder) = extras.audit {
            builder = builder.audit_log(recorder);
        }
        match (extras.partition, extras.drivers) {
            // A partition's harness: its mode, identity, memory scope, crossing policy and
            // the drivers its own list names (spec 3.2 §7.7, §7.8); the coordinator's also
            // reaches the other partitions' drivers, each call a crossing (K-I15).
            (Some((partition, table, coordinator)), drivers) => {
                builder = builder.partition(partition, table);
                if coordinator {
                    builder = builder.partition_coordinator();
                }
                if let Some(drivers) = drivers {
                    builder = builder.partition_drivers(&drivers);
                    if coordinator && !extras.driver_crossings.is_empty() {
                        builder =
                            builder.partition_driver_crossings(&drivers, extras.driver_crossings);
                    }
                }
            }
            (None, Some(drivers)) => {
                // An added agent reaches only the drivers its definition names (spec 3.3 §5.7).
                let toolset = match &extras.added {
                    Some(added) => drivers.toolset_for(
                        &adk_kernel::PartitionId::default(),
                        &added.record.uses.drivers,
                    ),
                    None => drivers.toolset_for_manifests(&adk_kernel::PartitionId::default()),
                };
                builder = builder
                    .toolset(toolset)
                    .tool_classes(Arc::new(adk_harness::DriverClasses::new(drivers.clone())))
                    .add_operator_policy(drivers.policy());
            }
            (None, None) => {}
        }
        // The primary agent suggests agents and stamps its threads; an added agent is lowered
        // from its record beside the primary, after its area (spec 3.3 §5.7, §10 H3, L1).
        if let Some(handle) = extras.agents {
            builder = builder.agents_handle(handle);
        }
        if let Some(stamp) = extras.session_stamp {
            builder = builder.session_stamp(stamp);
        }
        if let Some(added) = &extras.added {
            builder = added.lower(builder);
        }
        // Servers the workspace declares in `.mcp.json` start once the workspace is trusted
        // (spec 3.1 §7.21, I3).
        if let Some(servers) =
            workspace_mcp_servers(&self.resolved.workspace)?.filter(|_| extras.added.is_none())
        {
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
        let session = self.session_id().await?;
        Ok(self.launcher_in(agent, session))
    }

    /// A launcher for `agent` over this setup's sessions and user, in `session`.
    fn launcher_in(&self, agent: HarnessAgent, session: Option<String>) -> HarnessLauncher {
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
        if let Some(session) = session {
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
        launcher = launcher.with_status_line("Plans", self.plans_note());
        launcher = launcher.with_status_line("Snapshots", self.snapshots_note());
        // The embedder is named in every session: recall queries carry the user's messages
        // (spec 3.1 §7.32, I73).
        if self.memory_on() {
            launcher = launcher.with_status_line("Embedder", self.embedder_label());
        }
        if let Some(personal) = &self.personal {
            for (label, line) in personal.status_lines() {
                launcher = launcher.with_status_line(label, line);
            }
        }
        for note in &self.resolved.notes {
            launcher = launcher.with_banner_note(note.clone());
        }
        launcher
    }

    /// Where snapshots go, for the banner (spec 3.1 §7.27).
    fn snapshots_note(&self) -> String {
        let Some(config) = &self.resolved.snapshots else {
            return "off (--no-snapshots or [snapshots] enabled = false): /rewind restores no file"
                .into();
        };
        if self.shared.is_some() {
            return "off: a harness served from a shared store keeps none".into();
        }
        #[cfg(feature = "persist")]
        let place = match &self.db {
            Some(db) => format!("beside harness.db in {}", db.snapshots_dir().display()),
            None => "in memory (--ephemeral)".into(),
        };
        #[cfg(not(feature = "persist"))]
        let place = "in memory".to_string();
        format!(
            "before each turn, {place}; last {} turns, {} MiB; /rewind, /redo",
            config.keep_turns,
            config.max_bytes / (1024 * 1024)
        )
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

    /// Where plans are checkpointed, for the banner (spec 3.1 §13 Q5).
    fn plans_note(&self) -> String {
        match &self.plans {
            Some((_, label)) => format!("checkpointed in {label}"),
            None => "in memory; an approved plan does not survive a restart".into(),
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
        if let Some(notice) =
            self.memory_notice_text("/memory lists them, /memory forget ID removes one")
        {
            println!("{notice}\n");
        }
    }

    /// The memory notice, once: `None` when memory is off or the notice was shown before.
    /// `commands` says how the facts are listed and removed where it is shown.
    fn memory_notice_text(&self, commands: &str) -> Option<String> {
        if !self.memory_on() {
            return None;
        }
        let dir = dirs::config_dir().map(|dir| dir.join("adk-rust"))?;
        let marker = dir.join("memory-notice");
        if marker.exists() {
            return None;
        }
        let recall = match &self.embedder {
            Some((_, name)) => format!("Related facts are found with {name}."),
            None => "Facts are found by shared words; nothing leaves this machine for it.".into(),
        };
        let notice = format!(
            "Long-term memory is on. The harness keeps short facts from your sessions: the \
             preferences you state, how a project builds and tests, what fixed a failure. They \
             live in {} on this machine, with personal data redacted and secrets refused, and \
             are recalled in later sessions. What you said is \"known\"; what the agent \
             inferred is \"assumed\" until you confirm it. {recall}\n{commands}, \
             `adk-rust memory export` writes them out, and \
             --no-memory turns memory off. Under `adk-rust up` in ask or auto mode, a \
             consolidation tick also tidies assumed facts after new sessions — `adk-rust memory \
             undo TICK` reverts one, --no-consolidation turns it off. This notice is shown once.",
            dir.join("harness.db").display()
        );
        let _ = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&marker, "shown\n"));
        Some(notice)
    }

    /// What embeds long-term facts, as the banner names it (spec 3.1 §7.32).
    fn embedder_label(&self) -> String {
        self.embedder
            .as_ref()
            .map_or_else(|| embedder::LEXICAL.to_string(), |(_, name)| name.clone())
    }

    fn memory_on(&self) -> bool {
        #[cfg(feature = "persist")]
        return self.resolved.memory && self.db.is_some();
        #[cfg(not(feature = "persist"))]
        false
    }
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
                        AgentExtras::default(),
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
        "harness ({}) on {} · mode {} · shell {}\nplans {}\ntask: {task}\n",
        setup.model_id,
        setup.resolved.workspace.display(),
        setup.resolved.mode,
        shell_label(&info),
        setup.plans_note()
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

/// `adk-rust --voice`: the harness over a realtime session (spec 3.1 §8.1, §7.17, H5).
///
/// The harness is built marked `Pack::Voice` with no console prompter: a call that needs a
/// decision is held and answered by voice, by `--voice-input`, or in the inbox
/// (`adk-rust approvals`), and resumed. Exit code 3 means a call is still held.
#[cfg(feature = "voice")]
pub async fn voice(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
    voice: crate::cli::VoiceArgs,
) -> Result<i32> {
    use adk_session::{CreateRequest, GetRequest};

    // The realtime session reads the provider and its key from the resolved model.
    let mut setup = Setup::new(model.clone(), instruction, args, HarnessMode::Ask).await?;
    setup.print_notes();
    setup.memory_notice();
    if !setup.resolved.packs.contains(&adk_harness::Pack::Voice) {
        setup.resolved.packs.push(adk_harness::Pack::Voice);
    }
    let agent = setup.agent(APP_NAME, setup.resolved.mode, setup.resolved.sandbox, None, None)?;
    let info = agent.info().clone();
    let sessions = setup.sessions();
    let session_id = match setup.session_id().await? {
        Some(id) => id,
        None => uuid::Uuid::new_v4().to_string(),
    };
    let existing = sessions
        .get(GetRequest {
            app_name: APP_NAME.to_string(),
            user_id: setup.user.clone(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await;
    if existing.is_err() {
        sessions
            .create(CreateRequest {
                app_name: APP_NAME.to_string(),
                user_id: setup.user.clone(),
                session_id: Some(session_id.clone()),
                state: std::collections::HashMap::new(),
            })
            .await?;
    }
    #[cfg(feature = "scripted-model")]
    let script = voice.voice_script.as_deref();
    #[cfg(not(feature = "scripted-model"))]
    let script = None;
    let realtime = crate::voice_cli::realtime_model(&model, script)?;
    println!(
        "harness ({}) voice on {} · mode {} · shell {} · realtime {}\n",
        setup.model_id,
        setup.resolved.workspace.display(),
        setup.resolved.mode,
        shell_label(&info),
        realtime.id
    );
    let surface = adk_harness::VoiceSurface::open(
        Arc::new(agent),
        realtime.model,
        sessions,
        setup.user.clone(),
        session_id.clone(),
        adk_harness::VoiceConfig::new(APP_NAME),
    )?;
    let code = crate::voice_cli::run(
        Arc::new(surface),
        voice.voice.as_deref().filter(|text| !text.trim().is_empty()),
        voice.voice_input.as_deref(),
        voice.voice_output.as_deref(),
        realtime.input_rate,
    )
    .await?;
    println!("session: {session_id}");
    Ok(code)
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
    // Where the mirror's directory resolves before the agent runs (spec 3.1 §8.1).
    let mirror = goal
        .state
        .as_deref()
        .map(|path| OutputFile::prepare(Path::new(path), &setup.resolved.workspace))
        .transpose()?;
    // A running kernel over harness.db runs the goal on its background queue
    // (spec 3.2 §8.1); `--resume` continues here.
    #[cfg(feature = "persist")]
    if !goal.resume
        && setup.db.is_some()
        && let Some(client) = crate::kernel_cli::local().await?
    {
        return goal_on_kernel(&setup, client, goal, mirror).await;
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
    println!("plans {}", setup.plans_note());
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
    if let (Some(mirror), Some(state)) = (&mirror, &state) {
        write_goal_mirror(mirror, state)?;
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

/// `adk-rust goal` with a kernel running (spec 3.2 §8.1): the goal is enqueued on the
/// shared stores for the kernel's background worker, and this terminal follows it over the
/// control plane until it ends, holds a call no one here decides, or Ctrl-C detaches —
/// which leaves it running.
#[cfg(feature = "persist")]
async fn goal_on_kernel(
    setup: &Setup,
    client: adk_kernel::control::ControlClient,
    goal: GoalArgs,
    mirror: Option<OutputFile>,
) -> Result<i32> {
    use crate::kernel_cli::Followed;

    let Some(db) = &setup.db else {
        bail!("a goal on the kernel is enqueued in harness.db");
    };
    let console = ConsoleIo::terminal()?;
    // The kernel's harness asks; this one only enqueues.
    let agent = setup.agent(APP_NAME, setup.resolved.mode, setup.resolved.sandbox, None, None)?;
    let mut launcher = setup
        .launcher(agent)
        .await?
        .with_console(console.clone())
        .with_run_store(db.runs.clone())
        .with_goal_queue(adk_kernel::QueueClass::BACKGROUND);
    let spec = GoalSpec {
        goal: goal.goal.clone(),
        until: goal.until.clone(),
        max_iterations: goal.max_iters,
        check_timeout: goal.check_timeout,
    };
    let mut cursor = Some(chrono::Utc::now());
    let pid = launcher.enqueue_goal(spec).await?;
    let session = launcher.session_id().unwrap_or_default().to_string();
    println!("goal mode on the kernel at {}", client.endpoint());
    println!("goal:  {}", goal.goal);
    println!("until: {}  (budget: {} iterations)", goal.until, goal.max_iters);
    println!("process {pid} · session {session}");
    println!(
        "Ctrl-C detaches; the goal keeps running. Follow it again with `adk-rust attach          --session {session}`.\n"
    );
    let terminal = console.is_terminal().then_some(&console);
    let followed =
        crate::kernel_cli::follow(&client, terminal, &session, &mut cursor, Some(&pid)).await?;
    let state = launcher.goal().await?;
    if let (Some(mirror), Some(state)) = (&mirror, &state) {
        write_goal_mirror(mirror, state)?;
    }
    Ok(match followed {
        Followed::Done(_) => {
            let outcome = TurnOutcome { session_id: session, ..TurnOutcome::default() };
            goal_exit(state.as_ref(), &outcome)
        }
        Followed::Held(calls) => {
            println!(
                "\n{calls} call(s) wait for a decision. The goal waits on the kernel: decide                  with `adk-rust approvals` or from any surface."
            );
            EXIT_HELD
        }
        Followed::Detached => {
            println!("\ndetached; the goal keeps running on the kernel (process {pid}).");
            0
        }
    })
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
fn write_goal_mirror(mirror: &OutputFile, state: &GoalState) -> Result<()> {
    let text = serde_json::to_string_pretty(&json!({
        "goal": state.goal,
        "until": state.until,
        "iteration": state.iteration,
        "status": serde_json::to_value(state.status)?,
        "lastCheck": state.last_check,
    }))?;
    mirror.write(text.as_bytes())
}

/// A file the CLI writes when a run ends, as the operator and outside any sandbox (spec 3.1
/// §8.1): an agent can place links in its workspace, so the write is refused when a
/// link inside the workspace is on the way to the file, or when the file's directory
/// resolves elsewhere than it did before the agent ran. The content goes to a new file of an
/// unpredictable name, created only if nothing is there, then renamed over the target,
/// which replaces a link rather than following it.
struct OutputFile {
    /// The path, absolute and lexically normal.
    path: std::path::PathBuf,
    /// Its directory with every link resolved, before the run.
    dir: std::path::PathBuf,
    /// The workspace, resolved.
    workspace: std::path::PathBuf,
}

impl OutputFile {
    fn prepare(path: &Path, workspace: &Path) -> Result<Self> {
        let absolute = std::path::absolute(path)
            .with_context(|| format!("failed to resolve {}", path.display()))?;
        let mut normal = std::path::PathBuf::new();
        for part in absolute.components() {
            match part {
                std::path::Component::ParentDir => {
                    normal.pop();
                }
                std::path::Component::CurDir => {}
                other => normal.push(other),
            }
        }
        if normal.file_name().is_none() {
            bail!("{} names no file", path.display());
        }
        let parent = normal.parent().unwrap_or(Path::new("/"));
        let dir = resolve_dir(parent)
            .with_context(|| format!("failed to resolve {}", parent.display()))?;
        let workspace = resolve_dir(workspace)
            .with_context(|| format!("failed to resolve {}", workspace.display()))?;
        let file = Self { path: normal, dir, workspace };
        file.refuse_workspace_links()?;
        Ok(file)
    }

    /// Refuses a link inside the workspace on the way to the file's directory.
    fn refuse_workspace_links(&self) -> Result<()> {
        for ancestor in self.path.ancestors().skip(1) {
            let is_link = std::fs::symlink_metadata(ancestor)
                .is_ok_and(|metadata| metadata.file_type().is_symlink());
            let in_workspace = ancestor
                .parent()
                .and_then(|container| resolve_dir(container).ok())
                .is_some_and(|container| container.starts_with(&self.workspace));
            if is_link && in_workspace {
                bail!(
                    "refusing to write {}: {} is a link inside the workspace, which an agent \
                     may have placed; choose a path without one",
                    self.path.display(),
                    ancestor.display()
                );
            }
        }
        Ok(())
    }

    fn write(&self, bytes: &[u8]) -> Result<()> {
        use std::io::Write as _;
        let (Some(parent), Some(name)) = (self.path.parent(), self.path.file_name()) else {
            bail!("{} names no file", self.path.display());
        };
        self.refuse_workspace_links()?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let now = std::fs::canonicalize(parent)
            .with_context(|| format!("failed to resolve {}", parent.display()))?;
        if now != self.dir {
            bail!(
                "refusing to write {}: its directory now resolves to {}, not to {} as when the \
                 run started; a link was placed on the way during the run",
                self.path.display(),
                now.display(),
                self.dir.display()
            );
        }
        let tmp = now.join(format!(".{}.{}.tmp", name.to_string_lossy(), uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("failed to create {}", tmp.display()))?;
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        let renamed = written.and_then(|()| std::fs::rename(&tmp, now.join(name)));
        if let Err(error) = renamed {
            std::fs::remove_file(&tmp).ok();
            return Err(error).with_context(|| format!("failed to write {}", self.path.display()));
        }
        Ok(())
    }
}

/// `dir` with its deepest existing ancestor canonicalized, which resolves every link on the
/// way, and the components that do not exist yet appended.
fn resolve_dir(dir: &Path) -> std::io::Result<std::path::PathBuf> {
    let mut existing = dir.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(mut resolved) => {
                resolved.extend(missing.iter().rev());
                return Ok(resolved);
            }
            Err(error) => {
                let Some(name) = existing.file_name().map(std::ffi::OsStr::to_os_string) else {
                    return Err(error);
                };
                missing.push(name);
                if !existing.pop() {
                    return Err(error);
                }
            }
        }
    }
}

/// `adk-rust ultracode` beyond the shared flags (spec 3.1 §8.1).
pub struct UltracodeArgs {
    pub task: String,
    /// `--max-rounds`.
    pub max_rounds: Option<i64>,
    /// `--architecture`.
    pub architecture: Option<ArchitectureChoice>,
    /// `--router`.
    pub router: Option<RouterMode>,
    /// `--max-model-calls`.
    pub max_model_calls: Option<u64>,
    /// `--receipt`.
    pub receipt: Option<std::path::PathBuf>,
}

/// `sha256:<hex>` of `bytes`.
fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// `adk-rust ultracode` on the harness (spec 3.1 §7.10, §8.1): one implement turn in the
/// user's mode, the review routed once to the parallel graph or a Blackboard board, then
/// review and revise under the run's model-call budget, every step in one audit log.
pub async fn ultracode(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
    ultra: UltracodeArgs,
) -> Result<i32> {
    let setup = Arc::new(Setup::new(model, instruction, args, HarnessMode::Ask).await?);
    setup.print_notes();
    let flags = UltracodeFlags {
        architecture: ultra.architecture,
        router: ultra.router,
        max_rounds: ultra.max_rounds,
        max_model_calls: ultra.max_model_calls,
    };
    let limits =
        UltracodeLimits::resolve(user_config().as_deref(), &setup.resolved.workspace, &flags)?;
    for note in &limits.notes {
        println!("note: {note}");
    }
    // Where the receipt's directory resolves before the implementer runs (spec 3.1 §8.1).
    let receipt = ultra
        .receipt
        .as_deref()
        .map(|path| OutputFile::prepare(path, &setup.resolved.workspace))
        .transpose()?;
    let task = ultra.task;
    let run_id = uuid::Uuid::new_v4().to_string();
    // One signed log per run: the implementer's, the reviewers' and the board's records and
    // the run's own, in one hash chain (spec 3.1 §9.3).
    let log = Arc::new(adk_harness::EphemeralAuditLog::new()?);
    let recorder: Arc<dyn AuditRecorder> = log.clone();
    let session_id = match setup.session_id().await? {
        Some(session) => session,
        None => uuid::Uuid::new_v4().to_string(),
    };
    let console = ConsoleIo::terminal()?;
    let prompter = setup.unattended_prompter(&console);
    let agent = setup.agent_with(
        setup.resolved.effort,
        APP_NAME,
        setup.resolved.mode,
        setup.resolved.sandbox,
        prompter,
        None,
        AgentExtras {
            budget: Some(RunBudget::new().max_model_calls(limits.implement_model_calls)),
            audit: Some(recorder.clone()),
            memory_with_inbox: false,
            ..AgentExtras::default()
        },
    )?;
    let runtime = agent.runtime().clone();
    let launcher = setup.launcher_in(agent, Some(session_id.clone())).with_console(console);
    let audit = UltracodeAudit::new(recorder.clone(), &setup.user, &run_id, session_id);
    let task_digest = sha256(task.as_bytes());
    audit
        .record(
            "harness.ultracode.started",
            json!({
                "runId": run_id,
                "taskDigest": task_digest,
                "requested": limits.architecture,
                "source": limits.architecture_source,
                "routerMode": limits.router,
                "limits": limits,
                "worstCaseModelCalls": limits.worst_case_model_calls(),
                "modelCallBound": limits.model_call_bound(),
                "notes": limits.notes,
            }),
        )
        .await?;

    let workspace = setup.resolved.workspace.clone();
    println!("ultracode ({}) on {}", setup.model_id, workspace.display());
    println!("task: {task}");
    println!(
        "budget: at most {} model calls (worst case {}) · max rounds {}\n",
        limits.model_call_bound(),
        limits.worst_case_model_calls(),
        limits.max_rounds
    );

    let roles = RoleModels {
        router: setup.models.harness.llm.clone(),
        reviewers: std::collections::BTreeMap::new(),
        default_reviewer: setup.models.harness.llm.clone(),
        board: setup.models.harness.llm.clone(),
    };
    let before = snapshot(&workspace, &limits).await;
    let agents = Arc::new(HarnessUltra {
        setup: setup.clone(),
        implementer: tokio::sync::Mutex::new(launcher),
        implementer_runtime: runtime,
        held: std::sync::Mutex::new(None),
        last_turn: std::sync::Mutex::new(TurnStatus::Completed),
        start: before.clone(),
        after_turn: tokio::sync::Mutex::new(None),
        plan: std::sync::Mutex::new(None),
        escalated: std::sync::atomic::AtomicBool::new(false),
        roles,
        limits: limits.clone(),
        recorder,
        audit: audit.clone(),
        run_id: run_id.clone(),
        task_digest: task_digest.clone(),
        calls: std::sync::atomic::AtomicU64::new(0),
        charged: std::sync::atomic::AtomicU64::new(0),
    });

    println!("━━ implement ━━");
    agents.implement(&task).await;
    let after = agents.after_turn.lock().await.clone().unwrap_or_default();
    let mut change = before.diff_with(&after, &limits);
    change.first_turn = *agents.last_turn.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let inputs = RouteInputs {
        features: TaskFeatures::extract(&task),
        change,
        repo: RepoSignals::from_snapshot(&after),
        limits: limits.clone(),
    };
    // The router call runs at the session's `members` effort (spec 3.1 §7.23).
    let decision =
        route(&agents.roles.router, Some(setup.resolved.effort.members), &task, inputs).await;
    agents.charged.store(decision.usage.model_calls, std::sync::atomic::Ordering::SeqCst);
    // Write-ahead: no review starts unless the routing decision is on record (I62).
    audit.record("harness.ultracode.routed", decision.record_ext(&run_id, &task_digest)).await?;
    print_route(&decision, &limits);
    *agents.plan.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(decision.plan.clone());

    let plan = ultra::ReviewLoop::routed(&decision.plan, decision.plan.escalated(&limits).as_ref());
    let budget = ultra::UltraBudget {
        max_model_calls: limits.max_model_calls,
        implement_model_calls: limits.implement_model_calls,
        reviewer_model_calls: limits.reviewer_model_calls,
        board_model_calls: limits.board_model_calls,
    };
    let outcome =
        ultra::run_plan(agents.clone(), &plan, &budget, &task, decision.usage.model_calls).await?;
    let held = agents.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    audit
        .record(
            "harness.ultracode.finished",
            json!({
                "runId": run_id,
                "rounds": outcome.rounds,
                "decision": outcome.decision,
                "architecture": decision.plan.architecture,
                "escalated": agents.escalated.load(std::sync::atomic::Ordering::SeqCst),
                "modelCalls": agents.model_calls() + decision.usage.model_calls,
                "heldCalls": held.as_ref().map_or(0, |outcome| outcome.held.len()),
            }),
        )
        .await?;
    if let Some(receipt) = &receipt {
        let raw = decision.router.as_ref().and_then(|router| router.raw.clone());
        write_receipt(receipt, &log, &run_id, raw).await?;
        println!("receipt: {}", receipt.path.display());
    }
    Ok(match held {
        Some(outcome) => held_exit(&outcome, persistent(&setup)),
        None => 0,
    })
}

/// A metadata snapshot of `workspace` that enters the skipped directories the `sensitive_paths`
/// of `limits` name, taken off the async runtime.
async fn snapshot(workspace: &Path, limits: &UltracodeLimits) -> WorkspaceSnapshot {
    let root = workspace.to_path_buf();
    let globs: Vec<String> =
        limits.sensitive_paths.iter().chain(&limits.workspace_sensitive_paths).cloned().collect();
    tokio::task::spawn_blocking(move || WorkspaceSnapshot::capture_with(&root, &globs))
        .await
        .unwrap_or_default()
}

/// Why `change` is sensitive, for the console: the first sensitive path, or the cap.
fn sensitive_reason(change: &ChangeSignals) -> String {
    let path = change
        .protected
        .iter()
        .chain(&change.security)
        .chain(&change.sensitive_matched)
        .chain(&change.workspace_sensitive_matched)
        .next();
    match path {
        Some(path) => format!("a sensitive path changed: {}", adk_harness::escape_control(path)),
        None if change.truncated => format!(
            "the workspace has more than {SNAPSHOT_MAX_FILES} files, so the change could not \
             be seen whole"
        ),
        None => "the change is sensitive".to_string(),
    }
}

/// Prints the routed plan and why it was chosen; a model's rationale is escaped and shown
/// here only, never to an agent (spec 3.1 §9.2 I62).
fn print_route(decision: &RouteDecision, limits: &UltracodeLimits) {
    let plan = &decision.plan;
    let rule = match &decision.fallback {
        Some(fallback) => format!("{}: {}", decision.rule, fallback.code),
        None => decision.rule.to_string(),
    };
    let reviewers: Vec<&str> = plan.reviewers.iter().map(|focus| focus.name()).collect();
    println!(
        "route: {} ({rule}) · reviewers {} · board rounds {} · review rounds {}",
        plan.architecture,
        reviewers.join(", "),
        plan.board_rounds,
        plan.review_rounds
    );
    let change = &decision.inputs.change;
    let why = match decision.rule {
        Rule::Override => format!(
            "{} set the architecture to {}",
            match limits.architecture_source {
                SettingSource::Flag => "--architecture",
                SettingSource::ProjectConfig => adk_cli::harness::PROJECT_CONFIG_FILE,
                SettingSource::UserConfig | SettingSource::Default => "your config",
            },
            limits.architecture
        ),
        Rule::Unchanged => match change.first_turn {
            TurnStatus::Completed => {
                "the first turn changed no file; the fixed review applies".to_string()
            }
            status => format!("the first turn ended {status:?}; the fixed review applies"),
        },
        Rule::Sensitive => sensitive_reason(change),
        Rule::Model => {
            decision.router.as_ref().and_then(|router| router.rationale.clone()).unwrap_or_default()
        }
        Rule::Heuristic => format!(
            "heuristic score {} ({})",
            decision.heuristic.score,
            if decision.heuristic.triggers.is_empty() {
                "no trigger".to_string()
            } else {
                decision.heuristic.triggers.join(", ")
            }
        ),
    };
    println!("why: {why}\n");
}

/// Writes the `--receipt` file atomically: the run's `harness.ultracode.*` records, the
/// verification of the whole signed log, and the router's raw answer.
async fn write_receipt(
    receipt_file: &OutputFile,
    log: &adk_harness::EphemeralAuditLog,
    run_id: &str,
    router_answer: Option<String>,
) -> Result<()> {
    let verified = log.verify().await?;
    let records: Vec<adk_core::audit_log::AuditRecord> = log
        .records()
        .await?
        .into_iter()
        .filter(|record| record.action.starts_with("harness.ultracode."))
        .collect();
    let ext = |record: &adk_core::audit_log::AuditRecord| {
        serde_json::Value::Object(record.ext.clone().into_iter().collect())
    };
    let one = |action: &str| records.iter().find(|record| record.action == action).map(ext);
    let reviewed: Vec<serde_json::Value> = records
        .iter()
        .filter(|record| record.action == "harness.ultracode.reviewed")
        .map(ext)
        .collect();
    let receipt = json!({
        "runId": run_id,
        "chainVerified": verified,
        "verifierKey": log.verifier_key(),
        "started": one("harness.ultracode.started"),
        "routed": one("harness.ultracode.routed"),
        "escalated": one("harness.ultracode.escalated"),
        "reviewed": reviewed,
        "finished": one("harness.ultracode.finished"),
        "records": records,
        "routerAnswer": router_answer,
    });
    receipt_file.write(serde_json::to_string_pretty(&receipt)?.as_bytes())
}

/// The models of each `ultracode` role (spec 3.1 §7.10): every role uses the session's
/// client, per focus when one is set, and each call carries its effort from the session's
/// `EffortPolicy` (spec 3.1 §7.23): the router and board seats `members`, independent
/// reviewers `read_only`.
struct RoleModels {
    router: Arc<dyn Llm>,
    reviewers: std::collections::BTreeMap<Focus, Arc<dyn Llm>>,
    default_reviewer: Arc<dyn Llm>,
    board: Arc<dyn Llm>,
}

impl RoleModels {
    fn reviewer(&self, focus: &str) -> Arc<dyn Llm> {
        Focus::from_name(focus)
            .and_then(|focus| self.reviewers.get(&focus))
            .cloned()
            .unwrap_or_else(|| self.default_reviewer.clone())
    }
}

/// Implements in the user's mode in one session; reviews in read-only mode, independently
/// or on a board; counts every model call against the run (spec 3.1 §9.2 I61, I63).
struct HarnessUltra {
    setup: Arc<Setup>,
    implementer: tokio::sync::Mutex<HarnessLauncher>,
    /// The implementer's runtime, whose spend counts its model calls.
    implementer_runtime: adk_harness::HarnessRuntime,
    held: std::sync::Mutex<Option<TurnOutcome>>,
    last_turn: std::sync::Mutex<TurnStatus>,
    /// The workspace before the first turn, which every revise is compared with.
    start: WorkspaceSnapshot,
    /// The workspace after the latest implement or revise turn, which the review stage
    /// that follows must leave as it is (spec 3.1 §9.2 I61).
    after_turn: tokio::sync::Mutex<Option<WorkspaceSnapshot>>,
    /// The plan in effect: the routed one, tightened once a revise makes the change
    /// sensitive.
    plan: std::sync::Mutex<Option<ReviewPlan>>,
    escalated: std::sync::atomic::AtomicBool,
    roles: RoleModels,
    limits: UltracodeLimits,
    recorder: Arc<dyn AuditRecorder>,
    audit: UltracodeAudit,
    run_id: String,
    task_digest: String,
    /// Model calls of the implementer, reviewers and boards.
    calls: std::sync::atomic::AtomicU64,
    /// Model calls made outside the agents: the router's.
    charged: std::sync::atomic::AtomicU64,
}

impl HarnessUltra {
    fn reviewer(&self, focus: &str) -> IndependentReviewer {
        IndependentReviewer::new(self.roles.reviewer(focus), &self.setup.resolved.workspace)
            .effort(self.setup.resolved.effort)
            .guidance(self.setup.instruction.clone())
            .audit_log(self.recorder.clone())
            .user(self.setup.user.clone())
            .model_calls(self.limits.reviewer_model_calls)
    }

    fn charge(&self, calls: u64) {
        self.calls.fetch_add(calls, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl UltraAgents for HarnessUltra {
    async fn implement(&self, prompt: &str) {
        let before = self.implementer_runtime.spend(None).model_calls;
        let mut launcher = self.implementer.lock().await;
        let turn = launcher.run_once(prompt).await;
        // The turn's streamed text ends without a newline.
        println!();
        let status = match turn {
            Ok(outcome) if !outcome.held.is_empty() => {
                println!("  {} call(s) held for a decision", outcome.held.len());
                *self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(outcome);
                TurnStatus::Held
            }
            Ok(outcome) if outcome.interrupted => TurnStatus::Interrupted,
            Ok(outcome) if outcome.ended_by_denial => TurnStatus::Failed,
            Ok(_) => TurnStatus::Completed,
            Err(error) => {
                println!("  implement failed: {error}");
                TurnStatus::Failed
            }
        };
        *self.last_turn.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = status;
        let after = self.implementer_runtime.spend(None).model_calls;
        self.charge(after.saturating_sub(before));
        *self.after_turn.lock().await =
            Some(snapshot(&self.setup.resolved.workspace, &self.limits).await);
    }

    async fn escalate(&self, round: i64) -> Result<bool> {
        let plan = self.plan.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let Some((plan, escalated)) =
            plan.and_then(|plan| plan.escalated(&self.limits).map(|escalated| (plan, escalated)))
        else {
            return Ok(false);
        };
        let Some(after) = self.after_turn.lock().await.clone() else {
            return Ok(false);
        };
        // The whole change since the run began: a revise turn is reviewed as the first
        // turn would have been (spec 3.1 §7.10).
        let mut change = self.start.diff_with(&after, &self.limits);
        change.first_turn =
            *self.last_turn.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !change.is_sensitive() {
            return Ok(false);
        }
        let reviewers: Vec<&str> = escalated.reviewers.iter().map(|focus| focus.name()).collect();
        println!(
            "━━ escalate ━━ {}; reviewers now {}",
            sensitive_reason(&change),
            reviewers.join(", ")
        );
        // Write-ahead: no stage reviews with the new roster unless it is on record (I62).
        self.audit
            .record(
                "harness.ultracode.escalated",
                json!({
                    "runId": self.run_id,
                    "taskDigest": self.task_digest,
                    "routerVersion": ROUTER_VERSION,
                    "round": round,
                    "change": change,
                    "from": plan,
                    "plan": escalated,
                }),
            )
            .await?;
        *self.plan.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(escalated);
        self.escalated.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(true)
    }

    async fn review(&self, focus: &str, desc: &str, prompt: &str) -> String {
        let run = self.reviewer(focus).run(focus, desc, prompt).await;
        self.charge(run.model_calls);
        run.text
    }

    async fn review_board(&self, plan: &ultra::ReviewLoop, prompt: &str) -> ultra::BoardStage {
        let roster: Vec<Focus> =
            plan.reviewers.iter().filter_map(|(focus, _)| Focus::from_name(focus)).collect();
        println!("━━ review board ━━ {} seat(s), {} round(s)", roster.len(), plan.board_rounds);
        let used = self.model_calls() + self.charged.load(std::sync::atomic::Ordering::SeqCst);
        let remaining = self.limits.max_model_calls.saturating_sub(used);
        let fallback = self.reviewer(Focus::Correctness.name());
        let outcome = ReviewBoard::new(self.roles.board.clone(), &self.setup.resolved.workspace)
            .effort(self.setup.resolved.effort)
            .guidance(self.setup.instruction.clone())
            .audit_log(self.recorder.clone())
            .roster(roster)
            .rounds(plan.board_rounds)
            .model_calls(self.limits.board_model_calls)
            .user(self.setup.user.clone())
            .fallback(fallback, remaining)
            .run(prompt)
            .await;
        self.charge(outcome.model_calls);
        if let Some(code) = &outcome.error_code {
            println!("  board ended with {code}");
        }
        ultra::BoardStage {
            seats: outcome
                .seats
                .iter()
                .map(|seat| ultra::SeatReview {
                    focus: seat.focus.name().to_string(),
                    review: match seat.verdict {
                        Verdict::Approve => ultra::Review::Approve,
                        Verdict::Changes => ultra::Review::Changes,
                        Verdict::Failed => ultra::Review::Failed,
                    },
                    notes: seat.notes.clone(),
                })
                .collect(),
            status: match outcome.status {
                BoardStatus::Completed => "completed",
                BoardStatus::Failed => "failed",
                BoardStatus::BudgetExhausted => "budgetExhausted",
            }
            .to_string(),
            fallback: outcome.fallback.iter().map(|focus| focus.name().to_string()).collect(),
        }
    }

    fn model_calls(&self) -> u64 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn stage_reviewed(&self, stage: &ultra::StageReport) -> Result<()> {
        let notes: Vec<&str> = stage.seats.iter().map(|seat| seat.notes.as_str()).collect();
        let now = snapshot(&self.setup.resolved.workspace, &self.limits).await;
        let unchanged = self.after_turn.lock().await.as_ref().map(|turn| *turn == now);
        self.audit
            .record(
                "harness.ultracode.reviewed",
                json!({
                    "runId": self.run_id,
                    "round": stage.round,
                    "architecture": if stage.board { "blackboard" } else { "graph" },
                    "verdicts": stage.seats.iter().map(|seat| json!({
                        "focus": seat.focus,
                        "verdict": match seat.review {
                            ultra::Review::Approve => "approve",
                            ultra::Review::Changes => "changes",
                            ultra::Review::Failed => "failed",
                        },
                    })).collect::<Vec<_>>(),
                    "status": stage.status,
                    "fallback": stage.fallback,
                    "modelCalls": stage.model_calls,
                    "notesDigest": sha256(notes.join("\n").as_bytes()),
                    "workspaceUnchanged": unchanged,
                }),
            )
            .await
            .map_err(Into::into)
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
    store: Option<String>,
) -> Result<()> {
    let setup =
        Setup::open(model, instruction, args, HarnessMode::Plan, store.as_deref(), None).await?;
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
    let (approval_store, run_store) = match &setup.shared {
        Some(shared) => (shared.approvals.clone(), shared.runs.clone()),
        None => (approval_store, run_store),
    };
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

    // One kill switch for the server's runs, the background runs and the web home's turns,
    // so the home's Stop all stops them all (spec 3.3 §11 S-I10), on every replica.
    let governance =
        serve_governance(setup.shared.as_ref().map(|shared| shared.governance.clone())).await?;
    // A turn can run as a durable background run of the `adk-rust` target (spec 3.1 §7.12).
    let runner = adk_runner::Runner::builder()
        .app_name(APP_NAME)
        .agent(agent.agent())
        .session_service(setup.sessions())
        .approval_store(agent.runtime().approval_store().clone())
        .governance(governance.clone())
        .build()?;
    // The runner's root is the harness agent, which runs a session's goal while one runs, so
    // it also serves the durable goal target (spec 3.1 §7.11).
    let runner = Arc::new(runner);
    let background = adk_server::background::BackgroundState::new()
        .with_run_store(run_store)
        .with_app_name(APP_NAME)
        .with_agent(APP_NAME, runner.clone())
        .with_agent(adk_harness::GOAL_TARGET, runner)
        .with_governance(governance.clone());

    let mut launcher = HarnessLauncher::new(agent)
        .app_name(APP_NAME)
        .with_session_service(setup.sessions())
        .with_user_id(setup.user.clone())
        .with_approval_service(service)
        .with_background_runs(background)
        .with_governance(governance)
        .with_bind_address(host);
    if let Some(token) = token {
        launcher = launcher.with_auth_token(token);
    }
    if let Some(shared) = &setup.shared {
        println!(
            "  store {}: sessions, approvals, runs, modes, rules and plans, shared by every replica",
            shared.label
        );
    }
    println!("  plans {}", setup.plans_note());
    if let Some(personal) = &setup.personal {
        for (label, line) in personal.status_lines() {
            println!("  {label}: {line}");
        }
    }
    launcher.run_serve_directly(port).await.map_err(Into::into)
}

/// Where plans and goal graphs are checkpointed (spec 3.1 §7.11, §13 Q5): nowhere but memory
/// with `--ephemeral`; else `--checkpointer` or the user config's `[plan] checkpointer`, sealed
/// under the store key when `harness.db` is; else the store the sessions are in — the
/// `--store` backend or `harness.db`.
async fn plan_checkpointer(
    resolved: &ResolvedHarnessArgs,
    ephemeral: bool,
    shared: Option<&crate::shared_store::SharedStores>,
    #[cfg(feature = "persist")] db: Option<&crate::sessions::HarnessDb>,
) -> Result<Option<(Arc<dyn adk_graph::Checkpointer>, String)>> {
    if ephemeral {
        return Ok(None);
    }
    if let Some(url) = &resolved.checkpointer {
        let checkpointer = adk_harness::checkpointer_from_url(url).await?;
        let label = adk_harness::redact_store_url(url);
        #[cfg(all(feature = "persist", feature = "encrypted"))]
        if let Some(keyring) = db.and_then(|db| db.keyring.clone()) {
            let sealed = adk_harness::SealedCheckpointer::new(checkpointer, keyring);
            return Ok(Some((Arc::new(sealed), format!("{label}, sealed under the store key"))));
        }
        return Ok(Some((checkpointer, label)));
    }
    if let Some(shared) = shared {
        return Ok(Some((shared.checkpointer.clone(), shared.label.clone())));
    }
    #[cfg(feature = "persist")]
    if let Some(db) = db {
        return Ok(Some((db.plans.clone(), "harness.db".into())));
    }
    Ok(None)
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

/// How often a replica of `serve` reads the kill switch the replicas share.
const SERVE_GOVERNANCE_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The kill switch of a served harness (spec 3.3 §11 S-I10): mirrored over the store every
/// replica shares, so Stop all on one replica stops them all; else this process's own.
async fn serve_governance(
    shared: Option<Arc<dyn adk_core::GovernanceStateStore>>,
) -> Result<adk_core::GovernanceControl> {
    let Some(store) = shared else {
        return Ok(adk_core::GovernanceControl::new());
    };
    let control = adk_core::GovernanceControl::mirrored(
        store,
        adk_core::governance::GovernanceMirrorConfig::default()
            .with_poll_interval(SERVE_GOVERNANCE_POLL),
    );
    control.refresh().await?;
    control.spawn_mirror();
    Ok(control)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Replicas of `serve --store` share one kill switch (spec 3.3 §11 S-I10): Stop all on
    /// one freezes the others' runs too.
    #[tokio::test]
    async fn serve_replicas_share_the_kill_switch() {
        let store: Arc<dyn adk_core::GovernanceStateStore> =
            Arc::new(adk_core::governance_state::InMemoryGovernanceStateStore::new());
        let a = serve_governance(Some(store.clone())).await.unwrap();
        let b = serve_governance(Some(store)).await.unwrap();
        a.freeze_recorded(
            "Stop all",
            adk_core::audit_log::AuditActor::subject(adk_core::principal::SubjectRef::human("ana")),
        )
        .await
        .unwrap();
        b.refresh().await.unwrap();
        assert!(b.is_frozen(), "the other replica is stopped too");
        assert!(!serve_governance(None).await.unwrap().is_mirrored(), "one process, its own");
    }

    #[test]
    fn an_output_file_never_follows_a_planted_link() {
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("authorized_keys");
        std::fs::write(&victim, "keep").unwrap();
        let ws = tempfile::tempdir().unwrap();

        // The temporary name the receipt and the goal mirror used to write through.
        let receipt = OutputFile::prepare(&ws.path().join("receipt.json"), ws.path()).unwrap();
        std::os::unix::fs::symlink(&victim, ws.path().join("receipt.json.tmp")).unwrap();
        receipt.write(b"{}").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(std::fs::read_to_string(ws.path().join("receipt.json")).unwrap(), "{}");

        // A link at the target itself is replaced, not followed.
        std::fs::remove_file(ws.path().join("receipt.json")).unwrap();
        std::os::unix::fs::symlink(&victim, ws.path().join("receipt.json")).unwrap();
        receipt.write(b"{\"again\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(!ws.path().join("receipt.json").symlink_metadata().unwrap().is_symlink());

        // A directory link placed during the run.
        let out = OutputFile::prepare(&ws.path().join("out/r.json"), ws.path()).unwrap();
        std::os::unix::fs::symlink(outside.path(), ws.path().join("out")).unwrap();
        let error = out.write(b"{}").unwrap_err().to_string();
        assert!(error.contains("is a link inside the workspace"), "{error}");
        assert!(!outside.path().join("r.json").exists());

        // A directory link placed before the run, by an earlier one.
        let error = OutputFile::prepare(&ws.path().join("out/r.json"), ws.path())
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("is a link inside the workspace"), "{error}");

        // A directory swapped for a link the check cannot see as one: it resolves elsewhere.
        let file = OutputFile {
            path: ws.path().join("sub/r.json"),
            dir: ws.path().join("sub"),
            workspace: outside.path().join("elsewhere"),
        };
        std::fs::create_dir(ws.path().join("real")).unwrap();
        std::os::unix::fs::symlink(ws.path().join("real"), ws.path().join("sub")).unwrap();
        let error = file.write(b"{}").unwrap_err().to_string();
        assert!(error.contains("now resolves to"), "{error}");
        assert!(!ws.path().join("real/r.json").exists());

        // A link outside the workspace, such as the operator's own, is fine.
        let linked = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), linked.path().join("receipts")).unwrap();
        let file = OutputFile::prepare(&linked.path().join("receipts/r.json"), ws.path()).unwrap();
        file.write(b"{}").unwrap();
        assert!(outside.path().join("r.json").exists());
    }
}
