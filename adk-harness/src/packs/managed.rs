//! The managed form of the harness (spec 3.1 §7.18, feature `managed`).
//!
//! A hosted, multi-tenant harness is a [`HarnessDef`]: a [`ManagedAgentDef`] that also
//! declares the harness mode, shell policy, skills roots, members and packs. A
//! [`HarnessResolver`] on the managed runtime
//! ([`DefaultManagedAgentRuntime::with_agent_resolver`]) builds the harness from it, the
//! runtime runs it in its supervised session loop, parks custom tool calls and held
//! confirmations on durable runs, and replays the session's events to `adk-enterprise`
//! clients from any sequence number.
//!
//! The two durability engines:
//!
//! | Engine | Hosts | A held call is answered by |
//! |---|---|---|
//! | `DurableWorker` (`adk-server`, feature `background`) | local and single-tenant servers | the approvals REST, through the approval service |
//! | `DefaultManagedAgentRuntime` (`adk-managed`) | hosted tenants | `user.tool_confirmation`, recorded in the runtime's approval store, or its approval service |
//!
//! `adk-managed` is Experimental, so this pack is too.
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_harness::HarnessMode;
//! use adk_harness::packs::managed::{HarnessDef, HarnessResolver, ShellPolicy};
//! use adk_managed::DefaultManagedAgentRuntime;
//! use adk_managed::resolver::DefaultModelResolver;
//! use adk_managed::types::{ManagedAgentDef, ModelRef};
//!
//! let def: ManagedAgentDef =
//!     HarnessDef::new("reviewer", ModelRef::Shorthand("gemini-3.7-flash".into()))
//!         .with_mode(HarnessMode::Plan)
//!         .with_shell(ShellPolicy::none())
//!         .into();
//! let workspaces = tempfile::tempdir()?;
//! let runtime = DefaultManagedAgentRuntime::new(
//!     Arc::new(DefaultModelResolver::new()),
//!     Arc::new(adk_session::InMemorySessionService::new()),
//! )
//! .with_agent_resolver(Arc::new(HarnessResolver::new(workspaces.path())));
//! # let _ = (def, runtime);
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! [`DefaultManagedAgentRuntime::with_agent_resolver`]: adk_managed::DefaultManagedAgentRuntime::with_agent_resolver

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use adk_core::{
    AdkError, Agent, AgentCapabilities, AgentInteractionMode, AgentTopology, AgentTransferDecision,
    AgentTransferRequest, ApprovalCall, ApprovalScope, Content, ErrorCategory, ErrorComponent,
    Event, EventStream, FunctionResponseData, InvocationContext, Llm, Part, Result, RunConfig,
    async_trait,
};
use adk_devtools::Workspace;
use adk_managed::types::{ManagedAgentDef, ModelRef, RuntimeError, ToolConfig};
use adk_managed::{AgentResolver, ManagedCustomTool, ModelResolver};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::builder::{HarnessAgentBuilder, Pack};
use crate::instructions::InstructionsConfig;
use crate::mode::HarnessMode;
use crate::{HarnessAgent, ShellIsolation};

/// The [`ManagedAgentDef::metadata`] key a [`HarnessDef`]'s harness block travels under
/// (spec 3.1 §7.18).
///
/// The block is JSON in a string value, so a store or service that keeps a
/// `ManagedAgentDef`, including the session records the managed runtime rebuilds sessions
/// from, keeps it unchanged.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::managed::HARNESS_DEF_METADATA_KEY, "adk.harness");
/// ```
pub const HARNESS_DEF_METADATA_KEY: &str = "adk.harness";

/// The error code of a harness definition the resolver cannot build (spec 3.1 §7.18).
///
/// # Example
///
/// ```rust
/// assert_eq!(
///     adk_harness::packs::managed::INVALID_HARNESS_DEF_CODE,
///     "harness.managed.invalid_def"
/// );
/// ```
pub const INVALID_HARNESS_DEF_CODE: &str = "harness.managed.invalid_def";

/// A managed agent definition that is a harness (spec 3.1 §7.18): the model reference,
/// system prompt, description and custom tools of [`ManagedAgentDef`], plus the harness
/// mode, shell policy, skills roots, members and packs.
///
/// Converted into a `ManagedAgentDef` (`ManagedAgentDef::from`), the harness fields travel
/// in its metadata under [`HARNESS_DEF_METADATA_KEY`], so the managed runtime stores and
/// rebuilds it like any other definition. Its own JSON form is the definition's
/// snake_case wire shape with the harness fields beside the definition's.
///
/// | Definition field | In a harness |
/// |---|---|
/// | `model` | resolved by the runtime's `ModelResolver` |
/// | `system` | added to the harness prompt |
/// | `description` | the agent description |
/// | `tools` | `custom` tools only; the client executes them and the turn parks for the result |
/// | `permission_policy`, `mcp_servers`, `skills` | refused: `mode` governs confirmations, MCP servers are configured on the resolver, skills come from `skills_roots` |
///
/// # Example
///
/// ```rust
/// use adk_harness::HarnessMode;
/// use adk_harness::packs::managed::{HarnessDef, MemberDef};
/// use adk_managed::types::{ManagedAgentDef, ModelRef};
///
/// let def = HarnessDef::new("builder", ModelRef::Shorthand("gpt-5.6-terra".into()))
///     .with_mode(HarnessMode::Ask)
///     .with_member(MemberDef::new("tester", "Runs the test suite.", "Run the tests and report."));
/// let wire: ManagedAgentDef = def.into();
/// let back = HarnessDef::from_agent_def(&wire)?.expect("a harness block");
/// assert_eq!(back.mode, HarnessMode::Ask);
/// assert_eq!(back.members[0].name, "tester");
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct HarnessDef {
    /// The definition the harness extends.
    #[serde(flatten)]
    pub agent: ManagedAgentDef,
    /// The mode of sessions with no mode record (default `ask`).
    #[serde(default)]
    pub mode: HarnessMode,
    /// Where `bash` runs (default the OS sandbox).
    #[serde(default)]
    pub shell: ShellPolicy,
    /// Extra skill directories, relative to the workspace root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills_roots: Vec<String>,
    /// Team members beside the default ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberDef>,
    /// Packs: `browser`, `computer_use`, `payments`, `voice`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packs: Vec<String>,
}

/// The harness fields of a [`HarnessDef`] as they travel in the definition's metadata.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct HarnessBlock {
    #[serde(default)]
    mode: HarnessMode,
    #[serde(default)]
    shell: ShellPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skills_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    members: Vec<MemberDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    packs: Vec<String>,
}

impl HarnessDef {
    /// A harness named `name` on `model`, in Ask mode with the shell in the OS sandbox
    /// (spec 3.1 §7.18).
    pub fn new(name: impl Into<String>, model: ModelRef) -> Self {
        Self::from_agent(ManagedAgentDef::new(name, model))
    }

    /// `agent` as a harness with the default harness fields.
    pub fn from_agent(agent: ManagedAgentDef) -> Self {
        Self {
            agent,
            mode: HarnessMode::default(),
            shell: ShellPolicy::default(),
            skills_roots: Vec::new(),
            members: Vec::new(),
            packs: Vec::new(),
        }
    }

    /// The harness `def` declares, or `None` when its metadata carries no harness block
    /// (spec 3.1 §7.18).
    ///
    /// The returned definition's metadata no longer holds the block.
    ///
    /// # Errors
    ///
    /// Returns [`INVALID_HARNESS_DEF_CODE`] when the block is not valid JSON, has an unknown
    /// key, or names an unknown mode or shell.
    pub fn from_agent_def(def: &ManagedAgentDef) -> Result<Option<Self>> {
        let Some(text) =
            def.metadata.as_ref().and_then(|metadata| metadata.get(HARNESS_DEF_METADATA_KEY))
        else {
            return Ok(None);
        };
        let block: HarnessBlock = serde_json::from_str(text).map_err(|error| {
            invalid(format!(
                "metadata `{HARNESS_DEF_METADATA_KEY}` of definition '{}' is not a harness \
                 block: {error}",
                def.name
            ))
        })?;
        let mut agent = def.clone();
        if let Some(metadata) = agent.metadata.as_mut() {
            metadata.remove(HARNESS_DEF_METADATA_KEY);
            if metadata.is_empty() {
                agent.metadata = None;
            }
        }
        Ok(Some(Self {
            agent,
            mode: block.mode,
            shell: block.shell,
            skills_roots: block.skills_roots,
            members: block.members,
            packs: block.packs,
        }))
    }

    /// Sets the mode of sessions with no mode record.
    pub fn with_mode(mut self, mode: HarnessMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets where `bash` runs.
    pub fn with_shell(mut self, shell: ShellPolicy) -> Self {
        self.shell = shell;
        self
    }

    /// Adds a skill directory, relative to the workspace root.
    pub fn with_skills_root(mut self, root: impl Into<String>) -> Self {
        self.skills_roots.push(root.into());
        self
    }

    /// Adds a team member.
    pub fn with_member(mut self, member: MemberDef) -> Self {
        self.members.push(member);
        self
    }

    /// Adds a pack by name.
    pub fn with_pack(mut self, pack: impl Into<String>) -> Self {
        self.packs.push(pack.into());
        self
    }

    /// Sets the text added to the harness prompt.
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.agent.system = Some(system.into());
        self
    }

    /// Adds a custom tool: the client executes it, and the turn parks until the client
    /// sends `user.custom_tool_result`.
    pub fn with_custom_tool(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        self.agent.tools.push(ToolConfig::Custom {
            name: name.into(),
            description: Some(description.into()),
            input_schema,
        });
        self
    }
}

impl From<HarnessDef> for ManagedAgentDef {
    /// The definition with the harness block in its metadata (spec 3.1 §7.18).
    fn from(def: HarnessDef) -> Self {
        let HarnessDef { mut agent, mode, shell, skills_roots, members, packs } = def;
        let block = HarnessBlock { mode, shell, skills_roots, members, packs };
        let text = serde_json::to_string(&block).expect("a harness block always serializes");
        agent
            .metadata
            .get_or_insert_with(BTreeMap::new)
            .insert(HARNESS_DEF_METADATA_KEY.to_string(), text);
        agent
    }
}

/// Where a managed harness runs `bash` (spec 3.1 §7.18, §7.9).
///
/// | `isolation` | Effect |
/// |---|---|
/// | `os_sandbox` (default) | Seatbelt or bubblewrap; with the host shell refused by the resolver, an unavailable sandbox fails the build |
/// | `host` | the host shell behind per-call approval; refused unless [`HarnessResolver::allow_host_shell`] |
/// | `none` | no `bash` |
///
/// # Example
///
/// ```rust
/// use adk_harness::packs::managed::{ShellKind, ShellPolicy};
///
/// let policy: ShellPolicy = serde_json::from_str(r#"{"isolation": "none"}"#).unwrap();
/// assert_eq!(policy, ShellPolicy::none());
/// assert_eq!(ShellPolicy::default().isolation, ShellKind::OsSandbox);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub struct ShellPolicy {
    /// Where `bash` runs.
    #[serde(default)]
    pub isolation: ShellKind,
    /// Fail the build, and each call, when the sandbox is unavailable.
    #[serde(default)]
    pub fail_if_unavailable: bool,
}

impl ShellPolicy {
    /// No `bash`.
    pub fn none() -> Self {
        Self { isolation: ShellKind::None, fail_if_unavailable: false }
    }

    /// The host shell behind per-call approval.
    pub fn host() -> Self {
        Self { isolation: ShellKind::Host, fail_if_unavailable: false }
    }

    /// The OS sandbox.
    pub fn os_sandbox() -> Self {
        Self { isolation: ShellKind::OsSandbox, fail_if_unavailable: false }
    }

    /// Sets whether an unavailable sandbox fails the build and each call.
    pub fn with_fail_if_unavailable(mut self, yes: bool) -> Self {
        self.fail_if_unavailable = yes;
        self
    }
}

/// The shell of a [`ShellPolicy`] (spec 3.1 §7.18).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ShellKind {
    /// No `bash`.
    None,
    /// The host shell behind per-call approval.
    Host,
    /// The OS sandbox.
    #[default]
    OsSandbox,
}

/// A local team member a [`HarnessDef`] declares (spec 3.1 §7.18, §7.10).
///
/// The member is an `LlmAgent` on the harness model, delegated to in an isolated context
/// under the harness's mode. ACP and A2A members name a process or an endpoint, so they
/// are configured by the operator through [`HarnessResolver::configure`], never by a
/// definition.
///
/// # Example
///
/// ```rust
/// use adk_harness::packs::managed::{MemberDef, MemberToolsDef};
///
/// let member = MemberDef::new("tester", "Runs the tests.", "Run cargo test and report.")
///     .with_tools(MemberToolsDef::Full);
/// assert_eq!(member.tools, MemberToolsDef::Full);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub struct MemberDef {
    /// Member name; a default member's name replaces that member.
    pub name: String,
    /// When to use it (model-facing).
    pub description: String,
    /// The member's instruction.
    pub instruction: String,
    /// The member's tools (default read-only).
    #[serde(default)]
    pub tools: MemberToolsDef,
    /// Most model turns (default 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    /// Delegation timeout in seconds (default 600); never unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl MemberDef {
    /// A read-only member.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        instruction: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            instruction: instruction.into(),
            tools: MemberToolsDef::ReadOnly,
            max_iterations: None,
            timeout_secs: None,
        }
    }

    /// Sets the member's tools.
    pub fn with_tools(mut self, tools: MemberToolsDef) -> Self {
        self.tools = tools;
        self
    }
}

/// The tools of a [`MemberDef`] (spec 3.1 §7.10).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MemberToolsDef {
    /// Read tools only.
    #[default]
    ReadOnly,
    /// Every harness tool a member may have.
    Full,
}

type WorkspaceFn = dyn Fn(&HarnessDef) -> Result<Workspace> + Send + Sync;
type ConfigureFn = dyn Fn(HarnessAgentBuilder, &HarnessDef) -> HarnessAgentBuilder + Send + Sync;

/// Builds harnesses for the managed runtime (spec 3.1 §7.18): an [`AgentResolver`] for
/// [`DefaultManagedAgentRuntime::with_agent_resolver`].
///
/// | Definition | Built as |
/// |---|---|
/// | carries a harness block ([`HarnessDef`]) | a harness, with the model from the runtime's `ModelResolver` |
/// | named like a harness added with [`with_harness`](Self::with_harness), no block | that harness value |
/// | anything else | left to the runtime's `build_agent` |
///
/// A definition comes from a tenant, so the resolver keeps the operator's limits: the host
/// shell is refused unless [`allow_host_shell`](Self::allow_host_shell), the operator's
/// `AGENTS.md` and home skills directory are not loaded, skills roots stay inside the
/// workspace, and [`configure`](Self::configure) applies the operator's stores, policy,
/// audit log and approval service after the definition, so it has the last word.
///
/// The runtime drives the harness with its own runner, so before each turn the resolved
/// agent settles what `Harness::send` settles (spec 3.1 §7.12, §9.2 I37): a held call
/// whose decision a stopped turn took and whose result it never saved is answered from
/// the action ledger when it ran, and held again — pending in the approval store,
/// submitted to the approval service, and reported to the client as `requires_action` —
/// when it never began.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::InMemoryActionLedger;
/// use adk_harness::packs::managed::HarnessResolver;
///
/// let workspaces = tempfile::tempdir()?;
/// let ledger = Arc::new(InMemoryActionLedger::new());
/// let resolver = HarnessResolver::new(workspaces.path())
///     .configure(move |builder, _def| builder.action_ledger(ledger.clone()));
/// # let _ = resolver;
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// [`DefaultManagedAgentRuntime::with_agent_resolver`]: adk_managed::DefaultManagedAgentRuntime::with_agent_resolver
#[derive(Clone)]
pub struct HarnessResolver {
    workspace: Arc<WorkspaceFn>,
    allow_host_shell: bool,
    configure: Option<Arc<ConfigureFn>>,
    harnesses: BTreeMap<String, Arc<dyn Agent>>,
}

impl fmt::Debug for HarnessResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessResolver")
            .field("allow_host_shell", &self.allow_host_shell)
            .field("configure", &self.configure.is_some())
            .field("harnesses", &self.harnesses.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl HarnessResolver {
    /// A resolver that gives each definition the workspace `<root>/<name>`, created on
    /// first use (spec 3.1 §7.18).
    ///
    /// Every session of a definition works in its workspace, and definitions with the same
    /// name share one; use [`with_workspace`](Self::with_workspace) for another layout.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            workspace: Arc::new(move |def: &HarnessDef| {
                let dir = root.join(workspace_dir_name(&def.agent.name)?);
                std::fs::create_dir_all(&dir).map_err(|error| {
                    AdkError::new(
                        ErrorComponent::Agent,
                        ErrorCategory::Internal,
                        "harness.managed.workspace",
                        format!("cannot create the workspace {}: {error}", dir.display()),
                    )
                })?;
                Ok(Workspace::new(dir).allow_bash(def.shell.isolation != ShellKind::None))
            }),
            allow_host_shell: false,
            configure: None,
            harnesses: BTreeMap::new(),
        }
    }

    /// Gives each definition the workspace `workspace` returns.
    pub fn with_workspace(
        mut self,
        workspace: impl Fn(&HarnessDef) -> Result<Workspace> + Send + Sync + 'static,
    ) -> Self {
        self.workspace = Arc::new(workspace);
        self
    }

    /// Lets definitions run `bash` on the host, behind per-call approval (default off;
    /// spec 3.1 §7.9, §7.18).
    pub fn allow_host_shell(mut self, yes: bool) -> Self {
        self.allow_host_shell = yes;
        self
    }

    /// Applies `configure` to every harness after its definition: durable mode, rule,
    /// approval and ledger stores, an operator policy, the audit log, an approval service,
    /// MCP servers, ACP or A2A members and pack configuration.
    pub fn configure(
        mut self,
        configure: impl Fn(HarnessAgentBuilder, &HarnessDef) -> HarnessAgentBuilder
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.configure = Some(Arc::new(configure));
        self
    }

    /// Runs `harness` itself for every definition named like it that carries no harness
    /// block: the same value an application serves through `Launcher`, `ServerBuilder` or
    /// `serve_agent_engine` (spec 3.1 H9). The definition's model and fields are not used.
    pub fn with_harness(mut self, harness: &HarnessAgent) -> Self {
        let same = HarnessAgent {
            root: harness.root.clone(),
            runtime: harness.runtime.clone(),
            info: harness.info.clone(),
        };
        self.harnesses.insert(harness.info().name.clone(), Arc::new(ManagedHarness::new(same)));
        self
    }

    /// Builds the harness `def` declares, on `model` (spec 3.1 §7.18).
    ///
    /// # Errors
    ///
    /// Returns [`INVALID_HARNESS_DEF_CODE`] for a definition field the harness does not
    /// take, an unknown pack, an invalid member or skills root, a host shell the resolver
    /// refuses, or a feature the build lacks; and the builder's error, such as
    /// `harness.sandbox_unavailable`.
    pub fn build(&self, def: &HarnessDef, model: Arc<dyn Llm>) -> Result<HarnessAgent> {
        self.builder(def, model)?.build()
    }

    /// The builder of the harness `def` declares, configured, before `build`.
    fn builder(&self, def: &HarnessDef, model: Arc<dyn Llm>) -> Result<HarnessAgentBuilder> {
        let agent = &def.agent;
        if agent.permission_policy.is_some() {
            return Err(invalid(
                "permission_policy: the harness mode governs which calls need confirmation; \
                 set `mode` instead",
            ));
        }
        if !agent.mcp_servers.is_empty() {
            return Err(invalid(
                "mcp_servers: MCP servers start processes or reach endpoints, so the operator \
                 configures them with HarnessResolver::configure",
            ));
        }
        if !agent.skills.is_empty() {
            return Err(invalid(
                "skills: skill references are not resolved for a harness; declare \
                 `skills_roots` instead",
            ));
        }
        let mut builder = HarnessAgent::builder(agent.name.clone())
            .model(model)
            .workspace((self.workspace)(def)?)
            .mode(def.mode)
            .project_instructions(InstructionsConfig {
                user_file: None,
                walk_up_to_git_root: false,
                ..InstructionsConfig::default()
            });
        if let Some(system) = &agent.system {
            builder = builder.instruction(system.clone());
        }
        if let Some(description) = &agent.description {
            builder = builder.description(description.clone());
        }
        for tool in &agent.tools {
            let ToolConfig::Custom { name, description, input_schema } = tool else {
                let kind = serde_json::to_value(tool)
                    .ok()
                    .and_then(|value| value.get("type").and_then(Value::as_str).map(String::from))
                    .unwrap_or_default();
                return Err(invalid(format!(
                    "tool `{kind}`: a harness has its own governed file, shell and web tools; \
                     a definition declares only `custom` tools"
                )));
            };
            builder = builder.tool(Arc::new(ManagedCustomTool::new(
                name.clone(),
                description.clone().unwrap_or_default(),
                input_schema.clone(),
            )));
        }
        builder = self.shell(builder, def.shell)?;
        builder = skills(builder, &def.skills_roots)?;
        builder = members(builder, &def.members)?;
        for name in &def.packs {
            builder = builder.pack(pack(name)?);
        }
        builder = builder.pack(Pack::Managed);
        if let Some(configure) = &self.configure {
            builder = configure(builder, def);
        }
        Ok(builder)
    }

    fn shell(
        &self,
        builder: HarnessAgentBuilder,
        shell: ShellPolicy,
    ) -> Result<HarnessAgentBuilder> {
        match shell.isolation {
            ShellKind::None => Ok(builder.without_shell()),
            ShellKind::Host if self.allow_host_shell => Ok(builder.shell(ShellIsolation::Host)),
            ShellKind::Host => Err(invalid(
                "shell `host`: this runtime does not run a definition's commands on its host; \
                 use `os_sandbox` or `none`, or enable it with HarnessResolver::allow_host_shell",
            )),
            #[cfg(feature = "sandbox")]
            ShellKind::OsSandbox => {
                let builder = builder.shell(ShellIsolation::OsSandbox(Default::default()));
                // A sandbox that falls back to the host would run the host shell this
                // resolver refuses.
                Ok(if shell.fail_if_unavailable || !self.allow_host_shell {
                    builder.fail_if_unavailable()
                } else {
                    builder
                })
            }
            #[cfg(not(feature = "sandbox"))]
            ShellKind::OsSandbox => {
                Err(invalid("shell `os_sandbox` needs adk-harness built with feature `sandbox`"))
            }
        }
    }
}

#[async_trait]
impl AgentResolver for HarnessResolver {
    async fn resolve(
        &self,
        def: &ManagedAgentDef,
        models: &dyn ModelResolver,
    ) -> std::result::Result<Option<Arc<dyn Agent>>, RuntimeError> {
        let declared = HarnessDef::from_agent_def(def).map_err(runtime_error)?;
        match (self.harnesses.get(&def.name), declared) {
            (Some(_), Some(_)) => Err(RuntimeError::invalid_request(format!(
                "definition '{}' names a harness added with HarnessResolver::with_harness and \
                 also declares a harness block; a definition of an added harness carries none",
                def.name
            ))),
            (Some(harness), None) => Ok(Some(harness.clone())),
            (None, None) => Ok(None),
            (None, Some(harness_def)) => {
                let model = models.resolve(&def.model).await.map_err(|error| {
                    RuntimeError::provider_error(format!("{:?}", def.model), error.to_string())
                })?;
                let harness = self.build(&harness_def, model).map_err(runtime_error)?;
                Ok(Some(Arc::new(ManagedHarness::new(harness))))
            }
        }
    }
}

/// One harness per partition of a definition on a tenant kernel (spec 3.2 §7.8, §7.13): the
/// definition's harness built with `HarnessAgentBuilder::partition` over the tenant's
/// partition table — the partition's mode, its class and tenant on every principal, its
/// memory scope — after the operator's [`configure`](HarnessResolver::configure), and the
/// coordinator's with `partition_coordinator`. A definition without a harness block
/// resolves to `None`.
#[cfg(feature = "kernel")]
#[async_trait]
impl adk_kernel::managed::PartitionAgentResolver for HarnessResolver {
    async fn resolve_partition(
        &self,
        def: &ManagedAgentDef,
        host: &adk_kernel::managed::PartitionHost,
        models: &dyn ModelResolver,
    ) -> std::result::Result<Option<Arc<dyn Agent>>, RuntimeError> {
        let Some(harness_def) = HarnessDef::from_agent_def(def).map_err(runtime_error)? else {
            return Ok(None);
        };
        let model = models.resolve(&def.model).await.map_err(|error| {
            RuntimeError::provider_error(format!("{:?}", def.model), error.to_string())
        })?;
        let mut builder = self
            .builder(&harness_def, model)
            .map_err(runtime_error)?
            .partition(host.partition.clone(), host.partitions.clone());
        if host.coordinator {
            builder = builder.partition_coordinator();
        }
        let harness = builder.build().map_err(runtime_error)?;
        Ok(Some(Arc::new(ManagedHarness::new(harness))))
    }
}

/// A harness as the managed runtime runs it: the harness, and before each turn the
/// settlement of held calls a stopped turn left behind (spec 3.1 §7.12).
struct ManagedHarness {
    harness: Arc<HarnessAgent>,
}

impl ManagedHarness {
    fn new(harness: HarnessAgent) -> Self {
        Self { harness: Arc::new(harness) }
    }

    /// The events that settle this session's held calls with neither a decision nor a
    /// pending entry: answers from the action ledger, and holds of calls that never began.
    async fn settle(&self, ctx: &dyn InvocationContext) -> Result<Vec<Event>> {
        let held = ctx.session().held_tool_calls(self.name(), ctx.branch());
        let config = ctx.run_config();
        let (false, Some(store)) = (held.is_empty(), config.approval_store.as_ref()) else {
            return Ok(Vec::new());
        };
        let (app, user, session) = (ctx.app_name(), ctx.user_id(), ctx.session_id());
        let scope = ApprovalScope::new(app, user, session);
        let pending: Vec<String> =
            store.pending(&scope).await?.into_iter().map(|entry| entry.fingerprint).collect();
        let event = || {
            let mut event = Event::new(ctx.invocation_id());
            event.author = self.name().to_string();
            event.branch = ctx.branch().to_string();
            event
        };
        let mut answers = Vec::new();
        let mut settled = Vec::new();
        for call in held {
            let Some(call_id) = call.id.as_deref() else {
                continue;
            };
            let fingerprint = call.request.fingerprint();
            if pending.contains(&fingerprint) || store.has_decision(&scope, &fingerprint).await? {
                continue;
            }
            let key = crate::ledger::held_call_key(app, user, session, call_id, &fingerprint);
            let record = match &config.action_ledger {
                Some(ledger) => ledger.get(&key).await?,
                None => None,
            };
            if let Some(record) = record {
                tracing::warn!(
                    tool.name = %call.name,
                    function_call.id = %call_id,
                    "held call ran without its result being saved; answering it from the action ledger"
                );
                answers.push(Part::FunctionResponse {
                    function_response: FunctionResponseData::new(
                        call.name.clone(),
                        crate::ledger::recorded_answer(&call.name, &record),
                    ),
                    id: call.id.clone(),
                    annotations: None,
                });
                continue;
            }
            tracing::warn!(
                tool.name = %call.name,
                function_call.id = %call_id,
                "held call lost its decision before it began; holding it again"
            );
            store.record_pending(&scope, &call.request).await?;
            if let Some(service) = &config.approval_service {
                service
                    .submit_call(ApprovalCall {
                        scope: scope.clone(),
                        request: call.request.clone(),
                        reason: "held again: the turn that took its decision stopped before the \
                                 call began"
                            .to_string(),
                        agent_name: self.name().to_string(),
                        invocation_id: ctx.invocation_id().to_string(),
                        run_id: config.durable_run_id.clone(),
                        acting: Some(ctx.acting_context()),
                    })
                    .await?;
            }
            // The session loop reports a hold to the client and parks the turn on it.
            let mut hold = event();
            hold.actions.tool_confirmation = Some(call.request.clone());
            settled.push(hold);
        }
        if !answers.is_empty() {
            let mut answer = event();
            answer.llm_response.content =
                Some(Content { role: "function".to_string(), parts: answers });
            settled.insert(0, answer);
        }
        Ok(settled)
    }
}

#[async_trait]
impl Agent for ManagedHarness {
    fn name(&self) -> &str {
        self.harness.name()
    }

    fn description(&self) -> &str {
        self.harness.description()
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        self.harness.sub_agents()
    }

    fn interaction_mode(&self) -> AgentInteractionMode {
        self.harness.interaction_mode()
    }

    fn supports_agent_transfer(&self) -> bool {
        self.harness.supports_agent_transfer()
    }

    fn capabilities(&self) -> AgentCapabilities {
        self.harness.capabilities()
    }

    fn topology(&self) -> Option<AgentTopology> {
        self.harness.topology()
    }

    fn configure_run(&self, agent_name: &str, config: &mut RunConfig) {
        self.harness.configure_run(agent_name, config);
    }

    fn transfer_targets_for(&self, agent_name: &str) -> Option<Vec<String>> {
        self.harness.transfer_targets_for(agent_name)
    }

    fn strict_transfer_policy(&self) -> bool {
        self.harness.strict_transfer_policy()
    }

    async fn govern_transfer(
        &self,
        request: &AgentTransferRequest,
    ) -> Result<AgentTransferDecision> {
        self.harness.govern_transfer(request).await
    }

    fn allows_transfer_to_parent(&self) -> bool {
        self.harness.allows_transfer_to_parent()
    }

    fn allows_transfer_to_peers(&self) -> bool {
        self.harness.allows_transfer_to_peers()
    }

    async fn close(&self) {
        self.harness.close().await;
    }

    async fn screen_user_input(&self, content: &Content) -> Result<Option<Content>> {
        self.harness.screen_user_input(content).await
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let settled = self.settle(ctx.as_ref()).await?;
        if settled.is_empty() {
            return self.harness.run(ctx).await;
        }
        // The runner adds each settling event to the session before the harness's turn
        // starts, so the turn sees the calls answered or held again.
        let harness = self.harness.clone();
        Ok(Box::pin(async_stream::try_stream! {
            for event in settled {
                yield event;
            }
            let mut events = harness.run(ctx).await?;
            while let Some(event) = events.next().await {
                yield event?;
            }
        }))
    }
}

fn invalid(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::InvalidInput,
        INVALID_HARNESS_DEF_CODE,
        message,
    )
}

fn runtime_error(error: AdkError) -> RuntimeError {
    match error.category {
        ErrorCategory::InvalidInput | ErrorCategory::Forbidden | ErrorCategory::NotFound => {
            RuntimeError::invalid_request(error.to_string())
        }
        _ => RuntimeError::internal(error.to_string()),
    }
}

/// The directory name of a definition's default workspace.
fn workspace_dir_name(name: &str) -> Result<String> {
    let dir: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if dir.trim_matches('_').is_empty() {
        return Err(invalid(format!(
            "definition name '{name}' has no letter or digit to name its workspace"
        )));
    }
    Ok(dir)
}

fn pack(name: &str) -> Result<Pack> {
    match name {
        "browser" => Ok(Pack::Browser),
        "computer_use" => Ok(Pack::ComputerUse),
        "payments" => Ok(Pack::Payments),
        "voice" => Ok(Pack::Voice),
        "managed" => Err(invalid(
            "pack `managed`: the managed form is how this definition runs, and the resolver \
             marks every harness it builds with it; a definition does not add it",
        )),
        other => Err(invalid(format!(
            "unknown pack `{other}`; use browser, computer_use, payments or voice"
        ))),
    }
}

/// A skills root inside the workspace: relative, with no `..` or root component.
fn skills_root(root: &str) -> Result<PathBuf> {
    let path = Path::new(root);
    let inside = !root.is_empty()
        && path.components().all(|component| matches!(component, Component::Normal(_)));
    if inside {
        Ok(path.to_path_buf())
    } else {
        Err(invalid(format!(
            "skills root `{root}` must be a relative path inside the workspace, with no `..`"
        )))
    }
}

#[cfg(feature = "skills")]
fn skills(builder: HarnessAgentBuilder, roots: &[String]) -> Result<HarnessAgentBuilder> {
    // The workspace's own `.adk/skills` stays; the operator's home skills do not load.
    let mut extra_roots = vec![PathBuf::from(".adk/skills")];
    for root in roots {
        extra_roots.push(skills_root(root)?);
    }
    Ok(builder.skills(crate::skills::SkillsConfig {
        extra_roots,
        ..crate::skills::SkillsConfig::default()
    }))
}

#[cfg(not(feature = "skills"))]
fn skills(builder: HarnessAgentBuilder, roots: &[String]) -> Result<HarnessAgentBuilder> {
    for root in roots {
        skills_root(root)?;
    }
    if roots.is_empty() {
        Ok(builder)
    } else {
        Err(invalid("skills_roots needs adk-harness built with feature `skills`"))
    }
}

#[cfg(feature = "team")]
fn members(mut builder: HarnessAgentBuilder, members: &[MemberDef]) -> Result<HarnessAgentBuilder> {
    use crate::team::{MEMBER_MAX_ITERATIONS, MEMBER_TIMEOUT, MemberKind, MemberSpec, MemberTools};

    let mut names = std::collections::BTreeSet::new();
    for member in members {
        let blank = [&member.name, &member.description, &member.instruction]
            .iter()
            .any(|text| text.trim().is_empty());
        if blank || member.max_iterations == Some(0) || member.timeout_secs == Some(0) {
            return Err(invalid(format!(
                "member '{}' needs a name, a description, an instruction, and a non-zero \
                 max_iterations and timeout_secs",
                member.name
            )));
        }
        if !names.insert(member.name.as_str()) {
            return Err(invalid(format!("member '{}' is declared twice", member.name)));
        }
        builder = builder.member(MemberSpec {
            name: member.name.clone(),
            description: member.description.clone(),
            kind: MemberKind::Local { instruction: member.instruction.clone(), model: None },
            tools: match member.tools {
                MemberToolsDef::ReadOnly => MemberTools::ReadOnly,
                MemberToolsDef::Full => MemberTools::Full,
            },
            skills: false,
            max_iterations: member.max_iterations.unwrap_or(MEMBER_MAX_ITERATIONS),
            timeout: member
                .timeout_secs
                .map(std::time::Duration::from_secs)
                .unwrap_or(MEMBER_TIMEOUT),
            output_schema: None,
        });
    }
    Ok(builder)
}

#[cfg(not(feature = "team"))]
fn members(builder: HarnessAgentBuilder, members: &[MemberDef]) -> Result<HarnessAgentBuilder> {
    if members.is_empty() {
        Ok(builder)
    } else {
        Err(invalid("members need adk-harness built with feature `team`"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model() -> ModelRef {
        ModelRef::Shorthand("gemini-3.7-flash".into())
    }

    #[test]
    fn the_harness_block_round_trips_through_a_definition_and_its_json() {
        let def = HarnessDef::new("h", model())
            .with_mode(HarnessMode::Plan)
            .with_shell(ShellPolicy::host())
            .with_skills_root("skills")
            .with_member(MemberDef::new("t", "d", "i"))
            .with_pack("browser")
            .with_system("Be brief.")
            .with_custom_tool("lookup", "Looks up.", json!({"type": "object"}));
        let mut wire: ManagedAgentDef = def.into();
        wire.metadata.get_or_insert_with(BTreeMap::new).insert("team".into(), "a".into());
        // The definition crosses a store as JSON, as a session record does.
        let wire: ManagedAgentDef =
            serde_json::from_value(serde_json::to_value(&wire).unwrap()).unwrap();
        let back = HarnessDef::from_agent_def(&wire).unwrap().unwrap();
        assert_eq!(
            (back.mode, back.shell, back.skills_roots.clone(), back.packs.clone()),
            (
                HarnessMode::Plan,
                ShellPolicy::host(),
                vec!["skills".to_string()],
                vec!["browser".to_string()]
            )
        );
        assert_eq!(back.members, [MemberDef::new("t", "d", "i")]);
        assert_eq!(back.agent.system.as_deref(), Some("Be brief."));
        assert_eq!(
            back.agent.metadata,
            Some(BTreeMap::from([("team".to_string(), "a".to_string())])),
            "the block leaves the metadata; the caller's keys stay"
        );
    }

    #[test]
    fn a_definition_without_a_block_is_not_a_harness() {
        let def = ManagedAgentDef::new("plain", model());
        assert!(HarnessDef::from_agent_def(&def).unwrap().is_none());
    }

    #[test]
    fn a_malformed_or_unknown_block_is_refused() {
        for block in [
            "not json",
            r#"{"mode": "yolo"}"#,
            r#"{"mode": "auto", "auto_allow_sandboxed": true}"#,
            r#"{"shell": {"isolation": "container"}}"#,
            r#"{"members": [{"name": "m", "description": "d", "instruction": "i", "acp": "codex"}]}"#,
        ] {
            let def = ManagedAgentDef::new("h", model()).with_metadata(BTreeMap::from([(
                HARNESS_DEF_METADATA_KEY.to_string(),
                block.to_string(),
            )]));
            let error = HarnessDef::from_agent_def(&def).unwrap_err();
            assert_eq!(error.code, INVALID_HARNESS_DEF_CODE, "{block}");
        }
    }

    #[test]
    fn its_own_json_is_the_definition_with_the_harness_fields_beside_it() {
        let def: HarnessDef = serde_json::from_value(json!({
            "name": "h",
            "model": "gpt-5.6-terra",
            "system": "Be brief.",
            "mode": "read_only",
            "shell": {"isolation": "none"},
        }))
        .unwrap();
        assert_eq!((def.mode, def.shell), (HarnessMode::ReadOnly, ShellPolicy::none()));
        assert_eq!(def.agent.name, "h");
        let defaults: HarnessDef =
            serde_json::from_value(json!({"name": "h", "model": "gpt-5.6-terra"})).unwrap();
        assert_eq!((defaults.mode, defaults.shell), (HarnessMode::Ask, ShellPolicy::os_sandbox()));
    }

    #[test]
    fn skills_roots_stay_inside_the_workspace() {
        assert!(skills_root("skills/team").is_ok());
        for root in ["", "/etc", "../up", "a/../../b", "./a"] {
            assert!(skills_root(root).is_err(), "{root}");
        }
    }

    #[test]
    fn workspace_names_cannot_leave_the_root() {
        assert_eq!(workspace_dir_name("my agent/../x").unwrap(), "my_agent____x");
        assert!(workspace_dir_name("..").is_err());
        assert!(workspace_dir_name("").is_err());
    }

    #[test]
    fn packs_are_named_and_managed_is_not_one() {
        assert_eq!(pack("payments").unwrap(), Pack::Payments);
        assert_eq!(pack("managed").unwrap_err().code, INVALID_HARNESS_DEF_CODE);
        assert_eq!(pack("rocket").unwrap_err().code, INVALID_HARNESS_DEF_CODE);
    }
}
