//! `HarnessAgentBuilder` (spec 3.1 §7.2).

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::{LlmAgentBuilder, LlmEventSummarizer};
use adk_core::{
    AdkError, ApprovalService, ApprovalStore, EventsCompactionConfig, InMemoryApprovalStore,
    InstructionProvider, Llm, ReadonlyContext, Result, RunBudget, SpendLedger, Tool,
    ToolConfirmationHandler, ToolConfirmationPolicy, ToolPolicy, Toolset,
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
    member_registry: bool,
    #[cfg(feature = "team")]
    team_policy: bool,
    default_members: bool,
    member_model: Option<Arc<dyn Llm>>,
    plan_graph: bool,
    budget: Option<RunBudget>,
    spend_ledger: Option<Arc<dyn SpendLedger>>,
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
            member_registry: false,
            #[cfg(feature = "team")]
            team_policy: false,
            default_members: true,
            member_model: None,
            plan_graph: cfg!(feature = "graph"),
            budget: None,
            spend_ledger: None,
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

    /// Runs sandboxed commands without approval in Ask. Refuses to turn on until
    /// adk-sandbox S3 (spec 3.1 §10): the build fails while it is set.
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

    /// Adds a team member (spec 3.1 §7.10; not yet implemented).
    #[cfg(feature = "team")]
    pub fn member(mut self, spec: crate::team::MemberSpec) -> Self {
        self.members.push(spec);
        self
    }

    /// Discovers members at runtime (spec 3.1 §7.10; not yet implemented).
    #[cfg(feature = "team")]
    pub fn member_registry(mut self, _registry: Arc<dyn adk_agent::TeamAgentRegistry>) -> Self {
        self.member_registry = true;
        self
    }

    /// The team policy (spec 3.1 §7.10; not yet implemented).
    #[cfg(feature = "team")]
    pub fn team_policy(mut self, _policy: adk_agent::TeamPolicy) -> Self {
        self.team_policy = true;
        self
    }

    /// Leaves out the default members.
    pub fn without_default_members(mut self) -> Self {
        self.default_members = false;
        self
    }

    /// The model of members without their own (model tiering).
    pub fn member_model(mut self, model: Arc<dyn Llm>) -> Self {
        self.member_model = Some(model);
        self
    }

    /// The goal engine (spec 3.1 §7.11; not yet implemented).
    #[cfg(feature = "graph")]
    pub fn goal_engine(mut self, _engine: crate::goal::GoalEngine) -> Self {
        self.unimplemented.push("goal_engine (spec 3.1 §7.11)".to_string());
        self
    }

    /// Compiles approved plans with steps to a plan graph (default on with `graph`). The
    /// compiler lands with spec 3.1 §7.11; until then steps are stored with the plan.
    pub fn plan_graph(mut self, yes: bool) -> Self {
        self.plan_graph = yes;
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

    /// Loop detection (spec 3.1 §7.13; not yet implemented).
    #[cfg(feature = "observe")]
    pub fn loop_guard(mut self, _guard: LoopGuard) -> Self {
        self.unimplemented.push("loop_guard (spec 3.1 §7.13)".to_string());
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
        #[cfg_attr(not(feature = "team"), allow(unused_mut))]
        let mut unimplemented = self.unimplemented;
        #[cfg(feature = "team")]
        {
            if !self.members.is_empty() || self.member_registry || self.team_policy {
                unimplemented.push("team members (spec 3.1 §7.10)".to_string());
            }
        }
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
        let status = if self.shell_enabled {
            resolve_isolation(&self.shell)?
        } else {
            IsolationStatus::Host
        };

        // 2. Instructions and the class registry.
        let instructions = ProjectInstructions::load(workspace.root(), &self.instructions)?;
        let instruction_state = Arc::new(InstructionState {
            instructions,
            trust_store: self
                .trust_store
                .unwrap_or_else(|| Arc::new(InMemoryWorkspaceTrustStore::new())),
            require_trust: self.instructions.require_trust,
        });
        let registry = ToolClassRegistry::harness_defaults(&self.name);

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
            tools.push(Arc::new(HarnessBashTool::new(
                workspace.clone(),
                status.clone(),
                self.shell.fail_if_unavailable,
                ledger.clone(),
            )));
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
        tools.push(Arc::new(ExitPlanModeTool::new(mode_store.clone(), self.mode)));
        let tool_names: Vec<String> = tools.iter().map(|tool| tool.name().to_string()).collect();
        let toolset = Arc::new(HarnessToolset::new(tools, mode_store.clone(), self.mode));

        // 5 (policy first: the handler re-checks against it).
        let switches = PolicySwitches {
            auto_allow_sandboxed: false,
            allow_host_shell_in_auto: self.shell.allow_host_shell_in_auto,
            enforcer_active: matches!(status, IsolationStatus::OsSandbox { .. }),
            sandbox_protects_paths: false,
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
        let provider: InstructionProvider = Box::new(move |_ctx: Arc<dyn ReadonlyContext>| {
            let parts = parts.clone();
            Box::pin(async move { Ok(parts.assemble()) })
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
            builder = builder.tool(tool);
        }
        for extra in self.toolsets {
            builder = builder.toolset(extra);
        }
        if let Some(customize) = self.customize {
            builder = customize(builder);
        }
        let agent = builder.build()?;

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
            members: Vec::new(),
            packs: Vec::new(),
            tools: tool_names,
        };
        let _ = (self.default_members, self.member_model, self.plan_graph);
        Ok(HarnessAgent {
            root: Arc::new(agent),
            runtime: HarnessRuntime {
                policy,
                operator_policy: self.operator_policy,
                mode_store,
                standing_rules: rules,
                ledger,
                approval_store,
                approval_service: self.approval_service,
                handler,
                confirmation_timeout: self.confirmation_timeout,
                budget: self.budget,
                spend_ledger: self.spend_ledger,
                history_max_events: self.history_max_events,
                compaction,
                toolset,
                instructions: instruction_state,
                non_waivable_tools: non_waivable,
                workspace,
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
