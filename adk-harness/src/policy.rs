//! `HarnessPolicy`: modes, tool classes and standing rules as one [`ToolPolicy`]
//! (spec 3.1 §7.4, §7.5, §9.1, §9.2).
//!
//! Modes and standing rules are a policy, never a callback: the policy runs on every
//! path before a call can be held, while a callback that answers a call skips the
//! governed path (spec 3.1 §6 D-2).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::{
    ActingContext, BUILTIN_TOOL_ORIGIN, PolicyCall, PolicyDecision, STATIC_TOOL_ORIGIN,
    ToolConfirmationRequest, ToolEffect, ToolOrigin, ToolPolicy, ToolPolicyRequest, async_trait,
    tool_call_fingerprint,
};
use adk_devtools::Workspace;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::approvals::StandingRuleStore;
use crate::approvals::matcher::{
    RuleCall, RuleEvaluation, RuleOutcome, evaluate_standing_rules, is_protected_path,
    is_protected_target,
};
use crate::audit::HarnessAudit;
use crate::mode::{HarnessMode, ModeStore};
use crate::shell::{IsolationStatus, ShellLedger};

/// `policy_ref` of the harness policy; audit records cite it as `tool_policy:adk-harness` (spec 3.1 §7.3).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::HARNESS_POLICY_REF, "adk-harness");
/// ```
pub const HARNESS_POLICY_REF: &str = "adk-harness";

/// Names of the tools the harness itself publishes.
pub(crate) const HARNESS_TOOL_NAMES: &[&str] = &[
    "read_file",
    "glob",
    "grep",
    "write_file",
    "edit_file",
    "bash",
    "write_todos",
    "update_todo",
    "exit_plan_mode",
    "list_skills",
    "load_skill",
    "load_skill_resource",
    "search_skills",
    "memory_read",
    "memory_write",
    "memory_delete",
    "memory_search",
    "memory_remember",
    "web_fetch",
    "web_search",
    "mcp_list_resources",
    "mcp_read_resource",
];

/// Run policies a harness layer composed, each with the harness it belongs to.
static COMPOSED_LAYERS: Mutex<Vec<(usize, Weak<dyn ToolPolicy>)>> = Mutex::new(Vec::new());

fn address(policy: &dyn ToolPolicy) -> *const () {
    std::ptr::from_ref(policy).cast::<()>()
}

/// Records `policy` as the run policy the harness `owner` composed.
pub(crate) fn register_composed_layer(owner: usize, policy: &Arc<dyn ToolPolicy>) {
    let mut layers = COMPOSED_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
    layers.retain(|(_, layer)| layer.strong_count() > 0);
    layers.push((owner, Arc::downgrade(policy)));
}

/// The harness whose layer composed `policy`, if one did.
pub(crate) fn composed_layer_owner(policy: &dyn ToolPolicy) -> Option<usize> {
    let target = address(policy);
    let layers = COMPOSED_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
    layers
        .iter()
        .find(|(_, layer)| layer.strong_count() > 0 && layer.as_ptr().cast::<()>() == target)
        .map(|(owner, _)| *owner)
}

/// Whether `policy` is a run policy that a harness layer composed (spec 3.1 §7.3, §9.2 I17).
///
/// The check is bound to the policy instance the layer installed, never to its name: a
/// policy that merely calls itself `adk-harness` is not the harness, so `bash`,
/// `exit_plan_mode` and a nested harness's run refuse it.
///
/// # Example
///
/// ```rust
/// use adk_core::{DeclarativePolicy, StrictestPolicy};
/// use adk_harness::{HarnessAgent, InstructionsConfig};
/// use adk_model::{ScriptedLlm, ScriptedResponse};
/// use std::sync::Arc;
///
/// let named = DeclarativePolicy::builder().name("adk-harness").build();
/// assert!(!adk_harness::has_harness_layer(&named));
/// let composed = StrictestPolicy::new(vec![Arc::new(named)]);
/// assert!(!adk_harness::has_harness_layer(&composed));
///
/// let dir = tempfile::tempdir().unwrap();
/// let harness = HarnessAgent::builder("harness")
///     .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
///     .workspace(adk_devtools::Workspace::new(dir.path()))
///     .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
///     .build()
///     .unwrap();
/// let config = harness.run_config();
/// assert!(adk_harness::has_harness_layer(config.tool_policy.as_deref().unwrap()));
/// ```
pub fn has_harness_layer(policy: &dyn ToolPolicy) -> bool {
    composed_layer_owner(policy).is_some()
}

/// Where a shell call runs, as the policy classes it (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// use adk_harness::Isolation;
///
/// assert_eq!(serde_json::to_value(Isolation::OsSandbox).unwrap(), "os_sandbox");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// An active OS sandbox.
    OsSandbox,
    /// A container.
    Container,
    /// The host, including a degraded sandbox.
    Host,
}

/// A call's harness class (spec 3.1 §7.4).
///
/// A call's class comes from (`origin.source`, name) in the harness registry, never from
/// [`ToolEffect`] or MCP hints alone (spec 3.1 §9.2 I12).
///
/// # Example
///
/// ```rust
/// use adk_core::ToolEffect;
/// use adk_harness::ToolKind;
///
/// assert!(ToolKind::Edit.rule_eligible());
/// assert!(!ToolKind::PlanExit.rule_eligible());
/// let mcp = ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: false };
/// assert!(mcp.rule_eligible());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolKind {
    /// `read_file`, `glob`, `grep`, `list_skills`, `load_skill`, `load_skill_resource`,
    /// `search_skills`, `memory_read`, `memory_search`, `mcp_list_resources`,
    /// `mcp_read_resource` (spec 3.1 §7.21).
    Read,
    /// `write_todos`, `update_todo`.
    Todo,
    /// `exit_plan_mode`.
    PlanExit,
    /// `write_file`, `edit_file`; a protected path is the non-waivable variant.
    Edit,
    /// `bash`.
    Shell(Isolation),
    /// `bash` with `unsandboxed: true`.
    ShellEscalation,
    /// Team member tools.
    Delegate,
    /// `memory_write`, `memory_delete`.
    MemoryWrite,
    /// `memory_remember`: assumed long-term facts only (spec 3.1 §7.15.2).
    MemoryPropose,
    /// `web_fetch`, `web_search`: whether the URL's host is in the allowlist
    /// (spec 3.1 §7.20).
    Web {
        /// Whether the call's host is in `WebConfig::allowed_domains`.
        allowlisted: bool,
    },
    /// Payments pack tools; never waivable.
    Payment,
    /// `computer_use_execute`: a desktop action through the computer-use pack; never
    /// waivable (spec 3.1 §7.24).
    ComputerUse,
    /// Any other tool: MCP, browser, extras, skill-activated.
    External {
        /// The effect the tool declares.
        effect: ToolEffect,
        /// Whether the publisher is trusted to declare its effect.
        trusted_origin: bool,
    },
}

impl ToolKind {
    /// Whether any standing rule may cover the class (spec 3.1 §7.5).
    pub fn rule_eligible(&self) -> bool {
        !matches!(self, Self::PlanExit | Self::Payment | Self::MemoryWrite | Self::ComputerUse)
    }

    /// Whether the class is the shell or its escalation.
    pub fn is_shell(&self) -> bool {
        matches!(self, Self::Shell(_) | Self::ShellEscalation)
    }
}

/// Registry of the harness's own tool classes (spec 3.1 §7.4).
///
/// A call is a harness class only if the agent is one of the harness's own agents, the
/// name is registered, and the origin is [`STATIC_TOOL_ORIGIN`]. A caller of
/// `ToolPolicy::evaluate` without the call's origin gets a class resting on the agent's
/// tool names, which an `LlmAgent` keeps unique across its tools and toolsets.
///
/// # Example
///
/// ```rust
/// use adk_core::{ToolEffect, ToolOrigin};
/// use adk_harness::{Isolation, ToolClassRegistry, ToolKind};
/// use serde_json::json;
///
/// let registry = ToolClassRegistry::harness_defaults("harness");
/// let mcp = ToolOrigin::new("mcp:fs", &json!({ "name": "read_file" }), None);
/// let kind = registry.classify("harness", "read_file", Some(&mcp), &json!({}), ToolEffect::ReadOnly, Isolation::Host, false);
/// assert_eq!(kind, ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: false });
/// let own = registry.classify("harness", "read_file", None, &json!({}), ToolEffect::ReadOnly, Isolation::Host, false);
/// assert_eq!(own, ToolKind::Read);
/// ```
#[derive(Clone, Debug, Default)]
pub struct ToolClassRegistry {
    agents: BTreeSet<String>,
    classes: BTreeMap<String, ToolKind>,
    /// Hosts the web tools reach without approval (spec 3.1 §7.20).
    web_allowlist: Vec<String>,
    /// Classes of tools that publishers the harness does not own declare through a
    /// trusted manifest (spec 3.2 §7.7).
    origin_classes: Vec<Arc<dyn OriginClasses>>,
}

/// Classes tools of a publisher the harness does not own, from a declaration the user
/// approved rather than from the tool's own hints (spec 3.2 §7.7, 3.1 I12).
///
/// The kernel's drivers are classed this way: each driver tool's class comes from the
/// manifest the user approved at mount (`adk-harness`'s `kernel` feature).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{ToolEffect, ToolOrigin};
/// use adk_harness::{Isolation, OriginClasses, ToolClassRegistry, ToolKind};
/// use serde_json::json;
///
/// #[derive(Debug)]
/// struct Approved;
///
/// impl OriginClasses for Approved {
///     fn class_of(&self, source: &str, _tool: &str) -> Option<ToolKind> {
///         (source == "driver:notes")
///             .then_some(ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: true })
///     }
/// }
///
/// let registry = ToolClassRegistry::harness_defaults("harness").with_origin_classes(Arc::new(Approved));
/// let origin = ToolOrigin::new("driver:notes", &json!({ "name": "read" }), None);
/// let kind = registry.classify("harness", "read", Some(&origin), &json!({}), ToolEffect::NonIdempotent, Isolation::Host, false);
/// assert_eq!(kind, ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: true });
/// ```
pub trait OriginClasses: std::fmt::Debug + Send + Sync {
    /// The class of a call to `tool` that `source` publishes, or `None` when `source` is
    /// not one this classes.
    fn class_of(&self, source: &str, tool: &str) -> Option<ToolKind>;
}

impl ToolClassRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The harness tools for the agent named `agent`.
    pub fn harness_defaults(agent: &str) -> Self {
        let mut registry = Self::new().with_agent(agent);
        for name in [
            "read_file",
            "glob",
            "grep",
            "list_skills",
            "load_skill",
            "load_skill_resource",
            "search_skills",
        ] {
            registry.register(name, ToolKind::Read);
        }
        registry.register("write_todos", ToolKind::Todo);
        registry.register("update_todo", ToolKind::Todo);
        // Allowed in every mode: proposing is free, running is governed (spec 3.2 §7.5).
        #[cfg(feature = "kernel")]
        registry.register(crate::kernel::PROPOSE_FOLLOWUP, ToolKind::Todo);
        registry.register("exit_plan_mode", ToolKind::PlanExit);
        registry.register("write_file", ToolKind::Edit);
        registry.register("edit_file", ToolKind::Edit);
        registry.register("bash", ToolKind::Shell(Isolation::Host));
        registry.register("memory_read", ToolKind::Read);
        registry.register("memory_write", ToolKind::MemoryWrite);
        registry.register("memory_delete", ToolKind::MemoryWrite);
        registry.register("memory_search", ToolKind::Read);
        registry.register("memory_remember", ToolKind::MemoryPropose);
        // The allowlist decision is made per call from the URL's host (spec 3.1 §7.20).
        registry.register("web_fetch", ToolKind::Web { allowlisted: false });
        registry.register("web_search", ToolKind::Web { allowlisted: false });
        registry.register("mcp_list_resources", ToolKind::Read);
        registry.register("mcp_read_resource", ToolKind::Read);
        registry
    }

    /// Sets the hosts `web_fetch` and `web_search` reach without approval
    /// (spec 3.1 §7.20): exact hosts or `*.example.com`, the `DomainIn` grammar.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::ToolEffect;
    /// use adk_harness::{Isolation, ToolClassRegistry, ToolKind};
    /// use serde_json::json;
    ///
    /// let registry = ToolClassRegistry::harness_defaults("harness")
    ///     .with_web_allowlist(["docs.rs", "*.rust-lang.org"]);
    /// let classify = |args| registry.classify("harness", "web_fetch", None, &args, ToolEffect::ReadOnly, Isolation::Host, false);
    /// assert_eq!(classify(json!({ "url": "https://docs.rs/x" })), ToolKind::Web { allowlisted: true });
    /// assert_eq!(classify(json!({ "url": "https://rust-lang.org/" })), ToolKind::Web { allowlisted: false });
    /// assert_eq!(classify(json!({ "url": "https://user@docs.rs/" })), ToolKind::Web { allowlisted: false });
    /// ```
    #[must_use]
    pub fn with_web_allowlist(
        mut self,
        domains: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.web_allowlist = domains.into_iter().map(|d| d.into().to_ascii_lowercase()).collect();
        self
    }

    /// Classes the tools of publishers `classes` knows by their approved declarations
    /// (spec 3.2 §7.7); consulted for every origin other than the harness's own.
    #[must_use]
    pub fn with_origin_classes(mut self, classes: Arc<dyn OriginClasses>) -> Self {
        self.origin_classes.push(classes);
        self
    }

    /// The hosts the web tools reach without approval.
    pub fn web_allowlist(&self) -> &[String] {
        &self.web_allowlist
    }

    /// Whether the web call `tool` with `args` names an allowlisted host: `web_search` by
    /// its `site`, every other `Web` tool — `web_fetch`, the browser pack's navigation
    /// (spec 3.1 §7.24) — by its `url` (absent: not allowlisted).
    fn web_allowlisted(&self, tool: &str, args: &Value) -> bool {
        match tool {
            "web_search" => args.get("site").and_then(Value::as_str).is_some_and(|site| {
                crate::web::ParsedUrl::parse(&format!("https://{site}/")).is_ok_and(|parsed| {
                    crate::web::host_allowlisted(&parsed.host, &self.web_allowlist)
                })
            }),
            _ => args
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|url| crate::web::url_allowlisted(url, &self.web_allowlist)),
        }
    }

    /// Adds one of the harness's own agents.
    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agents.insert(agent.into());
        self
    }

    /// Registers `name` as `kind`.
    pub fn register(&mut self, name: impl Into<String>, kind: ToolKind) {
        self.classes.insert(name.into(), kind);
    }

    /// Removes `name`.
    pub fn unregister(&mut self, name: &str) {
        self.classes.remove(name);
    }

    /// Whether `name` is a registered harness tool.
    pub fn is_registered(&self, name: &str) -> bool {
        self.classes.contains_key(name)
    }

    /// The class of a call by `agent` to `tool` with `origin` and `args`.
    #[allow(clippy::too_many_arguments)]
    pub fn classify(
        &self,
        agent: &str,
        tool: &str,
        origin: Option<&ToolOrigin>,
        args: &Value,
        effect: ToolEffect,
        isolation: Isolation,
        trust_mcp_read_only_hints: bool,
    ) -> ToolKind {
        let static_origin = origin.is_none_or(|origin| origin.source == STATIC_TOOL_ORIGIN);
        if static_origin && self.agents.contains(agent) {
            match self.classes.get(tool) {
                Some(ToolKind::Shell(_)) => {
                    return if args.get("unsandboxed").and_then(Value::as_bool) == Some(true) {
                        ToolKind::ShellEscalation
                    } else {
                        ToolKind::Shell(isolation)
                    };
                }
                Some(ToolKind::Web { .. }) => {
                    return ToolKind::Web { allowlisted: self.web_allowlisted(tool, args) };
                }
                Some(kind) => return *kind,
                None => {}
            }
        }
        if let Some(origin) = origin.filter(|origin| origin.source != STATIC_TOOL_ORIGIN)
            && let Some(kind) = self
                .origin_classes
                .iter()
                .find_map(|classes| classes.class_of(&origin.source, tool))
        {
            return kind;
        }
        let trusted_origin = match origin {
            Some(origin) if origin.source == STATIC_TOOL_ORIGIN => true,
            Some(origin) if origin.source.starts_with("mcp:") => trust_mcp_read_only_hints,
            _ => false,
        };
        ToolKind::External { effect, trusted_origin }
    }
}

/// Rows of [`MODE_TABLE`] (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// use adk_harness::{Isolation, TableRow, ToolKind};
///
/// assert_eq!(TableRow::of(&ToolKind::Edit, true), TableRow::EditProtected);
/// assert_eq!(TableRow::of(&ToolKind::Shell(Isolation::Host), false), TableRow::ShellHost);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TableRow {
    /// Read, Todo, Delegate.
    ReadTodoDelegate,
    /// `exit_plan_mode`.
    PlanExit,
    /// Edit of a non-protected path.
    Edit,
    /// Edit of a protected path.
    EditProtected,
    /// Shell in an active OS sandbox.
    ShellOsSandbox,
    /// Shell in a container.
    ShellContainer,
    /// Shell on the host, including a degraded sandbox.
    ShellHost,
    /// Unsandboxed retry.
    ShellEscalation,
    /// Memory writes.
    MemoryWrite,
    /// Assumed long-term facts (spec 3.1 §7.15.2).
    MemoryPropose,
    /// A web call whose host is in the allowlist (spec 3.1 §7.20).
    WebAllowlisted,
    /// A web call to any other host.
    WebOther,
    /// Payments.
    Payment,
    /// A desktop action through the computer-use pack (spec 3.1 §7.24).
    ComputerUse,
    /// A read-only tool from a trusted origin.
    ExternalReadOnlyTrusted,
    /// Any other tool.
    ExternalOther,
}

impl TableRow {
    /// Every row, in table order.
    pub const ALL: [TableRow; 16] = [
        Self::ReadTodoDelegate,
        Self::PlanExit,
        Self::Edit,
        Self::EditProtected,
        Self::ShellOsSandbox,
        Self::ShellContainer,
        Self::ShellHost,
        Self::ShellEscalation,
        Self::MemoryWrite,
        Self::MemoryPropose,
        Self::WebAllowlisted,
        Self::WebOther,
        Self::Payment,
        Self::ComputerUse,
        Self::ExternalReadOnlyTrusted,
        Self::ExternalOther,
    ];

    /// The row of a call of `kind`; `protected_path` selects the protected edit row.
    pub fn of(kind: &ToolKind, protected_path: bool) -> Self {
        match kind {
            ToolKind::Read | ToolKind::Todo | ToolKind::Delegate => Self::ReadTodoDelegate,
            ToolKind::PlanExit => Self::PlanExit,
            ToolKind::Edit if protected_path => Self::EditProtected,
            ToolKind::Edit => Self::Edit,
            ToolKind::Shell(Isolation::OsSandbox) => Self::ShellOsSandbox,
            ToolKind::Shell(Isolation::Container) => Self::ShellContainer,
            ToolKind::Shell(Isolation::Host) => Self::ShellHost,
            ToolKind::ShellEscalation => Self::ShellEscalation,
            ToolKind::MemoryWrite => Self::MemoryWrite,
            ToolKind::MemoryPropose => Self::MemoryPropose,
            ToolKind::Web { allowlisted: true } => Self::WebAllowlisted,
            ToolKind::Web { allowlisted: false } => Self::WebOther,
            ToolKind::Payment => Self::Payment,
            ToolKind::ComputerUse => Self::ComputerUse,
            ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: true } => {
                Self::ExternalReadOnlyTrusted
            }
            ToolKind::External { .. } => Self::ExternalOther,
        }
    }

    /// Whether every cell of the row is Deny or a non-waivable approval: no standing rule
    /// or `tool_approvals` entry ever applies to the row.
    pub fn is_non_waivable(self) -> bool {
        matches!(
            self,
            Self::PlanExit
                | Self::EditProtected
                | Self::ShellEscalation
                | Self::MemoryWrite
                | Self::Payment
                | Self::ComputerUse
        )
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|row| *row == self).unwrap_or(Self::ALL.len() - 1)
    }
}

/// One cell of [`MODE_TABLE`] (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// use adk_harness::{ModeCell, PolicySwitches, TableDecision};
///
/// let switches = PolicySwitches::default();
/// assert_eq!(ModeCell::AskOrHostShellInAuto.resolve(&switches), TableDecision::RequireApproval { non_waivable: false });
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeCell {
    /// Allow.
    Allow,
    /// Deny.
    Deny,
    /// RequireApproval; a standing rule may allow.
    Ask,
    /// RequireApproval, non-waivable: no standing rule or `tool_approvals` entry applies.
    AskNonWaivable,
    /// Allow with a read-only workspace mount and no network, only under an active
    /// enforcer; Deny otherwise.
    AllowWithActiveEnforcer,
    /// RequireApproval; Allow when `auto_allow_sandboxed` is on.
    AskOrAutoAllowSandboxed,
    /// Allow once protected paths are read-only inside the sandbox (S3); RequireApproval
    /// until then.
    AllowWithS3,
    /// RequireApproval; Allow with `allow_host_shell_in_auto`.
    AskOrHostShellInAuto,
}

/// The switches the conditional cells of [`MODE_TABLE`] read (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// let switches = adk_harness::PolicySwitches::default();
/// assert!(!switches.auto_allow_sandboxed);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PolicySwitches {
    /// `auto_allow_sandboxed` is on (needs S3).
    pub auto_allow_sandboxed: bool,
    /// `allow_host_shell_in_auto` is on.
    pub allow_host_shell_in_auto: bool,
    /// An OS sandbox enforcer is active for the shell.
    pub enforcer_active: bool,
    /// Protected paths are read-only inside the sandbox (adk-sandbox S3).
    pub sandbox_protects_paths: bool,
}

/// What a [`ModeCell`] resolves to (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// use adk_harness::TableDecision;
///
/// assert!(TableDecision::RequireApproval { non_waivable: true }.is_non_waivable());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableDecision {
    /// Allow.
    Allow,
    /// Deny.
    Deny,
    /// RequireApproval.
    RequireApproval {
        /// No standing rule or `tool_approvals` entry applies.
        non_waivable: bool,
    },
}

impl TableDecision {
    /// Whether the decision is a non-waivable approval.
    pub fn is_non_waivable(self) -> bool {
        matches!(self, Self::RequireApproval { non_waivable: true })
    }
}

impl ModeCell {
    /// Resolves the cell under `switches`.
    pub fn resolve(self, switches: &PolicySwitches) -> TableDecision {
        let ask = TableDecision::RequireApproval { non_waivable: false };
        match self {
            Self::Allow => TableDecision::Allow,
            Self::Deny => TableDecision::Deny,
            Self::Ask => ask,
            Self::AskNonWaivable => TableDecision::RequireApproval { non_waivable: true },
            Self::AllowWithActiveEnforcer => {
                if switches.enforcer_active {
                    TableDecision::Allow
                } else {
                    TableDecision::Deny
                }
            }
            Self::AskOrAutoAllowSandboxed => {
                if switches.auto_allow_sandboxed && switches.sandbox_protects_paths {
                    TableDecision::Allow
                } else {
                    ask
                }
            }
            Self::AllowWithS3 => {
                if switches.sandbox_protects_paths {
                    TableDecision::Allow
                } else {
                    ask
                }
            }
            Self::AskOrHostShellInAuto => {
                if switches.allow_host_shell_in_auto {
                    TableDecision::Allow
                } else {
                    ask
                }
            }
        }
    }
}

/// The mode × class table of spec 3.1 §7.4, indexed `[TableRow][mode]` with modes in
/// [`HarnessMode::ALL`] order (ReadOnly, Plan, Ask, Auto).
///
/// # Example
///
/// ```rust
/// use adk_harness::{HarnessMode, ModeCell, decision_cell};
/// use adk_harness::TableRow;
///
/// assert_eq!(decision_cell(HarnessMode::Plan, TableRow::Edit), ModeCell::Deny);
/// assert_eq!(decision_cell(HarnessMode::Auto, TableRow::Edit), ModeCell::Allow);
/// ```
pub const MODE_TABLE: [[ModeCell; 4]; 16] = {
    use ModeCell::{
        Allow, AllowWithActiveEnforcer, AllowWithS3, Ask, AskNonWaivable, AskOrAutoAllowSandboxed,
        AskOrHostShellInAuto, Deny,
    };
    [
        // ReadOnly, Plan, Ask, Auto
        [Allow, Allow, Allow, Allow],       // Read, Todo, Delegate
        [Deny, AskNonWaivable, Deny, Deny], // PlanExit
        [Deny, Deny, Ask, Allow],           // Edit
        [Deny, Deny, AskNonWaivable, AskNonWaivable], // Edit, protected path
        [Deny, AllowWithActiveEnforcer, AskOrAutoAllowSandboxed, AllowWithS3], // Shell, OS sandbox
        [Deny, Deny, AskOrAutoAllowSandboxed, Allow], // Shell, container
        [Deny, Deny, Ask, AskOrHostShellInAuto], // Shell, host
        [Deny, Deny, AskNonWaivable, AskNonWaivable], // ShellEscalation
        [Deny, Deny, AskNonWaivable, AskNonWaivable], // MemoryWrite
        [Deny, Deny, Allow, Allow],         // MemoryPropose (assumed facts)
        [Allow, Allow, Allow, Allow],       // Web, allowlisted host
        [Deny, Deny, Ask, Ask],             // Web, other host
        [Deny, Deny, AskNonWaivable, AskNonWaivable], // Payment
        [Deny, Deny, AskNonWaivable, AskNonWaivable], // ComputerUse
        [Allow, Allow, Allow, Allow],       // External, read-only, trusted
        [Deny, Deny, Ask, Ask],             // External, other
    ]
};

fn mode_index(mode: HarnessMode) -> usize {
    HarnessMode::ALL.iter().position(|m| *m == mode).unwrap_or(0)
}

/// The [`MODE_TABLE`] cell for `mode` and `row` (spec 3.1 §7.4).
///
/// # Example
///
/// ```rust
/// use adk_harness::{HarnessMode, ModeCell, TableRow, decision_cell};
///
/// assert_eq!(decision_cell(HarnessMode::ReadOnly, TableRow::ShellHost), ModeCell::Deny);
/// ```
pub fn decision_cell(mode: HarnessMode, row: TableRow) -> ModeCell {
    MODE_TABLE[row.index()][mode_index(mode)]
}

/// Steps 4 and 6–9 of the decision function (spec 3.1 §7.4): combines the mode table's
/// decision with the standing-rule outcome.
///
/// Deny from the table or a Deny rule wins; a non-waivable approval ignores every rule;
/// an Allow rule turns a waivable approval into Allow and changes nothing else.
///
/// # Example
///
/// ```rust
/// use adk_core::PolicyDecision;
/// use adk_harness::approvals::matcher::RuleOutcome;
/// use adk_harness::{TableDecision, apply_rule};
///
/// let allow = RuleOutcome::Allow { rule_id: "r1".into() };
/// assert_eq!(apply_rule(TableDecision::RequireApproval { non_waivable: false }, &allow, "x"), PolicyDecision::Allow);
/// assert!(matches!(apply_rule(TableDecision::RequireApproval { non_waivable: true }, &allow, "x"), PolicyDecision::RequireApproval { .. }));
/// assert!(matches!(apply_rule(TableDecision::Deny, &allow, "x"), PolicyDecision::Deny { .. }));
/// ```
pub fn apply_rule(base: TableDecision, rule: &RuleOutcome, reason: &str) -> PolicyDecision {
    if base == TableDecision::Deny {
        return PolicyDecision::deny(reason);
    }
    if let RuleOutcome::Deny { reason, .. } = rule {
        return PolicyDecision::deny(format!("a standing rule denies it: {reason}"));
    }
    match base {
        TableDecision::Allow => PolicyDecision::Allow,
        TableDecision::RequireApproval { non_waivable: true } => PolicyDecision::require_approval(
            format!("{reason}; it needs a one-time approval and no standing rule applies"),
        ),
        TableDecision::RequireApproval { non_waivable: false } => match rule {
            RuleOutcome::Allow { .. } => PolicyDecision::Allow,
            _ => PolicyDecision::require_approval(reason),
        },
        TableDecision::Deny => PolicyDecision::deny(reason),
    }
}

/// The decision function's result with how it was reached.
#[derive(Clone, Debug)]
pub(crate) struct Evaluated {
    pub(crate) decision: PolicyDecision,
    pub(crate) kind: ToolKind,
    pub(crate) mode: HarnessMode,
    pub(crate) base: TableDecision,
}

/// What the confirmation handler needs to know about a call the policy did not deny.
#[derive(Clone, Debug)]
pub(crate) struct PromptContext {
    /// Whether the harness itself required approval. When it allowed the call, another
    /// layer of the run's policy or the agent's confirmation policy held it.
    pub(crate) harness_held: bool,
    /// The other layers of the run's policy, for the handler's re-check.
    pub(crate) peers: Option<Arc<dyn ToolPolicy>>,
    /// The run's kill switches; a freeze abandons an open prompt (I20).
    pub(crate) governance: Vec<adk_core::GovernanceControl>,
    pub(crate) app_name: String,
    pub(crate) user_id: String,
    pub(crate) session_id: String,
    pub(crate) invocation_id: String,
    pub(crate) agent_name: String,
    pub(crate) effect: ToolEffect,
    pub(crate) acting: Option<ActingContext>,
    pub(crate) audit: Option<AuditTrail>,
}

impl PromptContext {
    pub(crate) fn policy_request(&self, tool_name: &str, args: &Value) -> ToolPolicyRequest {
        let mut request = ToolPolicyRequest::new(tool_name, args.clone())
            .with_effect(self.effect)
            .with_agent_name(self.agent_name.clone())
            .with_identity(self.app_name.clone(), self.user_id.clone(), self.session_id.clone())
            .with_invocation_id(self.invocation_id.clone());
        if let Some(acting) = &self.acting {
            request = request.with_acting_context(acting.clone());
        }
        request
    }

    pub(crate) fn audit(&self) -> Option<HarnessAudit> {
        let trail = self.audit.clone()?;
        let actor = match &self.acting {
            Some(acting) => AuditActor::new(acting, self.invocation_id.clone()),
            None => AuditActor::for_invocation(
                &self.app_name,
                &self.agent_name,
                &self.user_id,
                &self.invocation_id,
            ),
        };
        Some(HarnessAudit::new(trail, actor, self.session_id.clone()))
    }
}

/// Upper bound of remembered prompt contexts; the oldest are dropped first.
const PROMPT_CONTEXT_CAP: usize = 4096;

fn call_key(agent: &str, subject: &str, tool: &str, args: &Value) -> String {
    format!("{agent}\u{1f}{subject}\u{1f}{}", tool_call_fingerprint(tool, args))
}

/// Contexts by call ID, dropping the oldest beyond [`PROMPT_CONTEXT_CAP`], so the
/// context of the call being decided is never the one dropped.
#[derive(Debug, Default)]
struct ByCall {
    entries: HashMap<String, (u64, PromptContext)>,
    order: VecDeque<(u64, String)>,
    next: u64,
}

/// Prompt contexts recorded by the policy for the handler, by call ID and by call key.
#[derive(Debug, Default)]
pub(crate) struct PromptContexts {
    by_call: Mutex<ByCall>,
    by_key: Mutex<HashMap<String, VecDeque<PromptContext>>>,
}

impl PromptContexts {
    fn insert(&self, call_id: Option<&str>, key: String, context: PromptContext) {
        if let Some(call_id) = call_id {
            let mut by_call = self.by_call.lock().unwrap_or_else(|e| e.into_inner());
            by_call.next += 1;
            let seq = by_call.next;
            by_call.entries.insert(call_id.to_string(), (seq, context.clone()));
            by_call.order.push_back((seq, call_id.to_string()));
            while by_call.order.len() > PROMPT_CONTEXT_CAP {
                let Some((seq, oldest)) = by_call.order.pop_front() else { break };
                if by_call.entries.get(&oldest).is_some_and(|(at, _)| *at == seq) {
                    by_call.entries.remove(&oldest);
                }
            }
        }
        if !context.harness_held {
            // Only calls the harness held are looked up without their ID.
            return;
        }
        let mut by_key = self.by_key.lock().unwrap_or_else(|e| e.into_inner());
        if by_key.len() >= PROMPT_CONTEXT_CAP {
            by_key.clear();
        }
        let queue = by_key.entry(key).or_default();
        queue.push_back(context);
        while queue.len() > 16 {
            queue.pop_front();
        }
    }

    /// The context recorded for the call `call_id`, left in place.
    #[cfg(feature = "acp-server")]
    pub(crate) fn by_call_id(&self, call_id: &str) -> Option<PromptContext> {
        let by_call = self.by_call.lock().unwrap_or_else(|e| e.into_inner());
        by_call.entries.get(call_id).map(|(_, context)| context.clone())
    }

    /// Takes the context recorded for `request`, by call ID first.
    pub(crate) fn take(&self, request: &ToolConfirmationRequest) -> Option<PromptContext> {
        if let Some(call_id) = request.function_call_id.as_deref() {
            let found = self
                .by_call
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entries
                .remove(call_id)
                .map(|(_, context)| context);
            if found.is_some() {
                return found;
            }
        }
        let acting = request.acting.as_deref()?;
        let subject = acting.on_behalf_of.as_ref().map(|s| s.id.as_str()).unwrap_or_default();
        let key = call_key(acting.agent.id.as_str(), subject, &request.tool_name, &request.args);
        let mut by_key = self.by_key.lock().unwrap_or_else(|e| e.into_inner());
        let queue = by_key.get_mut(&key)?;
        let context = queue.pop_front();
        if queue.is_empty() {
            by_key.remove(&key);
        }
        context
    }
}

/// Shell facts the policy decides on.
#[derive(Clone, Debug)]
pub(crate) struct ShellView {
    pub(crate) status: IsolationStatus,
    pub(crate) allow_unsandboxed_retry: bool,
    pub(crate) fail_if_unavailable: bool,
}

pub(crate) struct PolicyInner {
    pub(crate) registry: ToolClassRegistry,
    pub(crate) mode_store: Arc<dyn ModeStore>,
    pub(crate) default_mode: HarnessMode,
    pub(crate) rules: Arc<dyn StandingRuleStore>,
    pub(crate) ledger: Arc<dyn ShellLedger>,
    pub(crate) workspace: Workspace,
    pub(crate) shell: ShellView,
    pub(crate) switches: PolicySwitches,
    pub(crate) trust_mcp_read_only_hints: bool,
    pub(crate) prompts: PromptContexts,
    /// Provider-executed web tools the operator enabled (spec 3.1 §7.20, I49).
    pub(crate) provider_web_tools: BTreeSet<String>,
    #[cfg(feature = "hooks")]
    pub(crate) hooks: Option<Arc<crate::hooks::HookRunner>>,
    #[cfg(feature = "auth")]
    pub(crate) identity: Arc<crate::identity::IdentityGate>,
    #[cfg(feature = "observe")]
    pub(crate) root_agent: String,
}

/// The harness [`ToolPolicy`] (spec 3.1 §7.4, §7.5).
///
/// Reads the session's mode from the [`ModeStore`] and the subject's standing rules from
/// the [`StandingRuleStore`] on every evaluation, so a tightening or a revocation applies
/// at the next call (spec 3.1 §9.2 I4, I14). `configure_run` composes it with the run's
/// own policy, the stricter decision winning.
///
/// # Example
///
/// ```rust,ignore
/// let harness = adk_harness::HarnessAgent::builder("harness").model(model).workspace(ws).build()?;
/// let policy = harness.runtime().policy();
/// assert_eq!(adk_core::ToolPolicy::policy_ref(policy.as_ref()).as_deref(), Some("adk-harness"));
/// ```
#[derive(Clone)]
pub struct HarnessPolicy {
    pub(crate) inner: Arc<PolicyInner>,
    audit: Option<AuditTrail>,
    peers: Option<Arc<dyn ToolPolicy>>,
    governance: Vec<adk_core::GovernanceControl>,
}

impl std::fmt::Debug for HarnessPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessPolicy")
            .field("default_mode", &self.inner.default_mode)
            .field("isolation", &self.inner.shell.status)
            .field("audit", &self.audit.is_some())
            .finish()
    }
}

impl HarnessPolicy {
    pub(crate) fn new(inner: PolicyInner) -> Self {
        Self { inner: Arc::new(inner), audit: None, peers: None, governance: Vec::new() }
    }

    /// This policy, recording `harness.rule.*` evaluations through `audit`.
    pub(crate) fn with_audit(&self, audit: Option<AuditTrail>) -> Self {
        Self {
            inner: self.inner.clone(),
            audit,
            peers: self.peers.clone(),
            governance: self.governance.clone(),
        }
    }

    /// This policy as one layer of a run's policy whose other layers are `peers`, which the
    /// confirmation handler re-checks a held call against (spec 3.1 §7.3 compose rule).
    pub(crate) fn with_peers(mut self, peers: Option<Arc<dyn ToolPolicy>>) -> Self {
        self.peers = peers;
        self
    }

    /// This policy as a layer of a run whose kill switches are `governance`, which the
    /// confirmation handler watches while a person decides (spec 3.1 §9.2 I20).
    pub(crate) fn with_governance(mut self, governance: Vec<adk_core::GovernanceControl>) -> Self {
        self.governance = governance;
        self
    }

    /// Identifies the harness this policy belongs to; every clone of the harness shares it.
    pub(crate) fn owner_id(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }

    /// The default mode of sessions with no mode record.
    pub fn default_mode(&self) -> HarnessMode {
        self.inner.default_mode
    }

    /// The tool-class registry.
    pub fn registry(&self) -> &ToolClassRegistry {
        &self.inner.registry
    }

    /// The mode governing `invocation_id` of the session.
    ///
    /// An unparseable record or an unreadable store is `ReadOnly` (spec 3.1 §7.4).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mode = policy.mode_for("app", "alice", "s1", "inv-1").await;
    /// ```
    pub async fn mode_for(
        &self,
        app: &str,
        user: &str,
        session: &str,
        invocation_id: &str,
    ) -> HarnessMode {
        self.mode_and_plan_exit(app, user, session, invocation_id).await.0
    }

    /// The mode governing `invocation_id`, and whether that invocation already had a plan
    /// exit approved.
    async fn mode_and_plan_exit(
        &self,
        app: &str,
        user: &str,
        session: &str,
        invocation_id: &str,
    ) -> (HarnessMode, bool) {
        match self.inner.mode_store.get(app, user, session).await {
            Ok(Some(record)) => (
                record.mode_for_invocation(invocation_id),
                record.set_by == crate::mode::ModeSource::PlanApproval
                    && record.effective_from.as_deref() == Some(invocation_id),
            ),
            Ok(None) => (self.inner.default_mode, false),
            Err(error) => {
                tracing::warn!(error = %error, "mode store unavailable; evaluating as read_only");
                (HarnessMode::ReadOnly, false)
            }
        }
    }

    /// The class of the call `request` describes.
    pub fn classify(&self, request: &ToolPolicyRequest, origin: Option<&ToolOrigin>) -> ToolKind {
        // A provider-executed web tool the operator enabled is decided as an allowlisted web
        // call: enabling it is the allow decision, recorded per run (spec 3.1 §7.20, I49).
        if self.inner.provider_web_tools.contains(&request.tool_name) {
            return ToolKind::Web { allowlisted: true };
        }
        self.inner.registry.classify(
            &request.agent_name,
            &request.tool_name,
            origin,
            &request.args,
            request.effect,
            self.inner.shell.status.isolation(),
            self.inner.trust_mcp_read_only_hints,
        )
    }

    fn audit_for(&self, request: &ToolPolicyRequest) -> Option<HarnessAudit> {
        let trail = self.audit.clone()?;
        let actor = match &request.acting {
            Some(acting) => AuditActor::new(acting, request.invocation_id.clone()),
            None => AuditActor::for_invocation(
                &request.app_name,
                &request.agent_name,
                &request.user_id,
                &request.invocation_id,
            ),
        };
        Some(HarnessAudit::new(trail, actor, request.session_id.clone()))
    }

    /// Step 1 of the decision function: calls refused whatever the mode or rules.
    async fn hard_deny(
        &self,
        request: &ToolPolicyRequest,
        origin: Option<&ToolOrigin>,
        kind: &ToolKind,
    ) -> Option<String> {
        if origin.is_some_and(|origin| origin.source == BUILTIN_TOOL_ORIGIN)
            && is_provider_shell_builtin(&request.tool_name)
        {
            return Some(format!(
                "'{}' is a provider-executed shell; the harness runs commands only through its own bash",
                request.tool_name
            ));
        }
        if matches!(kind, ToolKind::Read | ToolKind::Edit)
            && let Some(path) = request.args.get("path").and_then(Value::as_str)
            && self.inner.workspace.resolve(path).is_err()
        {
            return Some(format!("path '{path}' is outside the workspace"));
        }
        if matches!(kind, ToolKind::Shell(_))
            && self.inner.shell.fail_if_unavailable
            && let IsolationStatus::Degraded { requested, reason } = &self.inner.shell.status
        {
            return Some(format!("the requested {requested} shell is unavailable: {reason}"));
        }
        if *kind == ToolKind::ShellEscalation {
            return self.retry_refusal(request).await;
        }
        // A web URL naming a loopback, link-local or private literal, or a `.local` /
        // `.internal` name, is denied before the tool runs (spec 3.1 §7.20, §9.2 I46).
        if matches!(kind, ToolKind::Web { .. })
            && let Some(url) = request.args.get("url").and_then(Value::as_str)
            && crate::web::url_names_private_target(url)
        {
            return Some(format!(
                "harness.web.private_target: '{}' is a private or local target the harness never fetches",
                crate::web::url_host(url).unwrap_or_default()
            ));
        }
        None
    }

    /// Why an unsandboxed retry is refused, or `None` when its `retry_of` is valid
    /// (spec 3.1 §7.9, §9.2 I25).
    async fn retry_refusal(&self, request: &ToolPolicyRequest) -> Option<String> {
        if !self.inner.shell.allow_unsandboxed_retry {
            return Some("unsandboxed retries are turned off for this harness".to_string());
        }
        if self.inner.shell.status.behaves_as_host() {
            return Some(
                "the shell already runs on the host; there is no sandboxed call to retry".into(),
            );
        }
        let Some(retry_of) = request.args.get("retry_of").and_then(Value::as_str) else {
            return Some(
                "an unsandboxed command needs retry_of naming the failed sandboxed call".into(),
            );
        };
        if request.args.get("reason").and_then(Value::as_str).is_none_or(|r| r.trim().is_empty()) {
            return Some("an unsandboxed retry needs a reason".into());
        }
        let command = request.args.get("command").and_then(Value::as_str).unwrap_or_default();
        match self
            .inner
            .ledger
            .get(&request.app_name, &request.user_id, &request.session_id, retry_of)
            .await
        {
            Ok(Some(entry))
                if entry.sandboxed
                    && entry.failed()
                    && entry.command == command
                    && entry.retried_by.is_none() =>
            {
                None
            }
            Ok(Some(_)) => Some(format!(
                "retry_of '{retry_of}' is not a failed sandboxed call with this exact command that \
                 has not been retried"
            )),
            Ok(None) => Some(format!("retry_of '{retry_of}' names no bash call in this session")),
            Err(error) => Some(format!("the shell ledger is unavailable: {error}")),
        }
    }

    /// The decision function of spec 3.1 §7.4.
    ///
    /// `origin` is the called tool's [`ToolOrigin`] and `call_id` its function call ID, as
    /// the governed path passes them through `ToolPolicy::evaluate_call` (spec 3.1 §10 C1).
    /// Without an origin no Allow rule matches (Deny rules still do), so a direct
    /// `ToolPolicy::evaluate` holds a call a rule would allow, and the confirmation handler
    /// re-checks it against the request's origin.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let decision = policy.decide(&request, Some(&ToolOrigin::of(tool)), Some("call-1")).await;
    /// ```
    pub async fn decide(
        &self,
        request: &ToolPolicyRequest,
        origin: Option<&ToolOrigin>,
        call_id: Option<&str>,
    ) -> PolicyDecision {
        #[cfg(feature = "observe")]
        let span = crate::observe::decision_span(&request.tool_name, &self.inner.shell.status);
        #[cfg(feature = "observe")]
        let evaluated = {
            use tracing::Instrument as _;
            self.evaluate_full(request, origin, call_id, true).instrument(span.clone()).await
        };
        #[cfg(not(feature = "observe"))]
        let evaluated = self.evaluate_full(request, origin, call_id, true).await;
        #[cfg(feature = "observe")]
        {
            span.record("harness.tool_kind", crate::observe::tool_kind_label(&evaluated.kind));
            span.record("harness.mode", evaluated.mode.as_str());
            span.record("harness.decision", format!("{:?}", evaluated.decision).as_str());
            if request.agent_name != self.inner.root_agent {
                span.record("harness.member", request.agent_name.as_str());
            }
        }
        if evaluated.kind == ToolKind::ShellEscalation {
            crate::shell::record_retry_requested(
                self.audit_for(request),
                self.inner.ledger.as_ref(),
                request,
                call_id,
                &evaluated.decision,
            )
            .await;
        }
        evaluated.decision
    }

    /// The decision function with the class, mode and table decision it reached.
    /// `governed` marks the evaluation of the governed path, which records stale and
    /// expired rules and remembers a held call's context for the confirmation handler; the
    /// handler's re-check passes `false`, so nothing is recorded twice.
    pub(crate) async fn evaluate_full(
        &self,
        request: &ToolPolicyRequest,
        origin: Option<&ToolOrigin>,
        call_id: Option<&str>,
        governed: bool,
    ) -> Evaluated {
        let kind = self.classify(request, origin);
        let deny = |decision: PolicyDecision, mode: HarnessMode| Evaluated {
            decision,
            kind,
            mode,
            base: TableDecision::Deny,
        };
        if let Some(reason) = self.hard_deny(request, origin, &kind).await {
            return deny(PolicyDecision::deny(reason), self.inner.default_mode);
        }
        let (mode, plan_exited) = self
            .mode_and_plan_exit(
                &request.app_name,
                &request.user_id,
                &request.session_id,
                &request.invocation_id,
            )
            .await;
        if kind == ToolKind::PlanExit && plan_exited {
            // The approved mode applies from the next invocation; a second plan exit in this
            // one would only ask the person again.
            return deny(
                PolicyDecision::deny(
                    "the plan was already approved in this turn; the new mode applies from the \
                     user's next message, so end your turn now",
                ),
                mode,
            );
        }
        // Without authenticated identity on a served harness, Auto decides as Ask and no
        // standing rule applies (spec 3.1 §9.2 I6).
        #[cfg(feature = "auth")]
        let identity_inert = self.inner.identity.inert(&request.invocation_id);
        #[cfg(not(feature = "auth"))]
        let identity_inert = false;
        let mode =
            if identity_inert && mode == HarnessMode::Auto { HarnessMode::Ask } else { mode };
        // Decided on the target the write reaches, so a symlink inside the workspace does not
        // turn a protected file into an ordinary edit (I11, I29).
        let path = request.args.get("path").and_then(Value::as_str);
        let protected = kind == ToolKind::Edit
            && path.is_some_and(|path| is_protected_target(self.inner.workspace.root(), path));
        let row = TableRow::of(&kind, protected);
        let mut switches = self.inner.switches;
        // Unapproved sandboxed commands need every protected path in place at this call (I26).
        if matches!(row, TableRow::ShellOsSandbox | TableRow::ShellContainer)
            && switches.sandbox_protects_paths
        {
            switches.sandbox_protects_paths =
                crate::shell::protected_paths_hold(self.inner.workspace.root());
        }
        let base = decision_cell(mode, row).resolve(&switches);
        let mut reason = describe(&request.tool_name, row, mode, base);
        if protected && path.is_some_and(|path| !is_protected_path(path)) {
            reason.push_str(" (the path resolves through a symlink to a protected path)");
        }
        if base == TableDecision::Deny {
            return deny(PolicyDecision::deny(reason), mode);
        }
        // 4a: the PreTool hooks may add a Deny and nothing else (spec 3.1 §7.22, §9.2 I53).
        // Only the governed evaluation runs them; the handler's re-check does not.
        #[cfg(feature = "hooks")]
        if governed && let Some(hooks) = &self.inner.hooks {
            let scope = crate::hooks::HookScope::from_request(request, call_id);
            let class = crate::hooks::class_label(&kind);
            let verdict = hooks
                .pre_tool(
                    &crate::hooks::HookCall {
                        tool: &request.tool_name,
                        args: &request.args,
                        class: &class,
                        acting: request.acting.as_ref(),
                        function_call_id: call_id,
                    },
                    mode,
                    &scope,
                    self.audit_for(request).as_ref(),
                )
                .await;
            if let crate::hooks::PreToolVerdict::Deny { reason: hook_reason } = verdict {
                return deny(
                    PolicyDecision::deny(format!("denied by a hook: {hook_reason}")),
                    mode,
                );
            }
        }

        let mut matched_rules = Vec::new();
        let evaluation = if kind.rule_eligible() && !identity_inert {
            match self
                .inner
                .rules
                .rules_for(&request.app_name, &request.user_id, &request.session_id)
                .await
            {
                Ok(rules) => {
                    let call = RuleCall {
                        app_name: &request.app_name,
                        user_id: &request.user_id,
                        session_id: &request.session_id,
                        acting: request.acting.as_ref(),
                        tool_name: &request.tool_name,
                        args: &request.args,
                        origin,
                        kind,
                        workspace_root: self.inner.workspace.root(),
                    };
                    let evaluation = evaluate_standing_rules(&rules, &call, Utc::now());
                    matched_rules = rules;
                    evaluation
                }
                Err(error) => {
                    tracing::warn!(error = %error, "standing rules unavailable; asking instead");
                    return Evaluated {
                        decision: PolicyDecision::require_approval(format!(
                            "{reason}; standing rules are unavailable"
                        )),
                        kind,
                        mode,
                        base: TableDecision::RequireApproval { non_waivable: true },
                    };
                }
            }
        } else {
            RuleEvaluation::default()
        };
        if governed {
            self.record_rule_observations(request, &evaluation).await;
        }

        let decision = apply_rule(base, &evaluation.outcome, &reason);
        if let (PolicyDecision::Allow, RuleOutcome::Allow { rule_id }) =
            (&decision, &evaluation.outcome)
            && base != TableDecision::Allow
        {
            tracing::Span::current().record("harness.rule_id", rule_id.as_str());
            tracing::info!(
                tool.name = %request.tool_name,
                harness.rule_id = %rule_id,
                harness.mode = %mode,
                "tool call allowed by a standing rule"
            );
            if let (Some(audit), Some(rule)) =
                (self.audit_for(request), matched_rules.iter().find(|rule| rule.id == *rule_id))
            {
                let rule_digest = crate::fsutil::hex_digest(
                    serde_json::to_string(rule).unwrap_or_default().as_bytes(),
                );
                let recorded = audit
                    .record_draft(
                        adk_core::audit_log::AuditRecordDraft::new(
                            adk_core::audit_log::AuditRecordKind::Other(
                                crate::audit::HARNESS_AUDIT_KIND.to_string(),
                            ),
                            "harness.rule.matched",
                        )
                        .policy_ref(format!("tool_policy:{HARNESS_POLICY_REF}")),
                        json!({
                            "ruleId": rule_id,
                            "ruleDigest": rule_digest,
                            "tool": request.tool_name,
                            "toolSource": origin.map(|origin| origin.source.clone()),
                            "declarationDigest": origin.map(|origin| origin.declaration_digest.clone()),
                            "functionCallId": call_id,
                        }),
                    )
                    .await;
                if let Err(error) = recorded {
                    tracing::warn!(error = %error, "could not record harness.rule.matched");
                    return Evaluated {
                        decision: PolicyDecision::require_approval(format!(
                            "{reason}; the standing rule's audit record could not be written"
                        )),
                        kind,
                        mode,
                        base,
                    };
                }
            }
        }
        // Recorded for every call the harness does not deny: another layer of the run's
        // policy, or the agent's confirmation policy, can still hold a call the harness
        // allows, and the handler must then ask the person rather than refuse.
        if governed && !matches!(decision, PolicyDecision::Deny { .. }) {
            let subject = request
                .acting
                .as_ref()
                .and_then(|acting| acting.on_behalf_of.as_ref())
                .map_or(request.user_id.as_str(), |subject| subject.id.as_str());
            let agent = request
                .acting
                .as_ref()
                .map_or(request.agent_name.as_str(), |acting| acting.agent.id.as_str());
            self.inner.prompts.insert(
                call_id,
                call_key(agent, subject, &request.tool_name, &request.args),
                PromptContext {
                    harness_held: matches!(decision, PolicyDecision::RequireApproval { .. }),
                    peers: self.peers.clone(),
                    governance: self.governance.clone(),
                    app_name: request.app_name.clone(),
                    user_id: request.user_id.clone(),
                    session_id: request.session_id.clone(),
                    invocation_id: request.invocation_id.clone(),
                    agent_name: request.agent_name.clone(),
                    effect: request.effect,
                    acting: request.acting.clone(),
                    audit: self.audit.clone(),
                },
            );
        }
        Evaluated { decision, kind, mode, base }
    }

    async fn record_rule_observations(
        &self,
        request: &ToolPolicyRequest,
        evaluation: &RuleEvaluation,
    ) {
        if evaluation.stale.is_empty() && evaluation.expired.is_empty() {
            return;
        }
        for stale in &evaluation.stale {
            tracing::info!(
                harness.rule_id = %stale.rule_id,
                tool.name = %request.tool_name,
                "standing rule is stale: the tool's declaration changed"
            );
        }
        let Some(audit) = self.audit_for(request) else {
            return;
        };
        for stale in &evaluation.stale {
            let record = audit
                .record(
                    "harness.rule.stale",
                    json!({
                        "ruleId": stale.rule_id,
                        "subject": request.user_id,
                        "app": request.app_name,
                        "tool": request.tool_name,
                        "source": stale.pinned_source,
                        "declarationDigest": stale.pinned_digest,
                        "observedSource": stale.observed_source,
                        "observedDigest": stale.observed_digest,
                    }),
                )
                .await;
            if let Err(error) = record {
                tracing::warn!(error = %error, "could not record harness.rule.stale");
            }
        }
        for rule_id in &evaluation.expired {
            if let Err(error) = audit
                .record(
                    "harness.rule.expired",
                    json!({ "ruleId": rule_id, "subject": request.user_id, "app": request.app_name, "tool": request.tool_name }),
                )
                .await
            {
                tracing::warn!(error = %error, "could not record harness.rule.expired");
            }
        }
    }

    pub(crate) fn prompts(&self) -> &PromptContexts {
        &self.inner.prompts
    }

    pub(crate) fn isolation_status(&self) -> &IsolationStatus {
        &self.inner.shell.status
    }
}

/// Anthropic's and OpenAI's provider-executed shell built-ins (spec 3.1 §9.2 I27).
fn is_provider_shell_builtin(name: &str) -> bool {
    name.starts_with("bash_")
        || name == "local_shell"
        || name == "shell"
        || name.starts_with("code_execution")
}

fn describe(tool: &str, row: TableRow, mode: HarnessMode, base: TableDecision) -> String {
    let class = match row {
        TableRow::ReadTodoDelegate => "a read",
        TableRow::PlanExit => "the plan exit",
        TableRow::Edit => "an edit",
        TableRow::EditProtected => "an edit of a protected path",
        TableRow::ShellOsSandbox => "a sandboxed shell command",
        TableRow::ShellContainer => "a container shell command",
        TableRow::ShellHost => "a host shell command (full user permissions, network)",
        TableRow::ShellEscalation => "an unsandboxed retry",
        TableRow::MemoryWrite => "a memory write",
        TableRow::MemoryPropose => "a fact proposal",
        TableRow::WebAllowlisted => "a web request to an allowlisted host",
        TableRow::WebOther => "a web request to a host outside the allowlist",
        TableRow::Payment => "a payment",
        TableRow::ComputerUse => "a desktop action",
        TableRow::ExternalReadOnlyTrusted => "a trusted read-only tool",
        TableRow::ExternalOther => "an external tool",
    };
    match base {
        TableDecision::Deny => format!("'{tool}' is {class}, which {mode} mode does not allow"),
        TableDecision::RequireApproval { .. } => {
            format!("'{tool}' is {class}, which needs approval in {mode} mode")
        }
        TableDecision::Allow => format!("'{tool}' is {class}, allowed in {mode} mode"),
    }
}

#[async_trait]
impl ToolPolicy for HarnessPolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        self.evaluate_call(request, &PolicyCall::new()).await
    }

    async fn evaluate_call(
        &self,
        request: &ToolPolicyRequest,
        call: &PolicyCall<'_>,
    ) -> PolicyDecision {
        self.decide(request, call.origin, call.function_call_id).await
    }

    fn policy_ref(&self) -> Option<String> {
        Some(HARNESS_POLICY_REF.to_string())
    }
}
