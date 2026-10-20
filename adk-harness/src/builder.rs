//! `HarnessAgentBuilder` (spec 3.1 §7.2).

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
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
#[cfg(feature = "auth")]
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
use crate::toolset::HarnessToolset;

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
    /// The managed form.
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
    operator_policy: Option<Arc<dyn ToolPolicy>>,
    prompter: Option<Arc<dyn ApprovalPrompter>>,
    confirmation_handler: Option<Arc<dyn ToolConfirmationHandler>>,
    approval_store: Option<Arc<dyn ApprovalStore>>,
    approval_service: Option<Arc<dyn ApprovalService>>,
    action_ledger: Option<Arc<dyn ActionLedger>>,
    confirmation_timeout: Duration,
    shell: ShellConfig,
    shell_enabled: bool,
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
    #[cfg(feature = "memory")]
    memory: Option<crate::memory::MemoryConfig>,
    #[cfg(feature = "graph")]
    plan_checkpointer: Option<Arc<dyn adk_graph::Checkpointer>>,
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
            operator_policy: None,
            prompter: None,
            confirmation_handler: None,
            approval_store: None,
            approval_service: None,
            action_ledger: None,
            confirmation_timeout: Duration::from_secs(300),
            shell: ShellConfig::default(),
            shell_enabled: true,
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
            #[cfg(feature = "memory")]
            memory: None,
            #[cfg(feature = "graph")]
            plan_checkpointer: None,
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
        }
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

    /// An operator policy, composed with the harness policy; the stricter decision wins.
    pub fn operator_policy(mut self, policy: Arc<dyn ToolPolicy>) -> Self {
        self.operator_policy = Some(policy);
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

    /// The approval inbox held calls are submitted to.
    pub fn approval_service(mut self, service: Arc<dyn ApprovalService>) -> Self {
        self.approval_service = Some(service);
        self
    }

    /// How long a person may take to decide one call (default 300 s); a timeout denies.
    pub fn confirmation_timeout(mut self, timeout: Duration) -> Self {
        self.confirmation_timeout = timeout;
        self
    }

    /// Where `bash` runs (default host, spec 3.1 §7.9).
    pub fn shell(mut self, isolation: ShellIsolation) -> Self {
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
    /// read-only (adk-sandbox S3, checked by a conformance probe), with the network denied
    /// and every root-level protected path present; otherwise `build()` fails with
    /// `harness.auto_allow_sandboxed_unavailable` naming the reason. No effect on the host
    /// or a degraded shell.
    pub fn auto_allow_sandboxed(mut self) -> Self {
        self.shell.auto_allow_sandboxed = true;
        self
    }

    /// Offers an unsandboxed retry of a failed sandboxed command (default on).
    pub fn allow_unsandboxed_retry(mut self, yes: bool) -> Self {
        self.shell.allow_unsandboxed_retry = yes;
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

    /// File memory (spec 3.1 §7.15; off until set): adds `memory_read`, `memory_write`
    /// and `memory_delete`, and the memory index at the end of the instruction.
    #[cfg(feature = "memory")]
    pub fn memory(mut self, config: crate::memory::MemoryConfig) -> Self {
        self.memory = Some(config);
        self
    }

    /// Where plan graphs are checkpointed (spec 3.1 §7.11; default in memory): SQLite
    /// beside the harness stores in the CLI, the session's backend on a server.
    #[cfg(feature = "graph")]
    pub fn plan_checkpointer(mut self, checkpointer: Arc<dyn adk_graph::Checkpointer>) -> Self {
        self.plan_checkpointer = Some(checkpointer);
        self
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

    /// Loop detection (spec 3.1 §7.13; not yet implemented).
    #[cfg(feature = "observe")]
    pub fn loop_guard(mut self, guard: LoopGuard) -> Self {
        self.loop_guard = Some(guard);
        self
    }

    /// Adds a pack (spec 3.1 §7.16–§7.18; not yet implemented).
    pub fn pack(mut self, pack: Pack) -> Self {
        self.unimplemented.push(format!("pack {pack:?} (spec 3.1 §7.16–§7.18)"));
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
    pub fn build(self) -> Result<HarnessAgent> {
        let model = self.model.ok_or_else(|| AdkError::config("HarnessAgent requires a model"))?;
        let workspace =
            self.workspace.ok_or_else(|| AdkError::config("HarnessAgent requires a workspace"))?;
        let unimplemented = self.unimplemented;
        if let Some(first) = unimplemented.first() {
            return Err(AdkError::config(format!("not yet implemented: {first}")));
        }
        let harness_names: HashSet<&str> = HARNESS_TOOL_NAMES.iter().copied().collect();
        for tool in &self.tools {
            if harness_names.contains(tool.name()) {
                return Err(AdkError::config(format!(
                    "tool '{}' collides with a harness tool of the same name",
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
        #[cfg_attr(not(feature = "team"), allow(unused_mut))]
        let mut registry = ToolClassRegistry::harness_defaults(&self.name);
        // 4 (resolved here: member names register as harness classes, spec 3.1 §7.4).
        #[cfg(feature = "team")]
        let team_plan = {
            let mut reserved: HashSet<String> =
                HARNESS_TOOL_NAMES.iter().map(|name| (*name).to_string()).collect();
            reserved.extend(self.tools.iter().map(|tool| tool.name().to_string()));
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
        let ledger = self.shell_ledger.unwrap_or_else(|| Arc::new(InMemoryShellLedger::new()));
        let approval_store = self
            .approval_store
            .or_else(|| self.approval_service.as_ref().and_then(|service| service.approval_store()))
            .unwrap_or_else(|| Arc::new(InMemoryApprovalStore::new()));

        let mut tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(ReadFileTool::new(workspace.clone())),
            Arc::new(GlobTool::new(workspace.clone())),
            Arc::new(GrepTool::new(workspace.clone())),
        ];
        if workspace.is_writable() {
            tools.push(Arc::new(WriteFileTool::new(workspace.clone())));
            tools.push(Arc::new(EditFileTool::new(workspace.clone())));
        }
        if self.shell_enabled && workspace.bash_allowed() {
            let bash = HarnessBashTool::new(
                workspace.clone(),
                status.clone(),
                self.shell.fail_if_unavailable,
                ledger.clone(),
            )
            .with_mode_store(mode_store.clone(), self.mode)
            .allow_unsandboxed_retry(self.shell.allow_unsandboxed_retry);
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
        let plan_exit = ExitPlanModeTool::new(mode_store.clone(), self.mode);
        #[cfg(feature = "graph")]
        let plan_exit = match &plan_graphs {
            Some(graphs) => plan_exit.with_plan_graph(graphs.clone()),
            None => plan_exit,
        };
        tools.push(Arc::new(plan_exit));
        #[cfg(feature = "memory")]
        let memory_resolver = self.memory.clone().map(|config| {
            let resolver =
                crate::tools::MemoryResolver::new(config, workspace.root().to_path_buf());
            tools.push(Arc::new(crate::tools::MemoryReadTool::new(resolver.clone())));
            tools.push(Arc::new(crate::tools::MemoryWriteTool::new(resolver.clone())));
            tools.push(Arc::new(crate::tools::MemoryDeleteTool::new(resolver.clone())));
            resolver
        });
        let harness_audit = crate::agent::harness_audit_trail(self.audit_log)?;
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
            )?
        };
        #[cfg(feature = "memory")]
        if let Some(facts) = &facts {
            tools.push(Arc::new(crate::tools::MemorySearchTool::new(facts.clone())));
            tools.push(Arc::new(crate::tools::MemoryRememberTool::new(facts.clone())));
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
        #[cfg(feature = "team")]
        let member_tools = tools.clone();
        // Members get the same request observers as the coordinator (spec 3.1 §7.10, §7.13).
        #[cfg_attr(not(any(feature = "auth", feature = "observe")), allow(unused_mut))]
        let mut observers: Vec<Arc<dyn crate::toolset::RequestObserver>> = Vec::new();
        #[cfg(feature = "auth")]
        observers.push(identity_gate.clone());
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
        #[cfg_attr(not(feature = "memory"), allow(unused_mut))]
        let mut toolset =
            HarnessToolset::new(tools, mode_store.clone(), self.mode).with_observers(&observers);
        #[cfg(feature = "memory")]
        if let Some(facts) = facts {
            toolset = toolset.with_facts(facts);
        }
        let toolset = Arc::new(toolset);

        // 5 (policy first: the handler re-checks against it).
        let switches = PolicySwitches {
            auto_allow_sandboxed: shell.auto_allow_sandboxed,
            allow_host_shell_in_auto: self.shell.allow_host_shell_in_auto,
            enforcer_active: matches!(status, IsolationStatus::OsSandbox { .. }),
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
                allow_unsandboxed_retry: self.shell.allow_unsandboxed_retry,
                fail_if_unavailable: self.shell.fail_if_unavailable,
            },
            switches,
            trust_mcp_read_only_hints: self.trust_mcp_read_only_hints,
            prompts: PromptContexts::default(),
            #[cfg(feature = "auth")]
            identity: identity_gate.clone(),
            #[cfg(feature = "observe")]
            root_agent: self.name.clone(),
        });
        let handler: Option<Arc<dyn ToolConfirmationHandler>> =
            match (self.confirmation_handler, self.prompter) {
                (Some(handler), _) => Some(handler),
                (None, Some(prompter)) => Some(Arc::new(PromptingConfirmationHandler::new(
                    prompter,
                    rules.clone(),
                    policy.clone(),
                ))),
                (None, None) => None,
            };

        // 3. The LlmAgent.
        let environment = Environment {
            root: workspace.root().display().to_string(),
            branch: instruction_state.instructions.git_branch().map(str::to_string),
            isolation: status.to_string(),
            default_mode: self.mode,
            packs: Vec::new(),
        };
        let parts = Arc::new(PromptParts {
            base: self.base_prompt,
            operator: self.instruction,
            environment,
            state: instruction_state.clone(),
        });
        #[cfg(feature = "memory")]
        let provider_memory = memory_resolver.clone();
        let provider: InstructionProvider = Box::new(move |ctx: Arc<dyn ReadonlyContext>| {
            let parts = parts.clone();
            #[cfg(feature = "memory")]
            let memory = provider_memory.clone();
            Box::pin(async move {
                let instruction = parts.assemble();
                #[cfg(feature = "memory")]
                let instruction = match memory {
                    Some(memory) => memory.append_index(instruction, ctx.as_ref()).await,
                    None => instruction,
                };
                let _ = &ctx;
                Ok(instruction)
            })
        });
        let non_waivable: BTreeSet<String> = ["exit_plan_mode", "memory_write", "memory_delete"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let mut builder = LlmAgentBuilder::new(self.name.clone())
            .model(model.clone())
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
                crate::skills::SkillsWiring::Toolset(skills) => builder = builder.toolset(skills),
                crate::skills::SkillsWiring::Inject(index) => builder = builder.with_skills(index),
            }
        }
        for tool in self.tools {
            #[cfg(feature = "auth")]
            let tool = self.identity.protect_extra(tool);
            builder = builder.tool(tool);
        }
        for extra in self.toolsets {
            builder = builder.toolset(extra);
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
        let agent = builder.build()?;
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
        let info = HarnessInfo {
            name: self.name,
            default_mode: self.mode,
            isolation: status,
            degraded_reason,
            #[cfg(feature = "team")]
            members: team.as_ref().map(|team| team.member_names()).unwrap_or_default(),
            #[cfg(not(feature = "team"))]
            members: Vec::new(),
            packs: Vec::new(),
            tools: tool_names,
        };
        let _ = (self.default_members, self.member_model, self.plan_graph);
        Ok(HarnessAgent {
            root,
            runtime: HarnessRuntime {
                policy,
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
                goals,
            },
            info,
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
        out.push_str(&self.state.instructions.render(self.state.trusted()));
        out.trim_end().to_string()
    }
}

/// The long-term memory configuration of a builder (spec 3.1 §7.15.2).
#[cfg(feature = "memory")]
#[derive(Default)]
struct LongTermOptions {
    config: Option<crate::facts::LongTermMemoryConfig>,
    store: Option<Arc<dyn crate::facts::FactStore>>,
    index: Option<Arc<dyn crate::facts::FactIndex>>,
    embedder: Option<Arc<dyn adk_memory::EmbeddingProvider>>,
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
        let distill_model = config.distill_model.clone().unwrap_or(member_model);
        Ok(Some(Arc::new(crate::facts::runtime::FactsRuntime::new(
            config,
            store,
            index,
            crate::memory::project_slug(workspace_root),
            agent.to_string(),
            require_authenticated,
            audit,
            distill_model,
            budget,
            mode_store,
            default_mode,
        ))))
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
