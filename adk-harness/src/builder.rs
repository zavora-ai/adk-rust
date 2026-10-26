//! `HarnessAgentBuilder` (spec 3.1 §7.2).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use adk_agent::{LlmAgentBuilder, LlmEventSummarizer};
use adk_core::{
    ActionLedger, AdkError, ApprovalService, ApprovalStore, EventsCompactionConfig,
    InMemoryActionLedger, InMemoryApprovalStore, InstructionProvider, Llm, ReadonlyContext, Result,
    RunBudget, SpendLedger, Tool, ToolConfirmationHandler, ToolConfirmationPolicy, ToolPolicy,
    Toolset,
};
use adk_devtools::{EditFileTool, GlobTool, GrepTool, ReadFileTool, Workspace, WriteFileTool};

use crate::agent::{HarnessAgent, HarnessInfo, HarnessRuntime, InstructionState};
use crate::approvals::{
    ApprovalPrompter, InMemoryStandingRuleStore, PromptingConfirmationHandler, StandingRuleStore,
};
use crate::instructions::{
    InMemoryWorkspaceTrustStore, InstructionsConfig, ProjectInstructions, WorkspaceTrustStore,
};
use crate::mode::{HarnessMode, InMemoryModeStore, ModeStore};
#[cfg(any(feature = "auth", feature = "payments"))]
use crate::policy::ToolKind;
use crate::policy::{
    HARNESS_TOOL_NAMES, HarnessPolicy, PolicyInner, PolicySwitches, PromptContexts, ShellView,
    ToolClassRegistry,
};
use crate::prompt::HARNESS_SYSTEM_PROMPT;
use crate::shell::{
    InMemoryShellLedger, IsolationStatus, ShellConfig, ShellIsolation, ShellLedger,
    resolve_isolation,
};
use crate::tools::{ExitPlanModeTool, HarnessBashTool, TodoList, UpdateTodoTool, WriteTodosTool};
use crate::toolset::{HarnessToolset, mode_banner, website_banner};

/// A pack: a feature that adds tools or a surface under the same policy
/// (spec 3.1 §7.16–§7.18).
///
/// # Example
///
/// ```rust
/// let pack = adk_harness::Pack::Browser;
/// # let _ = pack;
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Pack {
    /// Browser tools.
    Browser,
    /// Governed computer use.
    ComputerUse,
    /// Payments under mandates.
    Payments,
    /// The voice surface.
    Voice,
    /// The managed form (feature `managed`): `packs::managed::HarnessResolver` serves the
    /// harness on the managed runtime.
    Managed,
}

/// Loop detection over the invocation's tool calls (spec 3.1 §7.13, feature `observe`).
///
/// # Example
///
/// ```rust
/// let guard = adk_harness::LoopGuard { window: 20, max_identical_calls: 3, max_redundant: 5 };
/// # let _ = guard;
/// ```
#[cfg(feature = "observe")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopGuard {
    /// Tool calls examined.
    pub window: usize,
    /// Identical calls that count as a loop.
    pub max_identical_calls: usize,
    /// Redundant calls that count as a loop.
    pub max_redundant: usize,
}

type Customize = Box<dyn FnOnce(LlmAgentBuilder) -> LlmAgentBuilder + Send + 'static>;

/// Builds a [`HarnessAgent`] (spec 3.1 §7.2).
///
/// The builder exposes no `before_tool_callback`, `enhanced_plugin` or
/// `require_tool_confirmation*`: the first two can answer a call before authorization and
/// the third forces a confirmation even when the policy allows, which defeats standing
/// rules. [`customize_llm`](Self::customize_llm) is the escape hatch.
///
/// # Example
///
/// ```rust
/// use adk_harness::{HarnessAgent, HarnessMode, InstructionsConfig};
/// use adk_model::{ScriptedLlm, ScriptedResponse};
/// use std::sync::Arc;
///
/// let dir = tempfile::tempdir().unwrap();
/// let model = Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("done")]));
/// let harness = HarnessAgent::builder("harness")
///     .model(model)
///     .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
///     .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
///     .mode(HarnessMode::Plan)
///     .build()
///     .unwrap();
/// assert!(harness.info().tools.contains(&"exit_plan_mode".to_string()));
/// ```
pub struct HarnessAgentBuilder {
    name: String,
    model: Option<Arc<dyn Llm>>,
    workspace: Option<Workspace>,
    description: Option<String>,
    base_prompt: Cow<'static, str>,
    instruction: Option<String>,
    instructions: InstructionsConfig,
    trust_store: Option<Arc<dyn WorkspaceTrustStore>>,
    mode: HarnessMode,
    mode_store: Option<Arc<dyn ModeStore>>,
    standing_rules: Option<Arc<dyn StandingRuleStore>>,
    shell_ledger: Option<Arc<dyn ShellLedger>>,
    background_shells: crate::shell::BackgroundShellConfig,
    shell_pids: Option<Arc<dyn crate::shell::ShellPidRegistry>>,
    shell_supervisor: Option<Arc<crate::shell::ShellSupervisor>>,
    read_records: Option<Arc<dyn adk_devtools::ReadLedger>>,
    operator_policy: Option<Arc<dyn ToolPolicy>>,
    origin_classes: Vec<Arc<dyn crate::policy::OriginClasses>>,
    prompter: Option<Arc<dyn ApprovalPrompter>>,
    confirmation_handler: Option<Arc<dyn ToolConfirmationHandler>>,
    approval_store: Option<Arc<dyn ApprovalStore>>,
    approval_service: Option<Arc<dyn ApprovalService>>,
    #[cfg(feature = "kernel")]
    kernel_approvals: Arc<crate::kernel::LateApprovalNotifier>,
    #[cfg(feature = "kernel")]
    kernel_agenda: adk_kernel::AgendaHandle,
    #[cfg(feature = "kernel")]
    kernel_ledger: adk_kernel::proc::LateLedgerSink,
    #[cfg(feature = "kernel")]
    partition: crate::partition::PartitionOptions,
    /// The agent a person added that this harness is built for (spec 3.3 §5.7, H3).
    #[cfg(feature = "kernel")]
    added: Option<crate::agents::AddedAgent>,
    /// Where `propose_agent` suggests agents: set on the primary agent only (spec 3.3 §5.7).
    #[cfg(all(feature = "kernel", feature = "web"))]
    agent_proposer: Option<Arc<dyn crate::agents::SuggestAgent>>,
    /// The kernel target this harness stamps its sessions with (spec 3.3 §5.7 "Threads").
    #[cfg(feature = "kernel")]
    session_stamp: Option<String>,
    action_ledger: Option<Arc<dyn ActionLedger>>,
    confirmation_timeout: Duration,
    shell: ShellConfig,
    shell_enabled: bool,
    /// The personal preset offers `bash` only in the OS sandbox or a container (spec 3.3
    /// §10 H1).
    isolated_shell_only: bool,
    /// The website preset leaves `bash` out (spec 3.5 §10 X10).
    preset_without_shell: bool,
    /// The website preset: its mode banner, and no `exit_plan_mode`, since the charter's
    /// autonomy levels set the mode (spec 3.5 §7.11, A-I13).
    website_preset: bool,
    /// The public partition's tools: of the harness's own tools the agent is offered these
    /// and nothing else (spec 3.5 §7.10, A-I2).
    public_only: Option<BTreeSet<String>>,
    /// `allow_unsandboxed_retry` was set explicitly, so a container shell keeps it.
    retry_explicit: bool,
    trust_mcp_read_only_hints: bool,
    todos: bool,
    #[cfg(feature = "skills")]
    skills: crate::skills::SkillsConfig,
    #[cfg(feature = "team")]
    members: Vec<crate::team::MemberSpec>,
    #[cfg(feature = "team")]
    member_registry: Option<Arc<dyn adk_agent::TeamAgentRegistry>>,
    #[cfg(feature = "team")]
    team_policy: Option<adk_agent::TeamPolicy>,
    default_members: bool,
    member_model: Option<Arc<dyn Llm>>,
    plan_graph: bool,
    /// The surface the person uses, for the manifest (spec 3.1 §7.25).
    surface: Option<crate::manifest::SurfaceManifest>,
    /// Pages `harness_help` returns by name (spec 3.1 §7.25).
    help_pages: BTreeMap<String, String>,
    #[cfg(feature = "memory")]
    memory: Option<crate::memory::MemoryConfig>,
    #[cfg(feature = "graph")]
    plan_checkpointer: Option<Arc<dyn adk_graph::Checkpointer>>,
    /// Snapshots and rewind (spec 3.1 §7.27); off until `snapshots` is set.
    snapshots: crate::snapshot::SnapshotSetup,
    goal_store: Option<Arc<dyn crate::goal::GoalStore>>,
    #[cfg(feature = "graph")]
    goal_engine: Option<crate::goal::GoalEngine>,
    #[cfg(feature = "memory")]
    long_term: LongTermOptions,
    budget: Option<RunBudget>,
    spend_ledger: Option<Arc<dyn SpendLedger>>,
    audit_log: Option<Arc<dyn adk_core::audit_log::AuditRecorder>>,
    #[cfg(feature = "auth")]
    identity: IdentityOptions,
    #[cfg(feature = "observe")]
    loop_guard: Option<LoopGuard>,
    tools: Vec<Arc<dyn Tool>>,
    toolsets: Vec<Arc<dyn Toolset>>,
    max_iterations: u32,
    tool_timeout: Duration,
    history_max_events: Option<usize>,
    compaction: Option<EventsCompactionConfig>,
    compaction_enabled: bool,
    customize: Option<Customize>,
    unimplemented: Vec<String>,
    last_todos: Option<Arc<Mutex<TodoList>>>,
    web: Option<crate::web::WebConfig>,
    /// Signs `web_fetch` requests (spec 3.5 §7.3 "Outbound").
    #[cfg(feature = "web")]
    web_signer: Option<Arc<dyn crate::web::RequestSigner>>,
    search_provider: Option<Arc<dyn crate::web::SearchProvider>>,
    /// `None`: the model provider's search when `web` is set (spec 3.1 §7.20).
    provider_web_tools: Option<crate::web::ProviderWebTools>,
    mcp_interaction: crate::mcp::McpInteractionConfig,
    #[cfg(feature = "mcp")]
    mcp_servers: Vec<crate::mcp::McpServerSet>,
    #[cfg(feature = "hooks")]
    hooks: Option<crate::hooks::HookSet>,
    #[cfg(feature = "hooks")]
    hooks_enabled: bool,
    effort: crate::posture::EffortPolicy,
    cache_capable: Option<Arc<dyn adk_core::CacheCapable>>,
    context_caching: bool,
    #[cfg(feature = "payments")]
    payments: Option<crate::packs::payments::PaymentsPack>,
    payments_requested: bool,
    managed_requested: bool,
    #[cfg(feature = "browser")]
    browser: Option<crate::packs::browser::BrowserPack>,
    browser_requested: bool,
    #[cfg(feature = "computer-use")]
    computer_use: Option<crate::packs::computer_use::ComputerUsePack>,
    computer_use_requested: bool,
    voice_requested: bool,
    take_over: Option<crate::take_over::TakeOver>,
}

impl HarnessAgentBuilder {
    /// A builder for a harness named `name` (default `"harness"`).
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            model: None,
            workspace: None,
            description: None,
            base_prompt: Cow::Borrowed(HARNESS_SYSTEM_PROMPT),
            instruction: None,
            instructions: InstructionsConfig::default(),
            trust_store: None,
            mode: HarnessMode::Ask,
            mode_store: None,
            standing_rules: None,
            shell_ledger: None,
            background_shells: crate::shell::BackgroundShellConfig::default(),
            shell_pids: None,
            shell_supervisor: None,
            read_records: None,
            operator_policy: None,
            origin_classes: Vec::new(),
            prompter: None,
            confirmation_handler: None,
            approval_store: None,
            approval_service: None,
            #[cfg(feature = "kernel")]
            kernel_approvals: Arc::default(),
            #[cfg(feature = "kernel")]
            kernel_agenda: adk_kernel::AgendaHandle::default(),
            #[cfg(feature = "kernel")]
            kernel_ledger: adk_kernel::proc::LateLedgerSink::new(),
            #[cfg(feature = "kernel")]
            partition: crate::partition::PartitionOptions::default(),
            #[cfg(feature = "kernel")]
            added: None,
            #[cfg(all(feature = "kernel", feature = "web"))]
            agent_proposer: None,
            #[cfg(feature = "kernel")]
            session_stamp: None,
            action_ledger: None,
            confirmation_timeout: Duration::from_secs(300),
            shell: ShellConfig::default(),
            shell_enabled: true,
            isolated_shell_only: false,
            preset_without_shell: false,
            website_preset: false,
            public_only: None,
            retry_explicit: false,
            trust_mcp_read_only_hints: false,
            todos: true,
            #[cfg(feature = "skills")]
            skills: crate::skills::SkillsConfig::default(),
            #[cfg(feature = "team")]
            members: Vec::new(),
            #[cfg(feature = "team")]
            member_registry: None,
            #[cfg(feature = "team")]
            team_policy: None,
            default_members: true,
            member_model: None,
            plan_graph: cfg!(feature = "graph"),
            surface: None,
            help_pages: BTreeMap::new(),
            #[cfg(feature = "memory")]
            memory: None,
            #[cfg(feature = "graph")]
            plan_checkpointer: None,
            snapshots: crate::snapshot::SnapshotSetup::default(),
            goal_store: None,
            #[cfg(feature = "graph")]
            goal_engine: None,
            #[cfg(feature = "memory")]
            long_term: LongTermOptions::default(),
            budget: None,
            spend_ledger: None,
            audit_log: None,
            #[cfg(feature = "auth")]
            identity: IdentityOptions::default(),
            #[cfg(feature = "observe")]
            loop_guard: None,
            tools: Vec::new(),
            toolsets: Vec::new(),
            max_iterations: 100,
            tool_timeout: Duration::from_secs(300),
            history_max_events: None,
            compaction: None,
            compaction_enabled: true,
            customize: None,
            unimplemented: Vec::new(),
            last_todos: None,
            web: None,
            #[cfg(feature = "web")]
            web_signer: None,
            search_provider: None,
            provider_web_tools: None,
            mcp_interaction: crate::mcp::McpInteractionConfig::default(),
            #[cfg(feature = "mcp")]
            mcp_servers: Vec::new(),
            #[cfg(feature = "hooks")]
            hooks: None,
            #[cfg(feature = "hooks")]
            hooks_enabled: true,
            effort: crate::posture::EffortPolicy::default(),
            cache_capable: None,
            context_caching: true,
            #[cfg(feature = "payments")]
            payments: None,
            payments_requested: false,
            managed_requested: false,
            #[cfg(feature = "browser")]
            browser: None,
            browser_requested: false,
            #[cfg(feature = "computer-use")]
            computer_use: None,
            computer_use_requested: false,
            voice_requested: false,
            take_over: None,
        }
    }

    /// Governed web access (spec 3.1 §7.20): `web_fetch` under `config`'s allowlist and
    /// caps, and web search — the built-in search of the provider the model's client serves,
    /// or the governed `web_search` when a [`search_provider`](Self::search_provider) is set.
    /// Without feature `web` the configuration is kept and no tool is added.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let harness = HarnessAgent::builder("harness")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .web(WebConfig { allowed_domains: vec!["docs.rs".into()], ..WebConfig::default() })
    ///     .build()?;
    /// ```
    pub fn web(mut self, config: crate::web::WebConfig) -> Self {
        self.web = Some(config);
        self
    }

    /// Signs every `web_fetch` request, each redirect hop included, with `signer`: the
    /// site's Web Bot Auth key, opt-in for general fetches (spec 3.5 §7.3 "Outbound").
    #[cfg(feature = "web")]
    pub fn web_signer(mut self, signer: Arc<dyn crate::web::RequestSigner>) -> Self {
        self.web_signer = Some(signer);
        self
    }

    /// The search backend behind the governed `web_search` (spec 3.1 §7.20); none by
    /// default. A configured provider replaces the model provider's built-in search, so
    /// every search runs under the policy, screening and a per-call record.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let harness = HarnessAgent::builder("harness")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .web(WebConfig::default())
    ///     .search_provider(Arc::new(SearxngSearch::new("http://localhost:8888")?))
    ///     .build()?;
    /// assert!(harness.info().tools.contains(&"web_search".to_string()));
    /// ```
    pub fn search_provider(mut self, provider: Arc<dyn crate::web::SearchProvider>) -> Self {
        self.search_provider = Some(provider);
        self
    }

    /// Provider-executed search and URL tools (spec 3.1 §7.20). Without this call a
    /// harness with [`web`](Self::web) declares
    /// [`ProviderWebTools::provider_default`](crate::web::ProviderWebTools::provider_default)
    /// — the search built-in of the provider the model's client serves — unless a
    /// [`search_provider`](Self::search_provider) is set; one without `web` declares none.
    /// [`ProviderWebTools::off`](crate::web::ProviderWebTools::off) turns them off.
    ///
    /// An enabled tool is attached for the provider the model's client serves only
    /// ([`adk_core::Llm::native_web_search`]), carries the allowlist where the provider
    /// supports one, is declared only to a run starting in Ask or Auto, is recorded as
    /// `harness.web.provider_enabled` once per run, and what it searched is recorded as
    /// `harness.web.searched` after each response (I49).
    pub fn provider_web_tools(mut self, tools: crate::web::ProviderWebTools) -> Self {
        self.provider_web_tools = Some(tools);
        self
    }

    /// How the harness answers what MCP servers ask of it (spec 3.1 §7.21): elicitation,
    /// sampling and resources. Without feature `mcp` the configuration is kept and no
    /// bridge is installed.
    pub fn mcp_interaction(mut self, config: crate::mcp::McpInteractionConfig) -> Self {
        self.mcp_interaction = config;
        self
    }

    /// MCP servers the harness starts and governs (spec 3.1 §7.21): their tools are
    /// `External` tools, their elicitations reach the prompter or the approval inbox,
    /// their sampling runs under the run's budget, and their resources are reachable
    /// through `mcp_list_resources` and `mcp_read_resource`.
    ///
    /// Operator-declared servers (`trusted: true`) start with the harness; servers a
    /// workspace file declares start only once the workspace is trusted (spec 3.1 §7.7,
    /// I3).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let servers = McpServerManager::from_json_file(".mcp.json")?.all_configs().await;
    /// let harness = HarnessAgent::builder("harness")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .mcp_servers(crate::mcp::McpServerSet::workspace(servers))
    ///     .build()?;
    /// ```
    #[cfg(feature = "mcp")]
    pub fn mcp_servers(mut self, servers: crate::mcp::McpServerSet) -> Self {
        self.mcp_servers.push(servers);
        self
    }

    /// The model (required).
    pub fn model(mut self, model: Arc<dyn Llm>) -> Self {
        self.model = Some(model);
        self
    }

    /// The workspace (required). `bash` is offered only when the workspace allows it.
    pub fn workspace(mut self, workspace: Workspace) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// The agent description.
    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    /// Replaces the base prompt (default [`HARNESS_SYSTEM_PROMPT`]).
    pub fn base_prompt(mut self, text: impl Into<Cow<'static, str>>) -> Self {
        self.base_prompt = text.into();
        self
    }

    /// Operator instruction text, placed after the base prompt. It is not
    /// template-expanded.
    pub fn instruction(mut self, text: impl Into<String>) -> Self {
        self.instruction = Some(text.into());
        self
    }

    /// The surface the person uses: its commands, the kernel and how each pack turns on,
    /// for the harness's manifest (spec 3.1 §7.25). Without it the manifest reports
    /// [`SurfaceManifest::library`](crate::SurfaceManifest::library).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{CommandEntry, HarnessAgent, InstructionsConfig, SurfaceManifest};
    /// use adk_model::{ScriptedLlm, ScriptedResponse};
    /// use std::sync::Arc;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let surface = SurfaceManifest::new("my-app", "1.0.0")
    ///     .command(CommandEntry::slash("approvals", "Calls waiting for a decision"));
    /// let harness = HarnessAgent::builder("assistant")
    ///     .model(Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("ok")])))
    ///     .workspace(adk_devtools::Workspace::new(dir.path()))
    ///     .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
    ///     .surface(surface)
    ///     .build()?;
    /// assert!(harness.manifest().render().contains("/approvals"));
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    pub fn surface(mut self, surface: crate::manifest::SurfaceManifest) -> Self {
        self.surface = Some(surface);
        self
    }

    /// A page `harness_help` returns for topic `name` (spec 3.1 §7.25), such as a page of
    /// the harness documentation a surface ships.
    pub fn help_page(mut self, name: impl Into<String>, text: impl Into<String>) -> Self {
        self.help_pages.insert(name.into(), text.into());
        self
    }

    /// Where instruction files come from (spec 3.1 §7.7).
    pub fn project_instructions(mut self, config: InstructionsConfig) -> Self {
        self.instructions = config;
        self
    }

    /// Where workspace trust decisions are kept (default in memory, so a workspace starts
    /// untrusted unless `require_trust` is off).
    pub fn workspace_trust(mut self, store: Arc<dyn WorkspaceTrustStore>) -> Self {
        self.trust_store = Some(store);
        self
    }

    /// The mode of sessions with no mode record (default [`HarnessMode::Ask`]).
    pub fn mode(mut self, mode: HarnessMode) -> Self {
        self.mode = mode;
        self
    }

    /// Where session modes are kept (default in memory).
    pub fn mode_store(mut self, store: Arc<dyn ModeStore>) -> Self {
        self.mode_store = Some(store);
        self
    }

    /// Where standing rules are kept (default in memory).
    pub fn standing_rules(mut self, store: Arc<dyn StandingRuleStore>) -> Self {
        self.standing_rules = Some(store);
        self
    }

    /// Where the shell ledger is kept (default in memory).
    pub fn shell_ledger(mut self, ledger: Arc<dyn ShellLedger>) -> Self {
        self.shell_ledger = Some(ledger);
        self
    }

    /// The limits of background shells (spec 3.1 §7.29); default
    /// [`BackgroundShellConfig::unattended`](crate::BackgroundShellConfig::unattended),
    /// and [`attended`](crate::BackgroundShellConfig::attended) for a REPL a person watches.
    pub fn background_shells(mut self, config: crate::shell::BackgroundShellConfig) -> Self {
        self.background_shells = config;
        self
    }

    /// Where each background shell's process is recorded (default in memory), so the next
    /// start reaps what a crash left running (spec 3.1 §7.29).
    pub fn shell_pid_registry(mut self, registry: Arc<dyn crate::shell::ShellPidRegistry>) -> Self {
        self.shell_pids = Some(registry);
        self
    }

    /// Runs background shells on `shells`, which every harness of a process can share, so a
    /// harness rebuilt for the same session keeps its shells (spec 3.1 §7.29). Its limits
    /// and pid registry replace [`background_shells`](Self::background_shells) and
    /// [`shell_pid_registry`](Self::shell_pid_registry).
    pub fn shell_supervisor(mut self, shells: Arc<crate::shell::ShellSupervisor>) -> Self {
        self.shell_supervisor = Some(shells);
        self
    }

    /// Where the file tools' read records are kept (default in memory): the sha256 each
    /// session's agent last read or wrote per file, which `edit_file` takes as its
    /// precondition (spec 3.1 §7.26, §10 D7). A durable store lets a held edit approved
    /// from another process run (§7.33 fix 5).
    pub fn read_records(mut self, records: Arc<dyn adk_devtools::ReadLedger>) -> Self {
        self.read_records = Some(records);
        self
    }

    /// An operator policy, composed with the harness policy; the stricter decision wins.
    pub fn operator_policy(mut self, policy: Arc<dyn ToolPolicy>) -> Self {
        self.operator_policy = Some(policy);
        self
    }

    /// Adds `policy` to the operator policy, the stricter decision winning, so a later
    /// [`operator_policy`](Self::operator_policy) or addition keeps it (spec 3.2 §7.7, K-I6).
    pub fn add_operator_policy(mut self, policy: Arc<dyn ToolPolicy>) -> Self {
        self.operator_policy = Some(match self.operator_policy.take() {
            Some(existing) => Arc::new(adk_core::StrictestPolicy::new(vec![existing, policy])),
            None => policy,
        });
        self
    }

    /// Builds the harness of `partition` in `partitions`, the partition table its kernel
    /// shares (spec 3.2 §7.8): the partition's mode as the harness mode, its class and
    /// tenant on every agent's principal, its `memory_scope` as the project of every
    /// long-term fact — preferences included — and of a working-notes root of its own, and
    /// its `PartitionPolicy` composed with the operator policy, so a request reaching
    /// another partition is denied unless the coordinator makes it, and then needs a
    /// non-waivable approval (K-I15). The harness gets `partition_memory_search` and
    /// `partition_memory_remember`, the crossings into other partitions' facts.
    ///
    /// Set the mode with this call, not before: it replaces the builder's mode.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let partitions = adk_kernel::Partitions::new(store);
    /// let work = HarnessAgent::builder("harness")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .long_term_memory(LongTermMemoryConfig::default())
    ///     .fact_store(shared_facts.clone())
    ///     .partition(adk_kernel::Partition::new("work", "work"), partitions.clone())
    ///     .build()?;
    /// ```
    #[cfg(feature = "kernel")]
    pub fn partition(
        mut self,
        partition: adk_kernel::Partition,
        partitions: adk_kernel::Partitions,
    ) -> Self {
        self.mode = partition.default_mode.into();
        #[cfg(feature = "memory")]
        {
            self.long_term.project = Some(partition.memory_scope.clone());
        }
        self.partition.partition = Some((partition, partitions));
        match self.partition.drivers.take() {
            Some(drivers) => self.partition_drivers(&drivers),
            None => self,
        }
    }

    /// Stamps every session this harness runs with `target`, its kernel target, on its first
    /// run, and refuses a session another agent stamped (spec 3.3 §5.7 "Threads"):
    /// [`AGENT_STATE_KEY`](crate::AGENT_STATE_KEY). `adk-rust up` stamps the primary agent's
    /// sessions `adk-rust`; an added agent stamps its own id.
    ///
    /// # Example
    ///
    /// ```rust
    /// let builder = adk_harness::HarnessAgent::builder("adk-rust").session_stamp("adk-rust");
    /// # let _ = builder;
    /// ```
    #[cfg(feature = "kernel")]
    pub fn session_stamp(mut self, target: impl Into<String>) -> Self {
        self.session_stamp = Some(target.into());
        self
    }

    /// Gives this harness `propose_agent`, suggesting agents to the agent table of the kernel
    /// `handle` is bound to (spec 3.3 §5.7, §10 H3). `adk-rust up` binds it on the primary
    /// agent only; an added agent never has the tool. Needs feature `web`, whose screening the
    /// suggestion's instructions pass.
    ///
    /// # Example
    ///
    /// ```rust
    /// let handle = adk_kernel::agents::AgentsHandle::new();
    /// let builder = adk_harness::HarnessAgent::builder("adk-rust").agents_handle(handle);
    /// # let _ = builder;
    /// ```
    #[cfg(all(feature = "kernel", feature = "web"))]
    pub fn agents_handle(mut self, handle: adk_kernel::agents::AgentsHandle) -> Self {
        self.agent_proposer = Some(Arc::new(handle));
        self
    }

    /// The harness of the agent a person added (spec 3.3 §5.7, H3): see
    /// [`added_agent`](Self::added_agent).
    #[cfg(feature = "kernel")]
    pub(crate) fn lower_added(mut self, added: crate::agents::AddedAgent) -> Self {
        self.name = added.id.clone();
        self.session_stamp = Some(added.id.clone());
        if !added.description.is_empty() {
            self.description = Some(added.description.clone());
        }
        if !added.instructions.is_empty() {
            self.instruction = Some(added.instructions.clone());
        }
        self.mode = added.mode_in(self.mode);
        #[cfg(feature = "team")]
        self.members.clear();
        self.default_members = false;
        self.partition.coordinator = false;
        self.goal_store = None;
        #[cfg(feature = "graph")]
        {
            self.goal_engine = None;
        }
        #[cfg(feature = "web")]
        {
            self.agent_proposer = None;
        }
        let read_only = added.uses.files != adk_kernel::agents::FileAccess::ReadWrite;
        if let Some(workspace) = self.workspace.take() {
            let root = match &added.uses.folder {
                Some(folder) => workspace.root().join(folder),
                None => workspace.root().to_path_buf(),
            };
            self.workspace = Some(
                Workspace::new(root)
                    .writable(workspace.is_writable() && !read_only)
                    .allow_bash(workspace.bash_allowed())
                    .bash_timeout(workspace.bash_timeout_value())
                    .max_output_bytes(workspace.max_output())
                    .inherit_env(workspace.inherits_env()),
            );
        }
        match &added.uses.web {
            adk_kernel::agents::WebAccess::None => {
                self.web = None;
                self.search_provider = None;
                self.provider_web_tools = Some(crate::web::ProviderWebTools::default());
            }
            adk_kernel::agents::WebAccess::Listed { hosts } => {
                let web = self.web.take().unwrap_or_default();
                self.web = Some(crate::web::WebConfig { allowed_domains: hosts.clone(), ..web });
                self.provider_web_tools = Some(crate::web::ProviderWebTools::default());
            }
            adk_kernel::agents::WebAccess::Any => {}
        }
        if added.uses.shell == adk_kernel::agents::ShellAccess::Sandboxed {
            self.isolated_shell_only = true;
        }
        self.added = Some(added);
        self
    }

    /// Makes this partition's harness the coordinator, the one agent that may reach
    /// another partition, each crossing under a non-waivable approval (spec 3.2 §7.8, K-I15).
    #[cfg(feature = "kernel")]
    pub fn partition_coordinator(mut self) -> Self {
        self.partition.coordinator = true;
        self
    }

    /// Mounts `drivers` for this partition (spec 3.2 §7.7, §7.8), as
    /// [`kernel_drivers`](Self::kernel_drivers) with the partition's id and its own `drivers`
    /// list: the harness sees a driver only when its manifest lists the partition and the
    /// partition lists its id, so a driver of another partition is invisible to it (K-I15).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let work = HarnessAgent::builder("harness")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .partition(adk_kernel::Partition::new("work", "work").with_driver("calendar"), partitions)
    ///     .partition_drivers(&drivers)
    ///     .build()?;
    /// ```
    ///
    /// Call it before or after [`partition`](Self::partition); `build` fails when no
    /// partition is set.
    #[cfg(feature = "kernel")]
    pub fn partition_drivers(mut self, drivers: &adk_kernel::drivers::DriverSupervisor) -> Self {
        let Some((partition, _)) = &self.partition.partition else {
            // Mounted once `partition` names the partition.
            self.partition.drivers = Some(drivers.clone());
            return self;
        };
        let (id, ids) = (partition.id.clone(), partition.drivers.clone());
        self.kernel_drivers(drivers, &id, &ids)
    }

    /// Offers the coordinator `tool`, which reaches partition `to`'s `resource` named
    /// `name` — a driver tool of that partition, or a tool reading its sessions — as the
    /// crossing tool `<to>__<tool>` (spec 3.2 §7.8). `access` is
    /// [`Read`](adk_kernel::CrossingAccess::Read) only for a tool that only reads, by its
    /// effect class, never by its own hints; a read approved once holds for the rest of the
    /// run, anything else is approved one call at a time.
    #[cfg(feature = "kernel")]
    pub fn partition_crossing_tool(
        mut self,
        tool: Arc<dyn Tool>,
        to: impl Into<adk_kernel::PartitionId>,
        resource: adk_kernel::CrossingResource,
        access: adk_kernel::CrossingAccess,
        name: impl Into<String>,
    ) -> Self {
        self.partition.crossings.push(crate::partition::CrossingSpec {
            tool,
            to: to.into(),
            resource,
            access,
            name: name.into(),
        });
        self
    }

    /// Offers the coordinator the drivers of `partitions` as crossings (spec 3.2 §7.7, §7.8,
    /// K-I15): each tool a target partition's own driver list serves, named `<to>__<tool>`,
    /// read from the partition table at every listing. Every call is the coordinator's
    /// crossing under a non-waivable approval: a read of a tool the driver's manifest
    /// classes `read`, once approved, holds for the rest of the run, and any other class is
    /// approved one call at a time; no standing rule and no run-wide approval answers one.
    /// The tools are classed by their manifests under the kernel's driver policy, as
    /// [`partition_drivers`](Self::partition_drivers) classes the partition's own.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let home = HarnessAgent::builder("harness")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .partition(home_partition, partitions)
    ///     .partition_coordinator()
    ///     .partition_drivers(&drivers)
    ///     .partition_driver_crossings(&drivers, ["work"])
    ///     .build()?;
    /// ```
    ///
    /// `build` fails unless the harness is a partition's coordinator.
    #[cfg(feature = "kernel")]
    pub fn partition_driver_crossings(
        mut self,
        drivers: &adk_kernel::drivers::DriverSupervisor,
        partitions: impl IntoIterator<Item = impl Into<adk_kernel::PartitionId>>,
    ) -> Self {
        let targets: Vec<adk_kernel::PartitionId> =
            partitions.into_iter().map(Into::into).collect();
        self.partition.driver_crossings = Some((drivers.clone(), targets));
        self.tool_classes(Arc::new(crate::kernel::DriverClasses::new(drivers.clone())))
            .add_operator_policy(drivers.policy())
    }

    /// Classes the tools of publishers `classes` knows by their approved declarations,
    /// such as the kernel's drivers (spec 3.2 §7.7).
    pub fn tool_classes(mut self, classes: Arc<dyn crate::policy::OriginClasses>) -> Self {
        self.origin_classes.push(classes);
        self
    }

    /// Asks a person through `prompter` on interactive surfaces.
    pub fn prompter(mut self, prompter: Arc<dyn ApprovalPrompter>) -> Self {
        self.prompter = Some(prompter);
        self
    }

    /// Uses `handler` as the confirmation handler instead of the prompting one.
    pub fn confirmation_handler(mut self, handler: Arc<dyn ToolConfirmationHandler>) -> Self {
        self.confirmation_handler = Some(handler);
        self
    }

    /// Where held calls and decisions are kept (default in memory).
    pub fn approval_store(mut self, store: Arc<dyn ApprovalStore>) -> Self {
        self.approval_store = Some(store);
        self
    }

    /// Where `bash`, `write_file`, `edit_file` and other non-idempotent calls are recorded
    /// before they execute, so a call re-issued after a crash, retry or resume is answered
    /// from its record instead of running twice (spec 3.1 §7.12, §9.2 I37).
    ///
    /// The default keeps records in memory, which covers retries and resumes within one
    /// process; pass a durable ledger, such as one of `adk-session`'s, on the backend the
    /// sessions and approvals use so the guarantee survives process loss. A run that already
    /// carries a ledger, for example from `RunnerConfigBuilder::action_ledger`, keeps storing
    /// in it.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::InMemoryActionLedger;
    /// use std::sync::Arc;
    ///
    /// let builder = adk_harness::HarnessAgent::builder("harness")
    ///     .action_ledger(Arc::new(InMemoryActionLedger::new()));
    /// # let _ = builder;
    /// ```
    pub fn action_ledger(mut self, ledger: Arc<dyn ActionLedger>) -> Self {
        self.action_ledger = Some(ledger);
        self
    }

    /// Puts the harness's `propose_followup` items on the agenda of the kernel `handle` is
    /// bound to (spec 3.2 §7.5, §7.13): a kernel built without `Harness::kernel`, such as a
    /// tenant's in the managed form (`adk_kernel::managed::TenantKernel::agenda_handle`).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let agenda = adk_kernel::AgendaHandle::new();
    /// let resolver = HarnessResolver::new("/srv/workspaces")
    ///     .configure({ let agenda = agenda.clone(); move |b, _| b.agenda_handle(agenda.clone()) });
    /// ```
    #[cfg(feature = "kernel")]
    pub fn agenda_handle(mut self, handle: adk_kernel::AgendaHandle) -> Self {
        self.kernel_agenda = handle;
        self
    }

    /// The approval inbox held calls are submitted to.
    ///
    /// With feature `kernel`, the harness keeps a wrapper that also reports each request
    /// reaching its outcome to the kernel `Harness::kernel` builds (spec 3.2 §7.4).
    pub fn approval_service(mut self, service: Arc<dyn ApprovalService>) -> Self {
        #[cfg(feature = "kernel")]
        let service = crate::kernel::NotifyingApprovals::wrap(service, &self.kernel_approvals);
        self.approval_service = Some(service);
        self
    }

    /// How long a person may take to decide one call (default 300 s); a call nobody decides
    /// in time waits in the approval store for a later decision (spec 3.3 §7.3).
    pub fn confirmation_timeout(mut self, timeout: Duration) -> Self {
        self.confirmation_timeout = timeout;
        self
    }

    /// Where `bash` runs (default host, spec 3.1 §7.9).
    ///
    /// Unless [`allow_unsandboxed_retry`](Self::allow_unsandboxed_retry) was called, the
    /// unsandboxed retry is on for the host and the OS sandbox and off for a container.
    pub fn shell(mut self, isolation: ShellIsolation) -> Self {
        #[cfg(feature = "docker")]
        let container = matches!(isolation, ShellIsolation::Container(_));
        #[cfg(not(feature = "docker"))]
        let container = false;
        if !self.retry_explicit {
            self.shell.allow_unsandboxed_retry = !container;
        }
        self.shell.isolation = isolation;
        self
    }

    /// Fails the build, and each call, when the requested sandbox is unavailable.
    pub fn fail_if_unavailable(mut self) -> Self {
        self.shell.fail_if_unavailable = true;
        self
    }

    /// Applies `.adk/harness.toml` (spec 3.1 §8.3), read with [`HarnessConfig::discover`].
    ///
    /// The file may only tighten (spec 3.1 §9.2 I3): its mode applies when it is stricter
    /// than the mode set so far, `sandbox = true` runs `bash` in the OS sandbox, and
    /// `fail_if_unavailable = true` refuses a build without it. What the file asked for and
    /// the harness ignored is logged as a warning.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{HarnessAgent, HarnessConfig, HarnessMode, InstructionsConfig};
    /// use adk_model::{ScriptedLlm, ScriptedResponse};
    /// use std::sync::Arc;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let config = HarnessConfig::from_toml("mode = \"plan\"")?;
    /// let harness = HarnessAgent::builder("harness")
    ///     .model(Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("done")])))
    ///     .workspace(adk_devtools::Workspace::new(dir.path()))
    ///     .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
    ///     .config(config)
    ///     .build()?;
    /// assert_eq!(harness.info().default_mode, HarnessMode::Plan);
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    ///
    /// [`HarnessConfig::discover`]: crate::HarnessConfig::discover
    pub fn config(mut self, config: crate::config::HarnessConfig) -> Self {
        if let Some(mode) = config.mode {
            if mode.is_stricter_than(self.mode) {
                self.mode = mode;
            } else if mode != self.mode {
                tracing::warn!(
                    config.mode = %mode,
                    harness.mode = %self.mode,
                    "harness.toml mode ignored: it is looser than the harness's, and workspace files may only tighten"
                );
            }
        }
        if config.sandbox {
            #[cfg(feature = "sandbox")]
            if matches!(self.shell.isolation, ShellIsolation::Host) {
                self.shell.isolation = ShellIsolation::OsSandbox(Default::default());
            }
            #[cfg(not(feature = "sandbox"))]
            self.unimplemented.push(format!(
                "the OS sandbox {} asks for: build adk-harness with feature `sandbox` (spec 3.1 §9.2 I24)",
                crate::config::HARNESS_CONFIG_FILE
            ));
        }
        if config.fail_if_unavailable {
            self.shell.fail_if_unavailable = true;
        }
        for note in &config.notes {
            tracing::warn!(note = %note, "harness.toml setting ignored");
        }
        self
    }

    /// Runs sandboxed commands without approval in Ask (spec 3.1 §7.9, §9.2 I26).
    ///
    /// Turns on only under an active OS sandbox whose enforcer keeps protected paths
    /// read-only (adk-sandbox S3, checked by a conformance probe) or a container shell that
    /// mounts them read-only, with the network denied and every root-level protected path
    /// present; otherwise `build()` fails with `harness.auto_allow_sandboxed_unavailable`
    /// naming the reason. No effect on the host or a degraded shell.
    pub fn auto_allow_sandboxed(mut self) -> Self {
        self.shell.auto_allow_sandboxed = true;
        self
    }

    /// Offers an unsandboxed retry of a failed sandboxed command (default on for the host
    /// and the OS sandbox, off for a container).
    pub fn allow_unsandboxed_retry(mut self, yes: bool) -> Self {
        self.shell.allow_unsandboxed_retry = yes;
        self.retry_explicit = true;
        self
    }

    /// Runs host commands without approval in Auto.
    pub fn allow_host_shell_in_auto(mut self) -> Self {
        self.shell.allow_host_shell_in_auto = true;
        self
    }

    /// Leaves `bash` out.
    pub fn without_shell(mut self) -> Self {
        self.shell_enabled = false;
        self
    }

    /// Applies `preset` (spec 3.1 §7.19, spec 3.3 §10 H1): its prompt, mode, members and
    /// shell, replacing what an earlier preset set. Calls after it override what it sets,
    /// except the personal preset's shell: no later call gives it a host shell or an
    /// unsandboxed retry on the host.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{HarnessAgent, InstructionsConfig, Preset};
    /// use adk_model::{ScriptedLlm, ScriptedResponse};
    /// use std::sync::Arc;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let harness = HarnessAgent::builder("assistant")
    ///     .preset(Preset::Personal)
    ///     .model(Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("ok")])))
    ///     .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
    ///     .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
    ///     .build()?;
    /// assert!(harness.info().members.is_empty());
    /// assert!(!harness.info().tools.contains(&"bash".to_string()), "no host shell");
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    pub fn preset(self, preset: crate::Preset) -> Self {
        preset.apply(self)
    }

    /// Offers `bash` only when it runs in the OS sandbox or a container, with no unsandboxed
    /// retry (spec 3.3 §10 H1).
    pub(crate) fn isolated_shell_only(mut self, yes: bool) -> Self {
        self.isolated_shell_only = yes;
        self
    }

    /// Leaves `bash` out for as long as the preset that asked holds (spec 3.5 §10 X10).
    #[cfg(feature = "website")]
    pub(crate) fn preset_without_shell(mut self) -> Self {
        self.preset_without_shell = true;
        self
    }

    /// The website preset's mode banner and no `exit_plan_mode`, for as long as the preset
    /// holds (spec 3.5 §7.11, A-I13).
    #[cfg(feature = "website")]
    pub(crate) fn website_preset(mut self) -> Self {
        self.website_preset = true;
        self
    }

    /// Offers the agent no tool of the harness's own beyond `tools`: no workspace, memory,
    /// plan or web tool, and no harness toolset (spec 3.5 A-I2).
    #[cfg(feature = "website")]
    pub(crate) fn public_only(mut self, tools: BTreeSet<String>) -> Self {
        self.public_only = Some(tools);
        self
    }

    /// Undoes what a preset set that another preset leaves alone (spec 3.3 §10 H1).
    pub(crate) fn clear_preset(mut self) -> Self {
        if self.description.as_deref() == Some(crate::personal::PERSONAL_DESCRIPTION) {
            self.description = None;
        }
        #[cfg(feature = "website")]
        if self.description.as_deref() == Some(crate::website::WEBSITE_DESCRIPTION) {
            self.description = None;
        }
        self.preset_without_shell = false;
        self.website_preset = false;
        self.isolated_shell_only(false)
    }

    /// Trusts an MCP server's `readOnlyHint` for the External class (default `false`).
    pub fn trust_mcp_read_only_hints(mut self, yes: bool) -> Self {
        self.trust_mcp_read_only_hints = yes;
        self
    }

    /// Leaves `write_todos` and `update_todo` out.
    pub fn without_todos(mut self) -> Self {
        self.todos = false;
        self
    }

    /// Skills configuration (spec 3.1 §7.8).
    #[cfg(feature = "skills")]
    pub fn skills(mut self, config: crate::skills::SkillsConfig) -> Self {
        self.skills = config;
        self
    }

    /// Adds a team member (spec 3.1 §7.10). A member named like a default member
    /// replaces it.
    #[cfg(feature = "team")]
    pub fn member(mut self, spec: crate::team::MemberSpec) -> Self {
        self.members.push(spec);
        self
    }

    /// Discovers members in `registry` (spec 3.1 §7.10): every candidate no other member
    /// names becomes a member, resolved and authorized by the registry when the harness is
    /// built.
    #[cfg(feature = "team")]
    pub fn member_registry(mut self, registry: Arc<dyn adk_agent::TeamAgentRegistry>) -> Self {
        self.member_registry = Some(registry);
        self
    }

    /// The team policy (spec 3.1 §7.10): its budget, termination, failure policy and
    /// concurrency apply; the context is always isolated and the delegation depth 1
    /// ([`harness_team_policy`](crate::harness_team_policy)).
    #[cfg(feature = "team")]
    pub fn team_policy(mut self, policy: adk_agent::TeamPolicy) -> Self {
        self.team_policy = Some(policy);
        self
    }

    /// Leaves out the default members (`explore` and `general`).
    pub fn without_default_members(mut self) -> Self {
        self.default_members = false;
        self
    }

    /// The model of members without their own (model tiering).
    pub fn member_model(mut self, model: Arc<dyn Llm>) -> Self {
        self.member_model = Some(model);
        self
    }

    /// The goal engine (spec 3.1 §7.11): `Loop` by default, `Graph` by default when a
    /// plan checkpointer is configured.
    #[cfg(feature = "graph")]
    pub fn goal_engine(mut self, engine: crate::goal::GoalEngine) -> Self {
        self.goal_engine = Some(engine);
        self
    }

    /// Where session goals are kept (spec 3.1 §7.11; default in memory).
    pub fn goal_store(mut self, store: Arc<dyn crate::goal::GoalStore>) -> Self {
        self.goal_store = Some(store);
        self
    }

    /// Compiles approved plans with steps to a plan graph (spec 3.1 §7.11; default on with
    /// `graph`). Off, or without `graph`, steps are stored with the plan only.
    pub fn plan_graph(mut self, yes: bool) -> Self {
        self.plan_graph = yes;
        self
    }

    /// Snapshots the workspace before each user turn, plan step and rewind, so
    /// `Harness::rewind` can restore files, transcript and plan (spec 3.1 §7.27; off until
    /// set, on in adk-cli). Records and objects stay in memory unless
    /// [`snapshot_store`](Self::snapshot_store) and [`snapshot_objects`](Self::snapshot_objects)
    /// say otherwise.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let agent = HarnessAgent::builder("harness")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .snapshots(adk_harness::SnapshotConfig::default())
    ///     .build()?;
    /// ```
    pub fn snapshots(mut self, config: crate::snapshot::SnapshotConfig) -> Self {
        self.snapshots.config = Some(config);
        self
    }

    /// Where snapshot records are kept, such as `SqliteSnapshotStore` in `harness.db`.
    pub fn snapshot_store(mut self, store: Arc<dyn crate::snapshot::SnapshotStore>) -> Self {
        self.snapshots.store = Some(store);
        self
    }

    /// Where snapshot contents are kept, such as `FileSnapshotObjects::for_workspace` beside
    /// `harness.db`; never inside the workspace (I68).
    pub fn snapshot_objects(mut self, objects: Arc<dyn crate::snapshot::SnapshotObjects>) -> Self {
        self.snapshots.objects = Some(objects);
        self
    }

    /// File memory (spec 3.1 §7.15; off until set): adds `memory_read`, `memory_write`
    /// and `memory_delete`, and the memory index at the end of the instruction.
    #[cfg(feature = "memory")]
    pub fn memory(mut self, config: crate::memory::MemoryConfig) -> Self {
        self.memory = Some(config);
        self
    }

    /// Where plan graphs and the `Graph` goal engine are checkpointed (spec 3.1 §7.11,
    /// §13 Q5). Without this or [`plan_checkpoint_store`](Self::plan_checkpoint_store),
    /// plans live in memory and do not survive a restart; `adk-rust` checkpoints them in
    /// the backend that holds its sessions.
    #[cfg(feature = "graph")]
    pub fn plan_checkpointer(mut self, checkpointer: Arc<dyn adk_graph::Checkpointer>) -> Self {
        self.plan_checkpointer = Some(checkpointer);
        self
    }

    /// Checkpoints plan graphs and the `Graph` goal engine in the store `url` names
    /// (spec 3.1 §7.11, §13 Q5): `sqlite://`, `postgres://`, `redis://`, `mongodb://`,
    /// `neo4j://` or `firestore://`, opened by [`checkpointer_from_url`](crate::checkpointer_from_url).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example(model: std::sync::Arc<dyn adk_core::Llm>) -> adk_core::Result<()> {
    /// let agent = adk_harness::HarnessAgent::builder("harness")
    ///     .model(model)
    ///     .plan_checkpoint_store("postgres://harness@db/harness")
    ///     .await?
    ///     .build()?;
    /// # let _ = agent;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the errors of [`checkpointer_from_url`](crate::checkpointer_from_url): an
    /// unknown scheme, a scheme whose feature this build does not include, or a backend
    /// that cannot be opened.
    #[cfg(feature = "graph")]
    pub async fn plan_checkpoint_store(self, url: &str) -> Result<Self> {
        Ok(self.plan_checkpointer(crate::checkpoint::checkpointer_from_url(url).await?))
    }

    /// Long-term facts (spec 3.1 §7.15.2; off until set): recall into each user turn,
    /// `memory_search` and `memory_remember`, and session distillation.
    #[cfg(feature = "memory")]
    pub fn long_term_memory(mut self, config: crate::facts::LongTermMemoryConfig) -> Self {
        self.long_term.config = Some(config);
        self
    }

    /// Where long-term facts are kept (default in memory; SQLite in the CLI). Needs
    /// [`long_term_memory`](Self::long_term_memory).
    #[cfg(feature = "memory")]
    pub fn fact_store(mut self, store: Arc<dyn crate::facts::FactStore>) -> Self {
        self.long_term.store = Some(store);
        self
    }

    /// How recall finds candidate facts (default a [`LocalFactIndex`](crate::LocalFactIndex)
    /// over the fact store; a [`MemoryServiceIndex`](crate::MemoryServiceIndex) on servers).
    /// Needs [`long_term_memory`](Self::long_term_memory).
    #[cfg(feature = "memory")]
    pub fn fact_index(mut self, index: Arc<dyn crate::facts::FactIndex>) -> Self {
        self.long_term.index = Some(index);
        self
    }

    /// Embeds facts for the default [`LocalFactIndex`](crate::LocalFactIndex), which then
    /// recalls by cosine similarity instead of shared words (spec 3.1 §7.15.2). Needs
    /// [`long_term_memory`](Self::long_term_memory).
    #[cfg(feature = "memory")]
    pub fn memory_embedder(mut self, embedder: Arc<dyn adk_memory::EmbeddingProvider>) -> Self {
        self.long_term.embedder = Some(embedder);
        self
    }

    /// Resource limits for every run.
    pub fn budget(mut self, budget: RunBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// The ledger model calls reserve budget against.
    pub fn spend_ledger(mut self, ledger: Arc<dyn SpendLedger>) -> Self {
        self.spend_ledger = Some(ledger);
        self
    }

    /// Where every run's audit records go, in Block mode (spec 3.1 §7.14, §9.3). A
    /// runner's own `audit_log` takes precedence. With feature `auth` the default is an
    /// [`EphemeralAuditLog`](crate::EphemeralAuditLog); without it, runs record only to
    /// the trail the runner sets.
    pub fn audit_log(mut self, recorder: Arc<dyn adk_core::audit_log::AuditRecorder>) -> Self {
        self.audit_log = Some(recorder);
        self
    }

    /// Mints short-lived tokens for the subject a run acts for through `broker`, and hands
    /// them to `bash` for one call through a credential helper (spec 3.1 §7.14). The
    /// resources come from [`shell_credential`](Self::shell_credential), or else from the
    /// workspace's HTTPS git remotes.
    #[cfg(feature = "auth")]
    pub fn credentials(mut self, broker: Arc<dyn adk_core::CredentialBroker>) -> Self {
        self.identity.broker = Some(broker);
        self
    }

    /// Adds a resource the credential helper serves a token for (spec 3.1 §7.14).
    #[cfg(feature = "auth")]
    pub fn shell_credential(mut self, credential: crate::identity::ShellCredential) -> Self {
        self.identity.shell_credentials.push(credential);
        self
    }

    /// Requires a permission per tool class, checked when the tool executes
    /// (spec 3.1 §7.14).
    #[cfg(feature = "auth")]
    pub fn rbac(mut self, map: crate::identity::RbacMap) -> Self {
        self.identity.rbac = Some(map);
        self
    }

    /// Serves the harness to users the server authenticates: standing rules and Auto apply
    /// only to a run whose user was authenticated (`ReadonlyContext::is_authenticated`);
    /// any other run is decided as in Ask with no standing rule (spec 3.1 §9.2 I6).
    #[cfg(feature = "auth")]
    pub fn require_authenticated_identity(mut self) -> Self {
        self.identity.require_authenticated = true;
        self
    }

    /// Loop detection (spec 3.1 §7.13; off until set): before each model request the guard
    /// checks the invocation's tool calls, and a loop tightens the session to `ReadOnly` and
    /// ends the turn.
    #[cfg(feature = "observe")]
    pub fn loop_guard(mut self, guard: LoopGuard) -> Self {
        self.loop_guard = Some(guard);
        self
    }

    /// Adds a pack (spec 3.1 §7.16–§7.18, §7.24). `Pack::Payments` needs the services
    /// `pack_payments` carries, so on its own it fails at `build`; `Pack::Managed` marks the
    /// harness for the managed runtime (`info().packs` and the prompt name `managed`;
    /// `packs::managed::HarnessResolver` serves it) and fails at `build` without feature
    /// `managed`; `Pack::Browser` installs the browser pack over `BrowserConfig::default()`
    /// (Chrome, headless, WebDriver at `localhost:4444`) and fails at `build` without feature
    /// `browser`; `Pack::ComputerUse` needs the runtime `pack_computer_use` carries, so on its
    /// own it fails at `build`; `Pack::Voice` marks the harness for the voice surface
    /// (`packs::voice::VoiceSurface` runs it, spec 3.1 §7.17) and fails at `build` without
    /// feature `voice`.
    pub fn pack(mut self, pack: Pack) -> Self {
        match pack {
            Pack::Payments => self.payments_requested = true,
            Pack::Managed => self.managed_requested = true,
            Pack::Browser => self.browser_requested = true,
            Pack::ComputerUse => self.computer_use_requested = true,
            Pack::Voice => self.voice_requested = true,
        }
        self
    }

    /// The payments pack (spec 3.1 §7.16, feature `payments`): the `adk-payments` tools as
    /// class `Payment` — denied in ReadOnly and Plan, held for a non-waivable approval in
    /// Ask and Auto, never covered by a standing rule — under the run budget, the mandate's
    /// guardrails and the spend ledger, with the journal receipt linked from
    /// `harness.payment`. Every model request names the mandate and the remaining budget.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let harness = HarnessAgent::builder("harness")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .pack_payments(PaymentsPack::new(merchant.clone(), merchant, mandate))
    ///     .build()?;
    /// ```
    #[cfg(feature = "payments")]
    pub fn pack_payments(mut self, pack: crate::packs::payments::PaymentsPack) -> Self {
        self.payments = Some(pack);
        self.payments_requested = true;
        self
    }

    /// The browser pack (spec 3.1 §7.24, feature `browser`): `adk-browser`'s tools under the
    /// harness policy — observation as `Read`, navigation as `Web` decided by the URL's host
    /// under the harness's `WebConfig::allowed_domains`, actions as `External` held for
    /// approval in Ask and Auto and denied in ReadOnly and Plan — with the §7.20 URL checks
    /// before the browser moves and every result screened and delivered as untrusted text.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let harness = HarnessAgent::builder("harness")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .web(WebConfig { allowed_domains: vec!["docs.rs".into()], ..WebConfig::default() })
    ///     .pack_browser(BrowserPack::new(session))
    ///     .build()?;
    /// ```
    #[cfg(feature = "browser")]
    pub fn pack_browser(mut self, pack: crate::packs::browser::BrowserPack) -> Self {
        self.browser = Some(pack);
        self.browser_requested = true;
        self
    }

    /// The computer-use pack (spec 3.1 §7.24, feature `computer-use`): `computer_use_observe`
    /// and `computer_use_preview` as `Read`, and `computer_use_execute` as class
    /// `ComputerUse` — denied in ReadOnly and Plan, held for a non-waivable one-shot approval
    /// bound to the runtime's action and policy digests in Ask and Auto — through
    /// adk-computer-use's reference graph, whose approval interrupt the harness approval
    /// answers.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let harness = HarnessAgent::builder("harness")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .pack_computer_use(ComputerUsePack::new(runtime, authorizer))
    ///     .build()?;
    /// ```
    #[cfg(feature = "computer-use")]
    pub fn pack_computer_use(mut self, pack: crate::packs::computer_use::ComputerUsePack) -> Self {
        self.computer_use = Some(pack);
        self.computer_use_requested = true;
        self
    }

    /// Lets the harness's tool calls ask the person for a secret through `take_over`
    /// (spec 3.3 §8.2, §10 H2): the request reaches surfaces through the approval inbox, the
    /// typed value reaches the asking call only, and every occurrence of it in any call's
    /// result or error is replaced by
    /// [`TAKE_OVER_PLACEHOLDER`](crate::take_over::TAKE_OVER_PLACEHOLDER). Asking needs an
    /// [`approval_service`](Self::approval_service); the wait ends at the
    /// [`confirmation_timeout`](Self::confirmation_timeout), or shortly before the call's
    /// [`tool_timeout`](Self::tool_timeout) when that comes first. The shell never takes
    /// over, so passing `take_over` to [`credentials`](Self::credentials) gives `bash` no
    /// token.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let take_over = TakeOver::new();
    /// let harness = HarnessAgent::builder("assistant")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .tool(Arc::new(SignIn { take_over: take_over.clone() }))
    ///     .approval_service(inbox)
    ///     .take_over(take_over)
    ///     .build()?;
    /// ```
    pub fn take_over(mut self, take_over: crate::take_over::TakeOver) -> Self {
        self.take_over = Some(take_over);
        self
    }

    /// Adds a tool, governed as `External`.
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Adds a toolset, whose tools are governed as `External`.
    pub fn toolset(mut self, toolset: Arc<dyn Toolset>) -> Self {
        self.toolsets.push(toolset);
        self
    }

    /// Most model turns per run (default 100).
    pub fn max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = n;
        self
    }

    /// Tool timeout (default 300 s).
    pub fn tool_timeout(mut self, timeout: Duration) -> Self {
        self.tool_timeout = timeout;
        self
    }

    /// Most recent events loaded per run.
    pub fn history_max_events(mut self, n: usize) -> Self {
        self.history_max_events = Some(n);
        self
    }

    /// Events compaction for the harness's runner (default interval 10, overlap 4,
    /// summarized by the harness model).
    pub fn compaction(mut self, config: EventsCompactionConfig) -> Self {
        self.compaction = Some(config);
        self.compaction_enabled = true;
        self
    }

    /// Turns events compaction off.
    pub fn without_compaction(mut self) -> Self {
        self.compaction_enabled = false;
        self
    }

    /// The reasoning effort of each mode and model role (spec 3.1 §7.23); the defaults
    /// are the spec's table (plan high, members low).
    ///
    /// The policy applies per model call (§10 C4): each request of the harness model carries
    /// the level of the mode it reads, a member's request `members`, the distiller's
    /// `distiller` and an MCP sampling request `sampling`, as `GenerateContentConfig::effort`,
    /// which each `adk-model` provider maps to its own control. A mode switch changes the
    /// next call's effort without rebuilding a client.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{Effort, EffortPolicy, HarnessAgent};
    ///
    /// let builder = HarnessAgent::builder("coder")
    ///     .effort(EffortPolicy { plan: Effort::High, ask: Effort::Low, ..EffortPolicy::default() });
    /// # let _ = builder;
    /// ```
    pub fn effort(mut self, policy: crate::posture::EffortPolicy) -> Self {
        self.effort = policy;
        self
    }

    /// A model that caches its context explicitly (a `GeminiModel`): the runner gets it
    /// with `ContextCacheConfig::default()` (4,096 tokens, 600 s, 3 intervals) so the
    /// prompt prefix is cached (spec 3.1 §7.23).
    pub fn cache_capable(mut self, model: Arc<dyn adk_core::CacheCapable>) -> Self {
        self.cache_capable = Some(model);
        self
    }

    /// Turns explicit context caching off: the runner gets no cache even with
    /// [`cache_capable`](Self::cache_capable) (spec 3.1 §7.23).
    pub fn without_context_caching(mut self) -> Self {
        self.context_caching = false;
        self
    }

    /// Edits the underlying `LlmAgentBuilder` before it is built.
    ///
    /// This is the escape hatch, with two caveats: a `before_tool_callback` or an enhanced
    /// plugin added here can answer a call before authorization, skipping the policy,
    /// guardrails and confirmation; and `require_tool_confirmation*` forces a confirmation
    /// even when the policy allows the call, which defeats standing rules.
    pub fn customize_llm(
        mut self,
        f: impl FnOnce(LlmAgentBuilder) -> LlmAgentBuilder + Send + 'static,
    ) -> Self {
        self.customize = Some(Box::new(f));
        self
    }

    /// The hooks the operator and the user configured (spec 3.1 §7.22). The workspace's
    /// `.adk/harness.toml` hooks are added at build, after these, and run only under the
    /// workspace's content-keyed trust. Hooks observe tool events through the shell's
    /// launcher; they can deny, fail or annotate and never allow (spec 3.1 §9.2 I53).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_harness::hooks::{HookEvent, HookSet, HookSpec};
    ///
    /// let hooks = HookSet::new()
    ///     .with(HookSpec::new(HookEvent::PostEdit, "cargo fmt").matching("write_*"));
    /// let harness = HarnessAgent::builder("harness").model(model).workspace(ws).hooks(hooks).build()?;
    /// ```
    #[cfg(feature = "hooks")]
    pub fn hooks(mut self, hooks: crate::hooks::HookSet) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Runs no hooks, not even the workspace's (`--no-hooks`, spec 3.1 §8.1).
    #[cfg(feature = "hooks")]
    pub fn without_hooks(mut self) -> Self {
        self.hooks_enabled = false;
        self.hooks = None;
        self
    }

    pub(crate) fn with_last_todos(mut self, last: Arc<Mutex<TodoList>>) -> Self {
        self.last_todos = Some(last);
        self
    }

    /// Builds the harness (spec 3.1 §7.2).
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the model or workspace is missing, when an
    /// extra tool collides with a harness tool or is a provider shell built-in, when a
    /// feature not yet implemented is requested, and `harness.sandbox_unavailable` when
    /// the requested sandbox is unavailable under `fail_if_unavailable`.
    pub fn build(mut self) -> Result<HarnessAgent> {
        // The website preset runs without a shell, whatever the workspace allows (X10).
        self.shell_enabled &= !self.preset_without_shell;
        let public_only = self.public_only.take();
        let model = self.model.ok_or_else(|| AdkError::config("HarnessAgent requires a model"))?;
        let mut workspace =
            self.workspace.ok_or_else(|| AdkError::config("HarnessAgent requires a workspace"))?;
        if let Some(records) = self.read_records.take() {
            workspace = workspace.with_read_ledger(records);
        }
        let unimplemented = self.unimplemented;
        if let Some(first) = unimplemented.first() {
            return Err(AdkError::config(format!("not yet implemented: {first}")));
        }
        if self.payments_requested {
            #[cfg(feature = "payments")]
            let configured = self.payments.is_some();
            #[cfg(not(feature = "payments"))]
            let configured = false;
            if !configured {
                return Err(AdkError::config(
                    "the payments pack needs a merchant checkout service, a transaction store \
                     and a mandate, which only the library API takes: \
                     HarnessAgentBuilder::pack_payments(PaymentsPack) with adk-harness feature \
                     `payments` (spec 3.1 §7.16)",
                ));
            }
        }
        if self.managed_requested && cfg!(not(feature = "managed")) {
            return Err(AdkError::config(
                "the managed form needs adk-harness feature `managed`: HarnessResolver builds a \
                 HarnessDef on the managed runtime and serves a harness marked Pack::Managed \
                 (spec 3.1 §7.18)",
            ));
        }
        // The browser pack (spec 3.1 §7.24): `Pack::Browser` alone is the default session.
        #[cfg(feature = "browser")]
        let browser = match (self.browser, self.browser_requested) {
            (Some(pack), _) => Some(pack),
            (None, true) => Some(crate::packs::browser::BrowserPack::default()),
            (None, false) => None,
        };
        #[cfg(not(feature = "browser"))]
        if self.browser_requested {
            return Err(AdkError::config(
                "the browser pack needs adk-harness feature `browser`: Pack::Browser or \
                 HarnessAgentBuilder::pack_browser(BrowserPack) (spec 3.1 §7.24)",
            ));
        }
        // The computer-use pack (spec 3.1 §7.24): a runtime is a connection to a server, so
        // `Pack::ComputerUse` alone cannot build.
        #[cfg(feature = "computer-use")]
        let computer_use = self.computer_use;
        if self.computer_use_requested {
            #[cfg(feature = "computer-use")]
            let configured = computer_use.is_some();
            #[cfg(not(feature = "computer-use"))]
            let configured = false;
            if !configured {
                return Err(AdkError::config(
                    "the computer-use pack needs a ComputerUseRuntime and a ScopeAuthorizer, \
                     which only the library API takes: \
                     HarnessAgentBuilder::pack_computer_use(ComputerUsePack) with adk-harness \
                     feature `computer-use` (spec 3.1 §7.24)",
                ));
            }
        }
        if self.voice_requested && cfg!(not(feature = "voice")) {
            return Err(AdkError::config(
                "the voice surface needs adk-harness feature `voice`: packs::voice::VoiceSurface \
                 runs a harness marked Pack::Voice as the agent of an IntegratedRealtimeRunner \
                 (spec 3.1 §7.17)",
            ));
        }
        let harness_names: HashSet<&str> = HARNESS_TOOL_NAMES.iter().copied().collect();
        for tool in &self.tools {
            if harness_names.contains(tool.name()) {
                return Err(AdkError::config(format!(
                    "tool '{}' collides with a harness tool of the same name",
                    tool.name()
                )));
            }
            #[cfg(feature = "payments")]
            if self.payments.is_some() && crate::packs::payments::is_payment_tool(tool.name()) {
                return Err(AdkError::config(format!(
                    "tool '{}' collides with a payments pack tool of the same name",
                    tool.name()
                )));
            }
            #[cfg(feature = "browser")]
            if browser.is_some() && crate::packs::browser::is_browser_tool(tool.name()) {
                return Err(AdkError::config(format!(
                    "tool '{}' collides with a browser pack tool of the same name (spec 3.1 §7.24)",
                    tool.name()
                )));
            }
            #[cfg(feature = "computer-use")]
            if computer_use.is_some()
                && crate::packs::computer_use::is_computer_use_tool(tool.name())
            {
                return Err(AdkError::config(format!(
                    "tool '{}' collides with a computer-use pack tool of the same name (spec 3.1 §7.24)",
                    tool.name()
                )));
            }
            if tool.is_builtin()
                && (tool.name().starts_with("bash") || tool.name().contains("shell"))
            {
                return Err(AdkError::config(format!(
                    "'{}' is a provider-executed shell; the harness runs commands only through its own bash (spec 3.1 §9.2 I27)",
                    tool.name()
                )));
            }
            // Every other builtin executes inside the provider, where no policy, ledger,
            // guardrail or audit record covers it (spec 3.1 §7.20 "Refused builtins").
            if tool.is_builtin() {
                return Err(AdkError::config(format!(
                    "'{}' is a provider-executed tool the harness does not manage; search and URL \
                     context are enabled through HarnessAgentBuilder::provider_web_tools, computer \
                     use through its pack, and the rest have no governed form (spec 3.1 §7.20)",
                    tool.name()
                )));
            }
        }
        if self.search_provider.is_some()
            && self.provider_web_tools.is_some_and(|tools| tools.anthropic_web_search)
        {
            return Err(AdkError::config(
                "the governed web_search and Anthropic's provider web_search share one tool name; \
                 set a search provider or enable anthropic_web_search, not both (spec 3.1 §7.20)",
            ));
        }

        // 1. Shell runtime.
        let shell = if self.shell_enabled {
            resolve_isolation(&self.shell, workspace.root())?
        } else {
            resolve_isolation(&ShellConfig::default(), workspace.root())?
        };
        let status = shell.status.clone();

        // 2. Instructions and the class registry.
        let instructions = ProjectInstructions::load(workspace.root(), &self.instructions)?;
        let instruction_state = Arc::new(InstructionState {
            instructions,
            trust_store: self
                .trust_store
                .unwrap_or_else(|| Arc::new(InMemoryWorkspaceTrustStore::new())),
            require_trust: self.instructions.require_trust,
        });
        // Hooks (spec 3.1 §7.22): operator and user hooks, then the workspace's, through the
        // shell's launcher and under the workspace's content-keyed trust.
        #[cfg(feature = "hooks")]
        let hook_runner = build_hook_runner(
            self.hooks_enabled,
            self.hooks,
            &workspace,
            &shell,
            &status,
            &instruction_state,
        )?;
        #[cfg_attr(not(feature = "team"), allow(unused_mut))]
        let mut registry = ToolClassRegistry::harness_defaults(&self.name).with_web_allowlist(
            self.web.as_ref().map(|web| web.allowed_domains.clone()).unwrap_or_default(),
        );
        for classes in &self.origin_classes {
            registry = registry.with_origin_classes(classes.clone());
        }
        // The payments pack (spec 3.1 §7.16): every payment tool name is class Payment,
        // declared or not, so a lookalike attached as an extra never classes as External.
        #[cfg(feature = "payments")]
        let payments = self.payments.map(|pack| pack.into_runtime(&self.name));
        #[cfg(feature = "payments")]
        if payments.is_some() {
            for name in crate::packs::payments::PAYMENT_TOOL_NAMES {
                registry.register(*name, ToolKind::Payment);
            }
        }
        // The browser pack (spec 3.1 §7.24): every browser tool name is classed, attached
        // or not, so a lookalike never classes as a plain External.
        #[cfg(feature = "browser")]
        if browser.is_some() {
            for (name, kind) in crate::packs::browser::browser_classes() {
                registry.register(name, kind);
            }
        }
        // The computer-use pack (spec 3.1 §7.24): observation and preview read; execution is
        // the never-waivable desktop action.
        #[cfg(feature = "computer-use")]
        if computer_use.is_some() {
            registry.register(crate::packs::computer_use::COMPUTER_USE_OBSERVE, ToolKind::Read);
            registry.register(crate::packs::computer_use::COMPUTER_USE_PREVIEW, ToolKind::Read);
            registry
                .register(crate::packs::computer_use::COMPUTER_USE_EXECUTE, ToolKind::ComputerUse);
        }
        // The partition's policy and crossing tools (spec 3.2 §7.8).
        #[cfg(feature = "kernel")]
        let partition = self.partition.wire(&self.name, &mut registry)?;
        // Crossing tools listed at request time: no run-wide approval names one (K-I15).
        #[cfg(feature = "kernel")]
        let non_waivable_prefixes: Vec<String> =
            partition.as_ref().map(|wired| wired.prefixes.clone()).unwrap_or_default();
        #[cfg(not(feature = "kernel"))]
        let non_waivable_prefixes: Vec<String> = Vec::new();
        // 4 (resolved here: member names register as harness classes, spec 3.1 §7.4).
        #[cfg(feature = "team")]
        let team_plan = {
            let mut reserved: HashSet<String> =
                HARNESS_TOOL_NAMES.iter().map(|name| (*name).to_string()).collect();
            reserved.extend(self.tools.iter().map(|tool| tool.name().to_string()));
            #[cfg(feature = "payments")]
            if payments.is_some() {
                reserved.extend(
                    crate::packs::payments::PAYMENT_TOOL_NAMES
                        .iter()
                        .map(|name| (*name).to_string()),
                );
            }
            #[cfg(feature = "browser")]
            if browser.is_some() {
                reserved.extend(
                    crate::packs::browser::browser_classes().map(|(name, _)| name.to_string()),
                );
            }
            #[cfg(feature = "computer-use")]
            if computer_use.is_some() {
                reserved.extend(
                    crate::packs::computer_use::COMPUTER_USE_TOOL_NAMES
                        .iter()
                        .map(|name| (*name).to_string()),
                );
            }
            let plan = crate::team::TeamPlan::prepare(
                &self.name,
                self.members,
                self.default_members,
                self.member_registry,
                self.team_policy,
                &reserved,
            )?;
            if let Some(plan) = &plan {
                plan.register_classes(&mut registry);
            }
            plan
        };

        let mode_store = self.mode_store.unwrap_or_else(|| Arc::new(InMemoryModeStore::new()));
        let rules =
            self.standing_rules.unwrap_or_else(|| Arc::new(InMemoryStandingRuleStore::new()));
        // An added agent applies only the rules pinned to its own id (spec 3.3 S-I19).
        #[cfg(feature = "kernel")]
        let rules: Arc<dyn StandingRuleStore> = match &self.added {
            Some(added) => Arc::new(crate::agents::PinnedOnlyRules::new(rules, added.id.clone())),
            None => rules,
        };
        let ledger = self.shell_ledger.unwrap_or_else(|| Arc::new(InMemoryShellLedger::new()));
        let shells = crate::shell::supervisor(
            self.shell_supervisor.take(),
            self.background_shells.clone(),
            self.shell_pids.take(),
        );
        let approval_store = self
            .approval_store
            .or_else(|| self.approval_service.as_ref().and_then(|service| service.approval_store()))
            .unwrap_or_else(|| Arc::new(InMemoryApprovalStore::new()));

        // Filled once the harness is built, from the tools, classes and switches below
        // (spec 3.1 §7.25).
        let manifest_cell: Arc<OnceLock<crate::manifest::HarnessManifest>> = Arc::default();
        let mut tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(ReadFileTool::new(workspace.clone())),
            Arc::new(GlobTool::new(workspace.clone())),
            Arc::new(GrepTool::new(workspace.clone())),
            Arc::new(crate::tools::HarnessHelpTool::new(
                manifest_cell.clone(),
                std::mem::take(&mut self.help_pages),
            )),
        ];
        if workspace.is_writable() {
            tools.push(Arc::new(WriteFileTool::new(workspace.clone())));
            tools.push(Arc::new(EditFileTool::new(workspace.clone())));
            tools.push(Arc::new(adk_devtools::ApplyPatchTool::new(workspace.clone())));
        }
        // While sandboxed commands run unapproved, no host file tool overlaps one (I26).
        let file_gate = shell.auto_allow_sandboxed.then(crate::shell::FileToolGate::default);
        if let Some(gate) = &file_gate {
            tools = tools.into_iter().map(|tool| gate.wrap(tool)).collect();
        }
        // The container shell (spec 3.1 §7.9 "Container"): its containers close with the
        // toolset.
        // Shell credentials reach `bash` through a launcher layer, which the container shell
        // does not have; refusing beats commands that silently run without them.
        #[cfg(all(feature = "docker", feature = "auth"))]
        if shell.container.is_some() && !self.identity.shell_credentials.is_empty() {
            return Err(AdkError::config(
                "shell credentials cannot be delivered to the container shell yet; use the host \
                 shell or the OS sandbox with shell_credential (spec 3.1 §7.14)",
            ));
        }
        // The personal preset's shell never retries on the host (spec 3.3 §10 H1).
        let allow_unsandboxed_retry =
            self.shell.allow_unsandboxed_retry && !self.isolated_shell_only;
        #[cfg(feature = "docker")]
        let container_pool = match &shell.container {
            Some(config) if self.shell_enabled && workspace.bash_allowed() => {
                let bash = crate::shell::ContainerBashTool::new(
                    workspace.clone(),
                    config.clone(),
                    ledger.clone(),
                )
                .allow_unsandboxed_retry(allow_unsandboxed_retry)
                .with_file_gate(file_gate.clone())
                .with_outputs(shells.outputs().clone());
                let pool = bash.pool();
                tools.push(Arc::new(bash));
                Some(pool)
            }
            _ => None,
        };
        #[cfg(feature = "docker")]
        let container_shell = container_pool.is_some();
        #[cfg(not(feature = "docker"))]
        let container_shell = false;
        // The personal preset never offers a host shell (spec 3.3 §10 H1).
        let isolated_shell = matches!(status, IsolationStatus::OsSandbox { .. });
        let host_shell_refused = self.isolated_shell_only && !isolated_shell;
        if self.shell_enabled && workspace.bash_allowed() && !container_shell && !host_shell_refused
        {
            let bash = HarnessBashTool::new(
                workspace.clone(),
                status.clone(),
                self.shell.fail_if_unavailable,
                ledger.clone(),
            )
            .with_mode_store(mode_store.clone(), self.mode)
            .allow_unsandboxed_retry(allow_unsandboxed_retry)
            .with_file_gate(file_gate.clone())
            .with_shells(shells.clone());
            #[cfg(feature = "sandbox")]
            let bash = match shell.launcher.clone() {
                Some(launcher) => bash.with_sandbox(launcher),
                None => bash,
            };
            #[cfg(feature = "auth")]
            let bash = self.identity.with_credentials(bash, workspace.root());
            #[cfg(not(feature = "auth"))]
            let bash: Arc<dyn Tool> = Arc::new(bash);
            tools.push(bash);
            tools.push(Arc::new(crate::shell::ShellKillTool::new(shells.clone())));
        }
        if tools.iter().any(|tool| tool.name() == "bash") {
            tools.push(Arc::new(crate::shell::ShellOutputTool::new(shells.clone())));
        }
        if self.todos {
            match &self.last_todos {
                Some(last) => {
                    tools.push(Arc::new(WriteTodosTool::with_last(last.clone())));
                    tools.push(Arc::new(UpdateTodoTool::with_last(last.clone())));
                }
                None => {
                    tools.push(Arc::new(WriteTodosTool::new()));
                    tools.push(Arc::new(UpdateTodoTool::new()));
                }
            }
        }
        #[cfg(feature = "graph")]
        let plan_graphs = self.plan_graph.then(|| {
            Arc::new(crate::plan_graph::PlanGraphs::new(
                self.plan_checkpointer
                    .clone()
                    .unwrap_or_else(|| Arc::new(adk_graph::MemoryCheckpointer::new())),
                &self.name,
            ))
        });
        let snapshots = std::mem::take(&mut self.snapshots).build(&workspace)?;
        #[cfg(feature = "graph")]
        if let (Some(graphs), Some(snapshots)) = (&plan_graphs, &snapshots) {
            graphs.set_snapshots(snapshots.clone());
        }
        let plan_exit = ExitPlanModeTool::new(mode_store.clone(), self.mode);
        #[cfg(feature = "graph")]
        let plan_exit = match &plan_graphs {
            Some(graphs) => plan_exit.with_plan_graph(graphs.clone()),
            None => plan_exit,
        };
        // A website agent's mode is its jobs' autonomy, which only the owner raises (A-I13).
        if !self.website_preset {
            tools.push(Arc::new(plan_exit));
        }
        // Follow-ups go on the agenda of the kernel hosting the harness (spec 3.2 §7.5).
        #[cfg(feature = "kernel")]
        tools.push(Arc::new(crate::kernel::ProposeFollowupTool::new(
            self.kernel_agenda.clone(),
            mode_store.clone(),
            self.mode,
            partition.as_ref().map(|wired| wired.wiring.partition.id.clone()),
        )));
        // Agents are suggested only by the primary agent, never by an added one (spec 3.3 §5.7).
        #[cfg(all(feature = "kernel", feature = "web"))]
        let agent_limits = Arc::new(OnceLock::new());
        #[cfg(all(feature = "kernel", feature = "web"))]
        if let Some(proposer) = self.agent_proposer.clone().filter(|_| self.added.is_none()) {
            tools.push(Arc::new(crate::agents::ProposeAgentTool::new(
                proposer,
                mode_store.clone(),
                self.mode,
                agent_limits.clone(),
                partition
                    .as_ref()
                    .map(|wired| wired.wiring.partition.id.clone())
                    .unwrap_or_default(),
            )));
        }
        #[cfg(feature = "memory")]
        let memory_resolver = self.memory.clone().map(|config| {
            // A partition keeps working notes of its own (spec 3.2 K-I15).
            #[cfg(feature = "kernel")]
            let config = match &partition {
                Some(wired) => wired.wiring.scope_notes(config, workspace.root()),
                None => config,
            };
            let resolver =
                crate::tools::MemoryResolver::new(config, workspace.root().to_path_buf());
            tools.push(Arc::new(crate::tools::MemoryReadTool::new(resolver.clone())));
            tools.push(Arc::new(crate::tools::MemoryWriteTool::new(resolver.clone())));
            tools.push(Arc::new(crate::tools::MemoryDeleteTool::new(resolver.clone())));
            resolver
        });
        let harness_audit = crate::agent::harness_audit_trail(self.audit_log)?;
        // Under a kernel, every audit record also writes its content-free ledger row
        // (spec 3.2 §7.10).
        #[cfg(feature = "kernel")]
        let harness_audit =
            harness_audit.map(|trail| crate::kernel::tap_audit(trail, &self.kernel_ledger));
        #[cfg(feature = "memory")]
        let facts = {
            #[cfg(feature = "auth")]
            let require_authenticated = self.identity.require_authenticated;
            #[cfg(not(feature = "auth"))]
            let require_authenticated = false;
            self.long_term.build(
                &self.name,
                workspace.root(),
                require_authenticated,
                harness_audit.clone(),
                self.member_model.clone().unwrap_or_else(|| model.clone()),
                self.budget.clone(),
                (mode_store.clone(), self.mode),
                self.effort.distiller,
            )?
        };
        #[cfg(feature = "memory")]
        if let Some(facts) = &facts {
            tools.push(Arc::new(crate::tools::MemorySearchTool::new(facts.clone())));
            tools.push(Arc::new(crate::tools::MemoryRememberTool::new(facts.clone())));
        }
        // Crossings into other partitions' facts, drivers and sessions (spec 3.2 §7.8).
        #[cfg(all(feature = "kernel", feature = "memory"))]
        if let (Some(wired), Some(facts)) = (&partition, &facts) {
            tools.extend(wired.wiring.memory_tools(facts));
        }
        #[cfg(feature = "kernel")]
        if let Some(wired) = &partition {
            tools.extend(wired.crossings.iter().cloned());
        }
        // Governed web tools (spec 3.1 §7.20): `web_search` only with a provider.
        #[cfg(feature = "web")]
        if let Some(config) = self.web.clone() {
            let runtime = crate::web::WebRuntime::new(config, self.web_signer.clone());
            tools.push(Arc::new(crate::tools::WebFetchTool::new(runtime.clone())));
            if let Some(provider) = &self.search_provider {
                tools.push(Arc::new(crate::tools::WebSearchTool::new(runtime, provider.clone())));
            }
        }
        // MCP interaction (spec 3.1 §7.21): the bridges exist before the prompter they
        // answer through, which is bound once the handler is built.
        #[cfg(feature = "mcp")]
        let mcp = (!self.mcp_servers.is_empty()).then(|| {
            // Sampling runs at `sampling` (spec 3.1 §7.23), whichever model answers it.
            let at_sampling =
                |model: Arc<dyn Llm>| crate::posture::AtEffort::wrap(model, self.effort.sampling);
            let mut interaction = self.mcp_interaction.clone();
            interaction.sampling_model = interaction.sampling_model.map(at_sampling);
            Arc::new(crate::mcp::McpInteractionRuntime::new(
                interaction,
                self.mcp_servers.clone(),
                at_sampling(model.clone()),
                self.member_model.clone().map(at_sampling),
            ))
        });
        #[cfg(feature = "mcp")]
        if let Some(mcp) = &mcp
            && self.mcp_interaction.resources
        {
            tools.push(Arc::new(crate::tools::McpListResourcesTool::new(mcp.clone())));
            tools.push(Arc::new(crate::tools::McpReadResourceTool::new(mcp.clone())));
        }
        // The payment tools join the harness tools, so RBAC, hooks and the class registry
        // cover them like any other (spec 3.1 §7.16).
        #[cfg(feature = "payments")]
        if let Some(payments) = &payments {
            tools.extend(payments.tools.iter().cloned());
        }
        // The browser pack's tools join the harness tools under the harness's web
        // configuration (spec 3.1 §7.24).
        #[cfg(feature = "browser")]
        let browser = browser.map(|pack| pack.into_runtime(self.web.clone().unwrap_or_default()));
        #[cfg(feature = "browser")]
        if let Some(browser) = &browser {
            tools.extend(browser.tools.iter().cloned());
        }
        #[cfg(feature = "computer-use")]
        let computer_use = computer_use.map(|pack| pack.into_runtime());
        #[cfg(feature = "computer-use")]
        if let Some(computer_use) = &computer_use {
            tools.extend(computer_use.tools.iter().cloned());
        }
        // Take over (spec 3.3 §10 H2): every call runs in a scope a secret can reach, and
        // leaves it scrubbed before RBAC, hooks or members see the result.
        if let Some(take_over) = &self.take_over {
            take_over.bind(self.approval_service.clone(), self.confirmation_timeout);
            let tool_timeout = self.tool_timeout;
            tools = tools
                .into_iter()
                .map(|tool| match tool.name() {
                    // The shell runs model-written commands, so it never takes over.
                    "bash" => crate::take_over::ScopedTool::wrap_shell(tool, tool_timeout),
                    _ => crate::take_over::ScopedTool::wrap(tool, tool_timeout),
                })
                .collect();
        }
        // A public-partition agent is offered its partition's tools and nothing else, so no
        // workspace, memory or plan tool reaches a model that reads untrusted text (A-I2).
        if let Some(allowed) = &public_only {
            tools.retain(|tool| allowed.contains(tool.name()));
        }
        let tool_names: Vec<String> = tools.iter().map(|tool| tool.name().to_string()).collect();
        #[cfg(feature = "auth")]
        let (tools, identity_gate) = {
            let isolation = status.isolation();
            let tools = self.identity.protect(tools, &registry, &self.name, isolation);
            (
                tools,
                Arc::new(crate::identity::IdentityGate::new(self.identity.require_authenticated)),
            )
        };
        // Every tool, the members' included, runs the PostTool and PostEdit hooks after it
        // executes (spec 3.1 §7.22); the wrapper forwards the declaration, so origins hold.
        #[cfg(feature = "hooks")]
        let tools = match &hook_runner {
            Some(runner) => crate::hooks::HookedTool::wrap_all(
                tools,
                runner,
                &registry,
                &self.name,
                status.isolation(),
                (mode_store.clone(), self.mode),
            ),
            None => tools,
        };
        #[cfg(feature = "team")]
        let member_tools = tools.clone();
        // A purchase is the coordinator's call to put to the person; members never pay
        // (spec 3.1 §7.16, §9.2 I38).
        #[cfg(all(feature = "team", feature = "payments"))]
        let member_tools: Vec<Arc<dyn Tool>> = member_tools
            .into_iter()
            .filter(|tool| !crate::packs::payments::is_payment_tool(tool.name()))
            .collect();
        // A member keeps the browser's observation tools and never navigates or acts
        // (spec 3.1 §7.24 "Members", §9.2 I60).
        #[cfg(all(feature = "team", feature = "browser"))]
        let member_tools: Vec<Arc<dyn Tool>> = member_tools
            .into_iter()
            .filter(|tool| !crate::packs::browser::browser_member_excluded(tool.name()))
            .collect();
        // A desktop action is the coordinator's to put to the person (spec 3.1 §7.24).
        #[cfg(all(feature = "team", feature = "computer-use"))]
        let member_tools: Vec<Arc<dyn Tool>> = member_tools
            .into_iter()
            .filter(|tool| !crate::packs::computer_use::is_computer_use_tool(tool.name()))
            .collect();
        // Members get the same request observers as the coordinator (spec 3.1 §7.10, §7.13).
        #[cfg_attr(not(feature = "observe"), allow(unused_mut))]
        #[cfg(feature = "auth")]
        let mut observers: Vec<Arc<dyn crate::toolset::RequestObserver>> =
            vec![identity_gate.clone()];
        #[cfg_attr(not(feature = "observe"), allow(unused_mut))]
        #[cfg(not(feature = "auth"))]
        let mut observers: Vec<Arc<dyn crate::toolset::RequestObserver>> = Vec::new();
        #[cfg(feature = "observe")]
        let run_trails = Arc::new(crate::observe::RunTrails::default());
        #[cfg(feature = "observe")]
        if let Some(guard) = self.loop_guard {
            observers.push(Arc::new(crate::observe::LoopGuardObserver::new(
                guard,
                mode_store.clone(),
                self.mode,
                run_trails.clone(),
            )));
        }
        // Every request names the mandate and the remaining budget (spec 3.1 §7.16).
        #[cfg(feature = "payments")]
        if let Some(payments) = &payments {
            observers.push(payments.reminder.clone());
        }
        #[cfg_attr(not(feature = "memory"), allow(unused_mut))]
        let mut toolset = HarnessToolset::new(tools, mode_store.clone(), self.mode)
            .with_banner(if self.website_preset { website_banner } else { mode_banner })
            .with_observers(&observers)
            .with_effort(self.effort)
            .with_rules_digest(rules.clone())
            .with_shells(shells);
        // Under a kernel, each run's tool calls write their ledger rows (spec 3.2 §7.10).
        #[cfg(all(feature = "kernel", feature = "observe"))]
        run_trails.attach_ledger(self.kernel_ledger.clone(), &toolset.all_tools());
        #[cfg(feature = "docker")]
        if let Some(pool) = container_pool {
            toolset = toolset.with_containers(pool);
        }
        #[cfg(feature = "memory")]
        if let Some(facts) = facts {
            toolset = toolset.with_facts(facts);
        }
        let toolset = Arc::new(toolset);

        // Provider-executed search and URL tools, for the provider the model's client
        // serves only, declared to runs starting in Ask or Auto; by default the provider's
        // search when web is on, replaced by a configured search provider (spec 3.1 §7.20,
        // I49).
        #[cfg(feature = "web")]
        let (provider_web, web_search_path) = {
            let tools = self.provider_web_tools.unwrap_or(if self.web.is_some() {
                crate::web::ProviderWebTools::provider_default()
            } else {
                crate::web::ProviderWebTools::off()
            });
            let toolset = if tools.any() {
                let allowlist =
                    self.web.as_ref().map(|web| web.allowed_domains.clone()).unwrap_or_default();
                Some(crate::web::provider::ProviderWebToolset::new(
                    tools,
                    model.as_ref(),
                    allowlist,
                    self.search_provider.is_some(),
                    mode_store.clone(),
                    self.mode,
                )?)
            } else {
                None
            };
            let path = match (&self.search_provider, &self.web) {
                (Some(provider), Some(_)) => {
                    crate::web::WebSearchPath::Governed { provider: provider.name().to_string() }
                }
                _ => match toolset.as_ref().and_then(|toolset| toolset.search_path()) {
                    Some(path) => path,
                    None if self.web.is_none() => {
                        crate::web::WebSearchPath::None { reason: "web is off".into() }
                    }
                    None => crate::web::WebSearchPath::None {
                        reason: "provider search is off and no search provider is configured"
                            .into(),
                    },
                },
            };
            let toolset = toolset.filter(|toolset| !toolset.tool_names().is_empty()).map(Arc::new);
            (toolset, path)
        };
        #[cfg(not(feature = "web"))]
        let web_search_path = crate::web::WebSearchPath::None {
            reason: "adk-harness is built without feature web".into(),
        };
        #[cfg(feature = "web")]
        let provider_web_names: BTreeSet<String> =
            provider_web.as_ref().map(|p| p.tool_names().into_iter().collect()).unwrap_or_default();
        #[cfg(not(feature = "web"))]
        let provider_web_names: BTreeSet<String> = BTreeSet::new();
        // 5 (policy first: the handler re-checks against it).
        let switches = PolicySwitches {
            auto_allow_sandboxed: shell.auto_allow_sandboxed,
            allow_host_shell_in_auto: self.shell.allow_host_shell_in_auto,
            enforcer_active: matches!(status, IsolationStatus::OsSandbox { .. })
                && shell.read_only_runs,
            sandbox_protects_paths: shell.protects_paths,
        };
        let policy = HarnessPolicy::new(PolicyInner {
            registry,
            mode_store: mode_store.clone(),
            default_mode: self.mode,
            rules: rules.clone(),
            ledger: ledger.clone(),
            workspace: workspace.clone(),
            shell: ShellView {
                status: status.clone(),
                allow_unsandboxed_retry,
                fail_if_unavailable: self.shell.fail_if_unavailable,
            },
            switches,
            trust_mcp_read_only_hints: self.trust_mcp_read_only_hints,
            prompts: PromptContexts::default(),
            provider_web_tools: provider_web_names,
            #[cfg(feature = "hooks")]
            hooks: hook_runner.clone(),
            #[cfg(feature = "auth")]
            identity: identity_gate.clone(),
            #[cfg(feature = "observe")]
            root_agent: self.name.clone(),
            rule_observer: std::sync::OnceLock::new(),
        });
        let prompting = match (&self.confirmation_handler, self.prompter) {
            (None, Some(prompter)) => {
                // The person is told about each call a standing rule allows (spec 3.1 §7.33
                // fix 3).
                policy.observe_rules(prompter.clone());
                let handler =
                    PromptingConfirmationHandler::new(prompter, rules.clone(), policy.clone());
                // A crossing is put to the person with its three answers (spec 3.2 §7.8).
                #[cfg(feature = "kernel")]
                let handler = match &partition {
                    Some(wired) => handler.with_partition(wired.wiring.clone()),
                    None => handler,
                };
                Some(Arc::new(handler))
            }
            _ => None,
        };
        let handler: Option<Arc<dyn ToolConfirmationHandler>> = match self.confirmation_handler {
            Some(handler) => Some(handler),
            None => prompting.clone().map(|handler| handler as Arc<dyn ToolConfirmationHandler>),
        };
        // Elicitations reach the person through the same prompter, or park in the approval
        // inbox (spec 3.1 §7.21).
        #[cfg(feature = "mcp")]
        if let Some(mcp) = &mcp {
            mcp.bind_surface(crate::mcp::ElicitationSurface {
                prompting: prompting.clone(),
                approval_service: self.approval_service.clone(),
                timeout: self.confirmation_timeout,
                mode_store: mode_store.clone(),
                default_mode: self.mode,
                isolation: status.clone(),
            });
        }
        #[cfg(not(feature = "mcp"))]
        let _ = &prompting;

        // 3. The LlmAgent.
        #[cfg(feature = "payments")]
        let mut packs: Vec<String> =
            payments.as_ref().map(|_| vec!["payments".to_string()]).unwrap_or_default();
        #[cfg(not(feature = "payments"))]
        let mut packs: Vec<String> = Vec::new();
        #[cfg(feature = "browser")]
        if browser.is_some() {
            packs.push("browser".to_string());
        }
        #[cfg(feature = "computer-use")]
        if computer_use.is_some() {
            packs.push("computer-use".to_string());
        }
        if self.managed_requested {
            packs.push("managed".to_string());
        }
        if self.voice_requested {
            packs.push("voice".to_string());
        }
        // What the harness can do, generated from what was just built (spec 3.1 §7.25).
        let manifest = {
            let isolation = status.isolation();
            let rows = toolset
                .all_tools()
                .iter()
                .map(|tool| {
                    let origin = adk_core::ToolOrigin::of(tool.as_ref());
                    let kind = policy.registry().classify(
                        &self.name,
                        tool.name(),
                        Some(&origin),
                        &serde_json::json!({}),
                        tool.effect(),
                        isolation,
                        self.trust_mcp_read_only_hints,
                    );
                    (tool.name().to_string(), crate::policy::TableRow::of(&kind, false))
                })
                .collect();
            let has_bash = tool_names.iter().any(|name| name == "bash");
            #[cfg(feature = "team")]
            let members =
                team_plan.as_ref().map(crate::team::TeamPlan::member_entries).unwrap_or_default();
            #[cfg(not(feature = "team"))]
            let members = Vec::new();
            #[cfg(feature = "memory")]
            let memory = crate::manifest::MemoryEntry {
                working_notes: memory_resolver.is_some(),
                long_term_facts: toolset.facts().is_some(),
                embedder: toolset.facts().map(|_| {
                    if self.long_term.embedder.is_some() {
                        "embeddings".to_string()
                    } else if self.long_term.index.is_some() {
                        "a custom index".to_string()
                    } else {
                        "lexical".to_string()
                    }
                }),
            };
            #[cfg(not(feature = "memory"))]
            let memory = crate::manifest::MemoryEntry::default();
            #[cfg(feature = "graph")]
            let plan_rewind = plan_graphs.is_some();
            #[cfg(not(feature = "graph"))]
            let plan_rewind = false;
            crate::manifest::HarnessManifest::generate(crate::manifest::ManifestInputs {
                surface: self
                    .surface
                    .take()
                    .unwrap_or_else(crate::manifest::SurfaceManifest::library),
                default_mode: self.mode,
                rows,
                switches,
                web_allowlist: policy.registry().web_allowlist(),
                shell: has_bash.then_some(&status),
                unsandboxed_retry: has_bash
                    && allow_unsandboxed_retry
                    && matches!(status, IsolationStatus::OsSandbox { .. }),
                members,
                active_packs: &packs,
                web: web_search_path.clone(),
                memory,
                ledger_durable: self.action_ledger.is_some(),
                plan_rewind,
                snapshots: snapshots.as_ref().map(|snapshots| snapshots.config().clone()),
                model: model.name(),
            })
        };
        let harness_block = manifest.render();
        let _ = manifest_cell.set(manifest.clone());
        let environment = Environment {
            root: workspace.root().display().to_string(),
            branch: instruction_state.instructions.git_branch().map(str::to_string),
            isolation: if host_shell_refused && !container_shell {
                "none".to_string()
            } else {
                status.to_string()
            },
            default_mode: self.mode,
            packs: packs.clone(),
        };
        let parts = Arc::new(PromptParts {
            base: self.base_prompt,
            operator: self.instruction,
            environment,
            harness: harness_block,
            state: instruction_state.clone(),
        });
        #[cfg(feature = "memory")]
        let provider_memory = memory_resolver.clone();
        #[cfg(feature = "hooks")]
        let provider_hooks = hook_runner.clone();
        // Assembled once per run and reused by every model call of it, so the request
        // prefix stays byte-stable while tools write memory or trust changes (spec 3.1
        // §7.7, §9.2 I56).
        let assembled: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let provider: InstructionProvider = Box::new(move |ctx: Arc<dyn ReadonlyContext>| {
            let parts = parts.clone();
            #[cfg(feature = "hooks")]
            let hooks = provider_hooks.clone();
            let assembled = assembled.clone();
            #[cfg(feature = "memory")]
            let memory = provider_memory.clone();
            Box::pin(async move {
                let key = format!("{}\u{1f}{}", ctx.session_id(), ctx.invocation_id());
                if let Some(instruction) =
                    assembled.lock().unwrap_or_else(|e| e.into_inner()).get(&key)
                {
                    return Ok(instruction.clone());
                }
                let instruction = parts.assemble();
                // The session's OnSessionStart notes join the <environment> block; they are
                // cached per session, so the prefix stays stable within a run (I56).
                #[cfg(feature = "hooks")]
                let instruction = match &hooks {
                    Some(hooks) => hooks.with_session_notes(
                        instruction,
                        ctx.app_name(),
                        ctx.user_id(),
                        ctx.session_id(),
                    ),
                    None => instruction,
                };
                #[cfg(feature = "memory")]
                let instruction = match memory {
                    Some(memory) => memory.append_index(instruction, ctx.as_ref()).await,
                    None => instruction,
                };
                let mut cache = assembled.lock().unwrap_or_else(|e| e.into_inner());
                if cache.len() >= 64 {
                    cache.clear();
                }
                cache.insert(key, instruction.clone());
                Ok(instruction)
            })
        });
        let non_waivable: BTreeSet<String> = ["exit_plan_mode", "memory_write", "memory_delete"]
            .into_iter()
            .map(str::to_string)
            .collect();
        // A payment is never covered by a `tool_approvals` entry (spec 3.1 §7.16, §9.2 I38).
        #[cfg(feature = "payments")]
        let non_waivable: BTreeSet<String> = if payments.is_some() {
            non_waivable
                .into_iter()
                .chain(
                    crate::packs::payments::PAYMENT_TOOL_NAMES
                        .iter()
                        .map(|name| (*name).to_string()),
                )
                .collect()
        } else {
            non_waivable
        };
        // A desktop action is never covered by a `tool_approvals` entry (spec 3.1 §7.24, I59).
        #[cfg(feature = "computer-use")]
        let non_waivable: BTreeSet<String> = if computer_use.is_some() {
            non_waivable
                .into_iter()
                .chain(std::iter::once(
                    crate::packs::computer_use::COMPUTER_USE_EXECUTE.to_string(),
                ))
                .collect()
        } else {
            non_waivable
        };
        // A provider that refuses its built-in web tools gets the request again without
        // them (spec 3.1 §7.20).
        #[cfg(feature = "web")]
        let agent_model: Arc<dyn Llm> = match &provider_web {
            Some(provider_web) => Arc::new(crate::web::provider::ProviderSearchFallback::new(
                model.clone(),
                provider_web.clone(),
            )),
            None => model.clone(),
        };
        #[cfg(not(feature = "web"))]
        let agent_model = model.clone();
        let mut builder = LlmAgentBuilder::new(self.name.clone())
            .model(agent_model)
            .description(self.description.unwrap_or_else(|| {
                "A governed harness that reads, edits and runs code in the workspace under approvals."
                    .to_string()
            }))
            .instruction_provider(provider)
            .toolset(toolset.clone())
            .max_iterations(self.max_iterations)
            .tool_timeout(self.tool_timeout)
            .parallelize_agent_delegations(true)
            .tool_confirmation_policy(ToolConfirmationPolicy::PerTool(
                ["exit_plan_mode".to_string()].into_iter().collect(),
            ));
        #[cfg(feature = "skills")]
        {
            match crate::skills::wire(workspace.root(), &self.skills)? {
                crate::skills::SkillsWiring::None => {}
                crate::skills::SkillsWiring::Toolset(_) if public_only.is_some() => {}
                crate::skills::SkillsWiring::Toolset(skills) => builder = builder.toolset(skills),
                crate::skills::SkillsWiring::Inject(index) => builder = builder.with_skills(index),
            }
        }
        // The operator's own tools and toolsets stay, so the public partition's policy refuses
        // one attached by mistake on the record; the harness adds no toolset of its own there.
        for tool in self.tools {
            let tool = match &self.take_over {
                Some(_) => crate::take_over::ScopedTool::wrap(tool, self.tool_timeout),
                None => tool,
            };
            #[cfg(feature = "auth")]
            let tool = self.identity.protect_extra(tool);
            builder = builder.tool(tool);
        }
        for extra in self.toolsets {
            let extra = match &self.take_over {
                Some(_) => crate::take_over::ScopedToolset::wrap(extra, self.tool_timeout),
                None => extra,
            };
            builder = builder.toolset(extra);
        }
        // The coordinator's crossings into other partitions' drivers (spec 3.2 §7.8).
        #[cfg(feature = "kernel")]
        if let Some(wired) = partition.as_ref().filter(|_| public_only.is_none()) {
            for crossings in &wired.toolsets {
                let crossings = match &self.take_over {
                    Some(_) => {
                        crate::take_over::ScopedToolset::wrap(crossings.clone(), self.tool_timeout)
                    }
                    None => crossings.clone(),
                };
                builder = builder.toolset(crossings);
            }
        }
        #[cfg(feature = "web")]
        if let Some(provider_web) = provider_web.as_ref().filter(|_| public_only.is_none()) {
            builder = builder.toolset(provider_web.clone());
        }
        #[cfg(feature = "mcp")]
        if let Some(mcp) = mcp.as_ref().filter(|_| public_only.is_none()) {
            builder = builder.toolset(mcp.manager());
        }
        if let Some(customize) = self.customize {
            builder = customize(builder);
        }
        #[cfg(feature = "memory")]
        if memory_resolver.is_some() {
            builder = builder.tool_confirmation_policy(ToolConfirmationPolicy::PerTool(
                ["exit_plan_mode", "memory_write", "memory_delete"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ));
        }
        // Payments are non-waivable: per-tool confirmation as defence in depth (spec 3.1 §7.2).
        #[cfg(feature = "payments")]
        if payments.is_some() {
            builder = builder
                .tool_confirmation_policy(ToolConfirmationPolicy::PerTool(non_waivable.clone()));
        }
        // An invocation's approved crossing reads end with it (spec 3.2 §13 Q3); a durable
        // run's end with the run, released by the kernel's worker.
        #[cfg(feature = "kernel")]
        if let Some(wired) = &partition {
            builder = builder.after_callback(crate::partition::release_on_end(
                wired.wiring.policy.clone(),
                self.name.clone(),
            ));
        }
        let agent = builder.build()?;
        // No run-wide approval answers a crossing (spec 3.2 K-I15).
        #[cfg(feature = "kernel")]
        let non_waivable: BTreeSet<String> = match &partition {
            Some(wired) => non_waivable
                .into_iter()
                .chain(wired.wiring.non_waivable(&wired.crossings))
                .collect(),
            None => non_waivable,
        };
        let coordinator: Arc<dyn adk_core::Agent> = Arc::new(agent);
        // 4. Members lowered to one TeamSpec with the coordinator (spec 3.1 §7.10).
        #[cfg(feature = "team")]
        let team = match team_plan {
            Some(plan) => Some(Arc::new(plan.compile(crate::team::TeamInputs {
                harness: &self.name,
                coordinator: coordinator.clone(),
                model: self.member_model.clone().unwrap_or_else(|| model.clone()),
                tools: &member_tools,
                mode_store: mode_store.clone(),
                default_mode: self.mode,
                workspace: &workspace,
                tool_timeout: self.tool_timeout,
                #[cfg(feature = "skills")]
                skills: &self.skills,
                observers: &observers,
                effort: self.effort,
            })?)),
            None => None,
        };
        #[cfg(feature = "team")]
        let root = team.as_ref().map_or_else(|| coordinator.clone(), |team| team.root().clone());
        #[cfg(all(feature = "graph", feature = "team"))]
        if let (Some(graphs), Some(team)) = (&plan_graphs, &team)
            && let Some(reviewer) = team.reviewer()
        {
            graphs.set_reviewer(reviewer);
        }
        #[cfg(not(feature = "team"))]
        let root = coordinator.clone();

        let goal_store =
            self.goal_store.unwrap_or_else(|| Arc::new(crate::goal::InMemoryGoalStore::new()));
        // A goal's check runs through the launcher the harness bash uses (spec 3.1 §7.11).
        let check_launcher: Arc<dyn adk_devtools::CommandLauncher> =
            Arc::new(adk_devtools::HostLauncher);
        #[cfg(feature = "sandbox")]
        let check_launcher: Arc<dyn adk_devtools::CommandLauncher> = match shell.launcher.clone() {
            Some(launcher) => Arc::new(launcher),
            None => check_launcher,
        };
        // The container shell has no launcher; a check never falls back to the host (I24).
        #[cfg(feature = "docker")]
        let check_launcher: Arc<dyn adk_devtools::CommandLauncher> = match shell.container {
            Some(_) => Arc::new(crate::shell::ContainerCheckRefusal),
            None => check_launcher,
        };
        let goals = crate::goal::GoalRuntime {
            store: goal_store.clone(),
            until: Arc::new(crate::goal::UntilCheckAgent::new(
                goal_store,
                workspace.clone(),
                check_launcher,
            )),
            #[cfg(feature = "graph")]
            engine: self.goal_engine.unwrap_or(if self.plan_checkpointer.is_some() {
                crate::goal::GoalEngine::Graph
            } else {
                crate::goal::GoalEngine::Loop
            }),
            #[cfg(feature = "graph")]
            checkpointer: self
                .plan_checkpointer
                .clone()
                .unwrap_or_else(|| Arc::new(adk_graph::MemoryCheckpointer::new())),
        };
        let compaction = if self.compaction_enabled {
            Some(self.compaction.unwrap_or_else(|| EventsCompactionConfig {
                compaction_interval: 10,
                overlap_size: 4,
                summarizer: Arc::new(LlmEventSummarizer::new(model)),
            }))
        } else {
            None
        };
        let degraded_reason = match &status {
            IsolationStatus::Degraded { reason, .. } => Some(reason.clone()),
            _ => None,
        };
        #[cfg(all(feature = "kernel", feature = "web"))]
        let _ = agent_limits.set(
            crate::agents::suggester_ceiling(
                tool_names.iter().map(String::as_str),
                &packs,
                tool_names.iter().any(|tool| tool == "bash").then_some(&status),
                self.mode,
                &partition
                    .as_ref()
                    .map(|wired| wired.wiring.partition.drivers.clone())
                    .unwrap_or_default(),
            )
            .uses,
        );
        let info = HarnessInfo {
            name: self.name,
            default_mode: self.mode,
            isolation: status,
            degraded_reason,
            #[cfg(feature = "team")]
            members: team.as_ref().map(|team| team.member_names()).unwrap_or_default(),
            #[cfg(not(feature = "team"))]
            members: Vec::new(),
            packs,
            tools: tool_names,
            effort: self.effort,
            context_caching: self.context_caching,
            web_search: web_search_path,
        };
        let _ = (self.default_members, self.member_model, self.plan_graph);
        Ok(HarnessAgent {
            root,
            runtime: HarnessRuntime {
                policy,
                // The partition's policy makes the operator layer stricter (spec 3.2 K-I6).
                #[cfg(feature = "kernel")]
                operator_policy: match &partition {
                    Some(wired) => Some(wired.wiring.operator_policy(self.operator_policy)),
                    None => self.operator_policy,
                },
                #[cfg(not(feature = "kernel"))]
                operator_policy: self.operator_policy,
                mode_store,
                standing_rules: rules,
                ledger,
                approval_store,
                approval_service: self.approval_service,
                ledgers: crate::ledger::HarnessLedgers::new(
                    self.action_ledger.unwrap_or_else(|| Arc::new(InMemoryActionLedger::new())),
                ),
                handler,
                confirmation_timeout: self.confirmation_timeout,
                budget: self.budget,
                #[cfg(not(feature = "observe"))]
                spend_ledger: self.spend_ledger,
                // Spend is metered on every run by default (spec 3.1 §7.13).
                #[cfg(feature = "observe")]
                spend_ledger: self.spend_ledger.or_else(|| {
                    Some(Arc::new(adk_core::InMemorySpendLedger::default()) as Arc<dyn SpendLedger>)
                }),
                history_max_events: self.history_max_events,
                compaction,
                toolset,
                instructions: instruction_state,
                non_waivable_tools: non_waivable,
                non_waivable_prefixes,
                workspace,
                audit: harness_audit,
                #[cfg(feature = "auth")]
                identity: identity_gate,
                #[cfg(feature = "observe")]
                spend: Arc::default(),
                #[cfg(feature = "observe")]
                run_trails,
                #[cfg(feature = "team")]
                team,
                #[cfg(feature = "graph")]
                plan_graph: plan_graphs,
                snapshots,
                goals,
                #[cfg(feature = "kernel")]
                kernel_approvals: self.kernel_approvals,
                #[cfg(feature = "kernel")]
                kernel_agenda: self.kernel_agenda,
                #[cfg(feature = "kernel")]
                kernel_ledger: self.kernel_ledger,
                #[cfg(feature = "kernel")]
                partition: partition.map(|wired| wired.wiring),
                #[cfg(feature = "kernel")]
                session_stamp: self.session_stamp,
                #[cfg(feature = "web")]
                provider_web,
                #[cfg(feature = "mcp")]
                mcp,
                #[cfg(feature = "hooks")]
                hooks: hook_runner,
                effort: self.effort,
                cache_capable: self.context_caching.then_some(self.cache_capable).flatten(),
                #[cfg(feature = "payments")]
                payments: payments.map(|payments| payments.reminder),
                take_over: self.take_over,
            },
            info,
            manifest,
        })
    }
}

/// The `<environment>` block (spec 3.1 §7.7 item 2).
struct Environment {
    root: String,
    branch: Option<String>,
    isolation: String,
    default_mode: HarnessMode,
    packs: Vec<String>,
}

/// The parts of the harness instruction, assembled once per run (spec 3.1 §7.7).
struct PromptParts {
    base: Cow<'static, str>,
    operator: Option<String>,
    environment: Environment,
    /// The `<harness>` block (spec 3.1 §7.25), after `<environment>` in the cached prefix.
    harness: String,
    state: Arc<InstructionState>,
}

impl PromptParts {
    fn assemble(&self) -> String {
        let env = &self.environment;
        let mut out = String::with_capacity(self.base.len() + 1024);
        out.push_str(&self.base);
        out.push_str("\n\n");
        if let Some(operator) = &self.operator {
            out.push_str(operator);
            out.push_str("\n\n");
        }
        out.push_str(&format!(
            "<environment>\nworkspace root: {}\nplatform: {} {}\ndate: {}\ngit branch: {}\nshell: {}\ndefault mode: {}\npacks: {}\n</environment>\n\n",
            env.root,
            std::env::consts::OS,
            std::env::consts::ARCH,
            chrono::Utc::now().format("%Y-%m-%d"),
            env.branch.as_deref().unwrap_or("none"),
            env.isolation,
            env.default_mode,
            if env.packs.is_empty() { "none".to_string() } else { env.packs.join(", ") },
        ));
        out.push_str(&self.harness);
        out.push_str("\n\n");
        out.push_str(&self.state.instructions.render(self.state.trusted()));
        out.trim_end().to_string()
    }
}

/// The hook runner of a build (spec 3.1 §7.22), or `None` when hooks are off or none are
/// configured. Workspace hooks come from `.adk/harness.toml` and run only while the
/// workspace's instruction files, the hooks file included, are trusted (I54).
#[cfg(feature = "hooks")]
fn build_hook_runner(
    enabled: bool,
    configured: Option<crate::hooks::HookSet>,
    workspace: &Workspace,
    shell: &crate::shell::ShellRuntime,
    status: &IsolationStatus,
    instructions: &Arc<InstructionState>,
) -> Result<Option<Arc<crate::hooks::HookRunner>>> {
    if !enabled {
        return Ok(None);
    }
    let mut set = configured.unwrap_or_default();
    if let Some(workspace_hooks) = crate::hooks::HookSet::load_workspace(workspace.root())? {
        set = set.merge(workspace_hooks);
    }
    set.validate()?;
    if set.is_empty() {
        return Ok(None);
    }
    // A hook runs through the shell's launcher (spec 3.1 §9.2 I54); the container shell has
    // none, and running `inherit` hooks on the host instead would escape the isolation.
    if matches!(status, IsolationStatus::Container { .. })
        && set.hooks.iter().any(|hook| hook.isolation == crate::hooks::HookIsolation::Inherit)
    {
        return Err(AdkError::new(
            adk_core::ErrorComponent::Agent,
            adk_core::ErrorCategory::InvalidInput,
            crate::hooks::HOOK_CONFIG_CODE,
            "hooks with `inherit` isolation cannot run inside the container shell yet; give them \
             `isolation = \"host\"`, run the harness with the OS sandbox, or disable hooks \
             (spec 3.1 §9.2 I54)",
        ));
    }
    // The Windows restricted-token sandbox starts its processes itself, and a hook needs a
    // command it can spawn and feed (spec 3.2 §10 W5).
    #[cfg(all(windows, feature = "sandbox"))]
    if shell.launcher.as_ref().is_some_and(crate::shell::SandboxLauncher::starts_processes_itself)
        && set.hooks.iter().any(|hook| hook.isolation == crate::hooks::HookIsolation::Inherit)
    {
        return Err(AdkError::new(
            adk_core::ErrorComponent::Agent,
            adk_core::ErrorCategory::InvalidInput,
            crate::hooks::HOOK_CONFIG_CODE,
            "hooks with `inherit` isolation cannot run inside the Windows restricted-token \
             sandbox yet; give them `isolation = \"host\"`, or disable hooks (spec 3.2 §10 W5)",
        ));
    }
    #[cfg_attr(not(feature = "sandbox"), allow(unused_mut))]
    let mut runner = crate::hooks::HookRunner::new(set, workspace.clone(), status.clone());
    #[cfg(feature = "sandbox")]
    if let Some(launcher) = &shell.launcher {
        runner = runner
            .with_launcher(Arc::new(launcher.clone()))
            .with_read_only_launcher(Arc::new(launcher.read_only()));
    }
    #[cfg(not(feature = "sandbox"))]
    let _ = shell;
    let state = instructions.clone();
    Ok(Some(Arc::new(runner.with_workspace_trust(move || state.trusted()))))
}

/// The long-term memory configuration of a builder (spec 3.1 §7.15.2).
#[cfg(feature = "memory")]
#[derive(Default)]
struct LongTermOptions {
    config: Option<crate::facts::LongTermMemoryConfig>,
    store: Option<Arc<dyn crate::facts::FactStore>>,
    index: Option<Arc<dyn crate::facts::FactIndex>>,
    embedder: Option<Arc<dyn adk_memory::EmbeddingProvider>>,
    /// A partition's memory scope: the project of every fact, read and written, in place
    /// of the workspace's (spec 3.2 §7.8).
    project: Option<String>,
}

#[cfg(feature = "memory")]
impl LongTermOptions {
    /// The harness's long-term memory, or `None` when it is off.
    #[allow(clippy::too_many_arguments)]
    fn build(
        &self,
        agent: &str,
        workspace_root: &std::path::Path,
        require_authenticated: bool,
        audit: Option<adk_core::audit_log::AuditTrail>,
        member_model: Arc<dyn Llm>,
        budget: Option<RunBudget>,
        (mode_store, default_mode): (Arc<dyn ModeStore>, HarnessMode),
        distiller: crate::posture::Effort,
    ) -> Result<Option<Arc<crate::facts::runtime::FactsRuntime>>> {
        let Some(config) = self.config.clone() else {
            if self.store.is_some() || self.index.is_some() || self.embedder.is_some() {
                return Err(AdkError::config(
                    "fact_store, fact_index and memory_embedder configure long-term memory; \
                     turn it on with long_term_memory(LongTermMemoryConfig::default())",
                ));
            }
            return Ok(None);
        };
        if self.index.is_some() && self.embedder.is_some() {
            return Err(AdkError::config(
                "memory_embedder configures the default LocalFactIndex; give your own index \
                 its embedder instead, for example LocalFactIndex::with_embedder",
            ));
        }
        let store =
            self.store.clone().unwrap_or_else(|| Arc::new(crate::facts::InMemoryFactStore::new()));
        let index = match (&self.index, &self.embedder) {
            (Some(index), _) => index.clone(),
            (None, Some(embedder)) => Arc::new(
                crate::facts::LocalFactIndex::new(store.clone()).with_embedder(embedder.clone()),
            ),
            (None, None) => Arc::new(crate::facts::LocalFactIndex::new(store.clone())),
        };
        // The distiller runs at `distiller` (spec 3.1 §7.23), whichever model it is.
        let distill_model = crate::posture::AtEffort::wrap(
            config.distill_model.clone().unwrap_or(member_model),
            distiller,
        );
        // Consolidation embeds with the default index's embedder (spec 3.2 §7.5).
        let embedder = if self.index.is_none() { self.embedder.clone() } else { None };
        let project =
            self.project.clone().unwrap_or_else(|| crate::memory::project_slug(workspace_root));
        Ok(Some(Arc::new(
            crate::facts::runtime::FactsRuntime::new(
                config,
                store,
                index,
                project,
                agent.to_string(),
                require_authenticated,
                audit,
                distill_model,
                budget,
                mode_store,
                default_mode,
            )
            .with_embedder(embedder)
            .confined(self.project.is_some()),
        )))
    }
}

/// The identity configuration of a builder (spec 3.1 §7.14).
#[cfg(feature = "auth")]
#[derive(Default)]
struct IdentityOptions {
    broker: Option<Arc<dyn adk_core::CredentialBroker>>,
    shell_credentials: Vec<crate::identity::ShellCredential>,
    rbac: Option<crate::identity::RbacMap>,
    require_authenticated: bool,
}

#[cfg(feature = "auth")]
impl IdentityOptions {
    /// `bash` with the credential helper when a broker is set.
    fn with_credentials(&self, bash: HarnessBashTool, root: &std::path::Path) -> Arc<dyn Tool> {
        let Some(broker) = &self.broker else {
            return Arc::new(bash);
        };
        let calls = Arc::new(crate::identity::CallEnvironment::default());
        let launcher_calls = calls.clone();
        let bash = bash.with_launcher_layer(move |inner| {
            Arc::new(crate::identity::CredentialLauncher::new(inner, launcher_calls.clone()))
        });
        Arc::new(crate::identity::CredentialedTool::new(
            Arc::new(bash),
            broker.clone(),
            crate::identity::resolve_credentials(self.shell_credentials.clone(), root),
            calls,
        ))
    }

    /// The harness tools, each wrapped for the permission its class needs.
    fn protect(
        &self,
        tools: Vec<Arc<dyn Tool>>,
        registry: &ToolClassRegistry,
        agent: &str,
        isolation: crate::policy::Isolation,
    ) -> Vec<Arc<dyn Tool>> {
        let Some(rbac) = &self.rbac else {
            return tools;
        };
        tools
            .into_iter()
            .map(|tool| {
                let class = registry.classify(
                    agent,
                    tool.name(),
                    None,
                    &serde_json::json!({}),
                    tool.effect(),
                    isolation,
                    false,
                );
                rbac.protect(tool, &class)
            })
            .collect()
    }

    /// An extra tool, wrapped for the permission its `External` class needs.
    fn protect_extra(&self, tool: Arc<dyn Tool>) -> Arc<dyn Tool> {
        match &self.rbac {
            Some(rbac) => {
                let class = ToolKind::External {
                    effect: tool.effect(),
                    trusted_origin: tool.origin() == adk_core::STATIC_TOOL_ORIGIN,
                };
                rbac.protect(tool, &class)
            }
            None => tool,
        }
    }
}
