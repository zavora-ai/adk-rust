//! `HarnessAgent`, its runtime, and the run-config layer (spec 3.1 §7.3).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditActor;
use adk_core::{
    AdkError, Agent, AgentCapabilities, AgentTopology, ApprovalService, ApprovalStore,
    DEFAULT_TOOL_CONFIRMATION_TIMEOUT, ErrorCategory, ErrorComponent, EventStream,
    EventsCompactionConfig, InvocationContext, Result, RunBudget, RunConfig, Session, SpendLedger,
    ToolConfirmationHandler, ToolPolicy, async_trait,
};
use adk_devtools::Workspace;
use serde::Serialize;
use serde_json::{Value, json};

use crate::approvals::StandingRuleStore;
use crate::approvals::matcher::is_protected_target;
use crate::audit::HarnessAudit;
use crate::instructions::{ProjectInstructions, WorkspaceTrustStore};
use crate::mode::{HarnessMode, ModeStore};
use crate::policy::{HarnessPolicy, composed_layer_owner, register_composed_layer};
use crate::shell::{IsolationStatus, ShellLedger};
use crate::tools::TodoList;
use crate::toolset::HarnessToolset;

/// Error code of a harness run without the harness policy in its run config (spec 3.1 §7.3).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::HARNESS_UNGOVERNED_CODE, "harness.ungoverned");
/// ```
pub const HARNESS_UNGOVERNED_CODE: &str = "harness.ungoverned";

/// What a built harness is (spec 3.1 §7.3).
///
/// # Example
///
/// ```rust,ignore
/// let info = harness.info();
/// println!("{} in {} mode, shell: {}", info.name, info.default_mode, info.isolation);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessInfo {
    /// The agent name.
    pub name: String,
    /// The mode of sessions with no mode record.
    pub default_mode: HarnessMode,
    /// Where `bash` runs.
    pub isolation: IsolationStatus,
    /// Why a requested sandbox is unavailable.
    pub degraded_reason: Option<String>,
    /// Team members.
    pub members: Vec<String>,
    /// Active packs.
    pub packs: Vec<String>,
    /// The harness tools.
    pub tools: Vec<String>,
    /// The reasoning effort of each mode and role (spec 3.1 §7.23).
    pub effort: crate::posture::EffortPolicy,
    /// Whether a `cache_capable` model gets the runner's explicit context cache.
    pub context_caching: bool,
    /// Which path `web_search` takes (spec 3.1 §7.20).
    pub web_search: crate::web::WebSearchPath,
}

impl HarnessInfo {
    /// Where `bash` runs, or `None` when the harness offers no shell — as the personal
    /// preset's does outside the OS sandbox or a container (spec 3.3 §10 H1). Surfaces show
    /// this rather than [`isolation`](Self::isolation), which names the isolation the
    /// harness was configured with whether or not it offers `bash`.
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
    /// assert_eq!(harness.info().shell(), None, "no host shell");
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    pub fn shell(&self) -> Option<&IsolationStatus> {
        self.tools.iter().any(|tool| tool == "bash").then_some(&self.isolation)
    }
}

/// Project instructions with the trust they load under.
#[derive(Debug)]
pub(crate) struct InstructionState {
    pub(crate) instructions: ProjectInstructions,
    pub(crate) trust_store: Arc<dyn WorkspaceTrustStore>,
    pub(crate) require_trust: bool,
}

impl InstructionState {
    pub(crate) fn trusted(&self) -> bool {
        if !self.require_trust {
            return true;
        }
        match self.trust_store.get(self.instructions.root_digest()) {
            Ok(Some(trust)) => trust.covers(&self.instructions.instructions_digest()),
            Ok(None) => false,
            Err(error) => {
                tracing::warn!(error = %error, "workspace trust store unavailable; treating the workspace as untrusted");
                false
            }
        }
    }
}

/// The governed services a harness applies to every run (spec 3.1 §7.3).
///
/// # Example
///
/// ```rust,ignore
/// let rules = harness.runtime().standing_rules().list("adk-rust", "alice").await?;
/// ```
#[derive(Clone)]
pub struct HarnessRuntime {
    pub(crate) policy: HarnessPolicy,
    pub(crate) operator_policy: Option<Arc<dyn ToolPolicy>>,
    pub(crate) mode_store: Arc<dyn ModeStore>,
    pub(crate) standing_rules: Arc<dyn StandingRuleStore>,
    pub(crate) ledger: Arc<dyn ShellLedger>,
    pub(crate) approval_store: Arc<dyn ApprovalStore>,
    pub(crate) approval_service: Option<Arc<dyn ApprovalService>>,
    pub(crate) ledgers: crate::ledger::HarnessLedgers,
    pub(crate) handler: Option<Arc<dyn ToolConfirmationHandler>>,
    pub(crate) confirmation_timeout: Duration,
    pub(crate) budget: Option<RunBudget>,
    pub(crate) spend_ledger: Option<Arc<dyn SpendLedger>>,
    pub(crate) history_max_events: Option<usize>,
    pub(crate) compaction: Option<EventsCompactionConfig>,
    pub(crate) toolset: Arc<HarnessToolset>,
    pub(crate) instructions: Arc<InstructionState>,
    pub(crate) non_waivable_tools: BTreeSet<String>,
    /// Prefixes of tool names no run-wide approval answers: the coordinator's crossing tools
    /// listed at request time, `<to>__` for each partition it crosses into (spec 3.2 K-I15).
    pub(crate) non_waivable_prefixes: Vec<String>,
    pub(crate) workspace: Workspace,
    pub(crate) audit: Option<adk_core::audit_log::AuditTrail>,
    #[cfg(feature = "auth")]
    pub(crate) identity: Arc<crate::identity::IdentityGate>,
    #[cfg(feature = "observe")]
    pub(crate) spend: Arc<crate::observe::SpendAccounts>,
    #[cfg(feature = "observe")]
    pub(crate) run_trails: Arc<crate::observe::RunTrails>,
    #[cfg(feature = "team")]
    pub(crate) team: Option<Arc<crate::team::HarnessTeam>>,
    #[cfg(feature = "graph")]
    pub(crate) plan_graph: Option<Arc<crate::plan_graph::PlanGraphs>>,
    /// Snapshots and rewind (spec 3.1 §7.27).
    pub(crate) snapshots: Option<Arc<crate::snapshot::Snapshots>>,
    pub(crate) goals: crate::goal::GoalRuntime,
    /// Reports approval outcomes to the kernel built by `Harness::kernel` (spec 3.2 §7.4).
    #[cfg(feature = "kernel")]
    pub(crate) kernel_approvals: Arc<crate::kernel::LateApprovalNotifier>,
    /// The agenda of the kernel built by `Harness::kernel`, for `propose_followup` (spec 3.2
    /// §7.5).
    #[cfg(feature = "kernel")]
    pub(crate) kernel_agenda: adk_kernel::AgendaHandle,
    /// The kernel's ledger, bound when a kernel built by `Harness::kernel` starts (spec 3.2
    /// §7.10).
    #[cfg(feature = "kernel")]
    pub(crate) kernel_ledger: adk_kernel::proc::LateLedgerSink,
    /// The partition the harness was built for and its policy (spec 3.2 §7.8).
    #[cfg(feature = "kernel")]
    pub(crate) partition: Option<crate::partition::PartitionWiring>,
    /// The kernel target the harness stamps its sessions with (spec 3.3 §5.7 "Threads").
    #[cfg(feature = "kernel")]
    pub(crate) session_stamp: Option<String>,
    /// Provider-executed search and URL tools, when the operator enabled any (spec 3.1 §7.20).
    #[cfg(feature = "web")]
    pub(crate) provider_web: Option<Arc<crate::web::provider::ProviderWebToolset>>,
    /// MCP servers and the bridges answering them (spec 3.1 §7.21).
    #[cfg(feature = "mcp")]
    pub(crate) mcp: Option<Arc<crate::mcp::McpInteractionRuntime>>,
    #[cfg(feature = "hooks")]
    pub(crate) hooks: Option<Arc<crate::hooks::HookRunner>>,
    pub(crate) effort: crate::posture::EffortPolicy,
    /// The model whose context the runner caches explicitly, when caching is on.
    // Read by `configure_runner`, which exists with feature `runner` only.
    #[cfg_attr(not(feature = "runner"), allow(dead_code))]
    pub(crate) cache_capable: Option<Arc<dyn adk_core::CacheCapable>>,
    /// The payments pack's per-request reminder (spec 3.1 §7.16).
    #[cfg(feature = "payments")]
    pub(crate) payments: Option<Arc<crate::packs::payments::PaymentsReminder>>,
    /// Take over, when the harness was built with it (spec 3.3 §10 H2).
    pub(crate) take_over: Option<crate::take_over::TakeOver>,
    /// The tools and toolsets the model is given besides `toolset`, for a realtime session
    /// of the harness (spec 3.3 §5.6).
    #[cfg_attr(not(feature = "voice"), allow(dead_code))]
    pub(crate) agent_tools: AgentTools,
}

/// The tools and toolsets the model is given besides the harness toolset.
pub(crate) type AgentTools = (Vec<Arc<dyn adk_core::Tool>>, Vec<Arc<dyn adk_core::Toolset>>);

/// The harness's audit trail, in Block mode: `recorder`, or with feature `auth` an
/// [`EphemeralAuditLog`](crate::EphemeralAuditLog) (spec 3.1 §9.2 I39).
pub(crate) fn harness_audit_trail(
    recorder: Option<Arc<dyn adk_core::audit_log::AuditRecorder>>,
) -> Result<Option<adk_core::audit_log::AuditTrail>> {
    #[cfg(feature = "auth")]
    let recorder = match recorder {
        Some(recorder) => Some(recorder),
        None => Some(Arc::new(crate::identity::EphemeralAuditLog::new()?)
            as Arc<dyn adk_core::audit_log::AuditRecorder>),
    };
    Ok(recorder.map(adk_core::audit_log::AuditTrail::new))
}

impl std::fmt::Debug for HarnessRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessRuntime")
            .field("policy", &self.policy)
            .field("confirmation_timeout", &self.confirmation_timeout)
            .field("handler", &self.handler.is_some())
            .finish_non_exhaustive()
    }
}

impl HarnessRuntime {
    /// The layer `configure_run` applies.
    pub fn layer(&self) -> HarnessLayer<'_> {
        HarnessLayer { runtime: self }
    }

    /// The harness policy.
    pub fn policy(&self) -> &HarnessPolicy {
        &self.policy
    }

    /// Where session modes are kept.
    pub fn mode_store(&self) -> &Arc<dyn ModeStore> {
        &self.mode_store
    }

    /// Where standing rules are kept.
    pub fn standing_rules(&self) -> &Arc<dyn StandingRuleStore> {
        &self.standing_rules
    }

    /// The per-session shell ledger.
    pub fn shell_ledger(&self) -> &Arc<dyn ShellLedger> {
        &self.ledger
    }

    /// Where held calls and their decisions are kept.
    pub fn approval_store(&self) -> &Arc<dyn ApprovalStore> {
        &self.approval_store
    }

    /// The approval inbox, if configured.
    pub fn approval_service(&self) -> Option<&Arc<dyn ApprovalService>> {
        self.approval_service.as_ref()
    }

    /// Take over (spec 3.3 §8.2, §10 H2), when the harness was built with it: surfaces answer
    /// a take-over entry through it.
    pub fn take_over(&self) -> Option<&crate::take_over::TakeOver> {
        self.take_over.as_ref()
    }

    /// Where non-idempotent calls are recorded when a run carries no ledger of its own
    /// (spec 3.1 §7.12).
    pub fn action_ledger(&self) -> &Arc<dyn adk_core::ActionLedger> {
        self.ledgers.store()
    }

    /// The live confirmation handler: the configured one, or the prompting handler over
    /// the configured prompter.
    pub fn confirmation_handler(&self) -> Option<&Arc<dyn ToolConfirmationHandler>> {
        self.handler.as_ref()
    }

    /// The harness toolset.
    pub fn toolset(&self) -> &Arc<HarnessToolset> {
        &self.toolset
    }

    /// The workspace's instruction files.
    pub fn instructions(&self) -> &ProjectInstructions {
        &self.instructions.instructions
    }

    /// Where workspace trust decisions are kept.
    pub fn trust_store(&self) -> &Arc<dyn WorkspaceTrustStore> {
        &self.instructions.trust_store
    }

    /// Whether the workspace's project instruction files load.
    pub fn workspace_trusted(&self) -> bool {
        self.instructions.trusted()
    }

    /// The workspace the harness works in.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// The events compaction the harness asks its runner for.
    pub fn compaction(&self) -> Option<&EventsCompactionConfig> {
        self.compaction.as_ref()
    }

    /// The spend of `subject`, or of every subject, since the harness was built: per
    /// acting agent, subject and vendor (spec 3.1 §7.13).
    #[cfg(feature = "observe")]
    pub fn spend(&self, subject: Option<&str>) -> crate::observe::SpendReport {
        self.spend.report(subject)
    }

    /// The audit trail a run gets when it carries none (spec 3.1 §7.14, §9.2 I39).
    pub fn audit_trail(&self) -> Option<&adk_core::audit_log::AuditTrail> {
        self.audit.as_ref()
    }

    /// Reads MCP resource `uri` of `server` for an explicit `@server:resource` mention by
    /// `user` in `session` of `app`: the delimited untrusted block, recorded as
    /// `harness.mcp.resource.read` with `via: mention` (spec 3.1 §7.21, I52).
    ///
    /// # Errors
    ///
    /// `harness.mcp.unknown_server`, the server's error, or the audit error.
    #[cfg(feature = "mcp")]
    pub async fn mcp_resource_mention(
        &self,
        app: &str,
        user: &str,
        session: &str,
        server: &str,
        uri: &str,
    ) -> Result<serde_json::Value> {
        let Some(mcp) = &self.mcp else {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::NotFound,
                "harness.mcp.unknown_server",
                "this harness has no MCP servers",
            ));
        };
        let audit = self.audit.clone().map(|trail| {
            crate::audit::HarnessAudit::new(
                trail,
                adk_core::audit_log::AuditActor::for_invocation(app, "harness", user, "mention"),
                session,
            )
        });
        mcp.read_resource(server, uri, "mention", audit.as_ref()).await
    }

    /// The MCP servers and bridges, when the harness has any (spec 3.1 §7.21).
    #[cfg(feature = "mcp")]
    pub fn mcp(&self) -> Option<&Arc<crate::mcp::McpInteractionRuntime>> {
        self.mcp.as_ref()
    }

    /// The compiled team, when the harness has members (spec 3.1 §7.10).
    #[cfg(feature = "team")]
    pub fn team(&self) -> Option<&crate::team::HarnessTeam> {
        self.team.as_deref()
    }

    /// Where session goals are kept (spec 3.1 §7.11).
    pub fn goal_store(&self) -> &Arc<dyn crate::goal::GoalStore> {
        &self.goals.store
    }

    /// The plan graphs, when `plan_graph` is on (spec 3.1 §7.11).
    #[cfg(feature = "graph")]
    pub fn plan_graph(&self) -> Option<&Arc<crate::plan_graph::PlanGraphs>> {
        self.plan_graph.as_ref()
    }

    /// The workspace's snapshots, when the harness takes them (spec 3.1 §7.27):
    /// [`Snapshots::take`](crate::snapshot::Snapshots::take) is "snapshot now" for a governed
    /// path that is about to do something a file restore must be able to undo (§7.30).
    pub fn snapshots(&self) -> Option<&Arc<crate::snapshot::Snapshots>> {
        self.snapshots.as_ref()
    }

    /// The hooks the harness runs, when any are configured (spec 3.1 §7.22).
    #[cfg(feature = "hooks")]
    pub fn hooks(&self) -> Option<&crate::hooks::HookSet> {
        self.hooks.as_deref().map(crate::hooks::HookRunner::set)
    }
}

/// What `HarnessAgent::configure_run` applies to a run's config (spec 3.1 §7.3).
///
/// | Rule | Effect |
/// |---|---|
/// | Compose, never replace | the run's policy and the operator policy are kept; the strictest decision wins |
/// | Idempotent | a config that already carries this harness's layer is left as it is; a policy that only names itself `adk-harness` is composed with it |
/// | Approvals | the approval service and store are filled only when empty or identical; otherwise the run's own are kept with a warning |
/// | Action ledger | the harness's ledger when the run carries none; a run's own keeps the records, with resumed held calls keyed alike (spec 3.1 §7.12) |
/// | Non-waivable | `tool_approvals` entries naming non-waivable calls are removed |
/// | Run-owned fields | `durable_run_id`, the confirmation handler and call-ID decisions are never touched |
///
/// # Example
///
/// ```rust,ignore
/// let mut config = adk_core::RunConfig::default();
/// harness.runtime().layer().apply(&mut config);
/// assert!(adk_harness::has_harness_layer(config.tool_policy.as_deref().unwrap()));
/// ```
#[derive(Clone, Copy)]
pub struct HarnessLayer<'a> {
    runtime: &'a HarnessRuntime,
}

/// Whether the `tool_approvals` key `fingerprint` names a call no fingerprint approval may
/// authorize (spec 3.1 §7.3, §9.2 I7). Anything that does not parse is removed too. An
/// edit's path is judged at the target it resolves to under `root` (I11, I29).
pub(crate) fn names_non_waivable(
    fingerprint: &str,
    non_waivable: &BTreeSet<String>,
    prefixes: &[String],
    root: &std::path::Path,
) -> bool {
    let parts: Vec<&str> = fingerprint.splitn(5, '\u{1f}').collect();
    if parts.iter().any(|part| non_waivable.contains(*part)) {
        return true;
    }
    if parts.get(3).is_some_and(|tool| prefixes.iter().any(|prefix| tool.starts_with(prefix))) {
        return true;
    }
    let [_, _, _, tool, args] = parts.as_slice() else {
        return true;
    };
    let Ok(args) = serde_json::from_str::<Value>(args) else {
        return true;
    };
    match *tool {
        "bash" => args.get("unsandboxed").is_some_and(|value| *value != Value::Bool(false)),
        // A protected path anywhere in a patch makes the whole call non-waivable (I67).
        "write_file" | "edit_file" | crate::edits::APPLY_PATCH => {
            crate::edits::edit_paths(tool, &args).is_none_or(|paths| {
                paths.is_empty() || paths.iter().any(|path| is_protected_target(root, path))
            })
        }
        _ => false,
    }
}

impl HarnessLayer<'_> {
    /// Applies the layer to `config`.
    pub fn apply(&self, config: &mut RunConfig) {
        self.apply_with(config, true);
    }

    /// Applies the layer; `compose_policy` false leaves the policy to `configure_run`,
    /// which binds it to the run's audit trail.
    fn apply_with(&self, config: &mut RunConfig, compose_policy: bool) {
        let runtime = self.runtime;
        // Before the policy is composed, which binds it to the run's trail. The runner path
        // installs the trail through `RunnerConfigBuilder::audit_log` instead, so a
        // runner's own log takes precedence.
        if compose_policy && config.audit.is_none() {
            config.audit = runtime.audit.clone();
        }
        let owner = runtime.policy.owner_id();
        if compose_policy
            && config.tool_policy.as_deref().and_then(composed_layer_owner) != Some(owner)
        {
            // A runner's own audit log also feeds a bound kernel ledger (spec 3.2 §7.10);
            // the harness's trail was tapped when it was built. Before the policy binds it.
            #[cfg(feature = "kernel")]
            if runtime.kernel_ledger.is_bound()
                && let Some(trail) = config.audit.take()
            {
                let address = |trail: &adk_core::audit_log::AuditTrail| {
                    Arc::as_ptr(trail.recorder()).cast::<()>() as usize
                };
                let ours =
                    runtime.audit.as_ref().is_some_and(|own| address(own) == address(&trail));
                config.audit = Some(if ours {
                    trail
                } else {
                    crate::kernel::tap_audit(trail, &runtime.kernel_ledger)
                });
            }
            let mut layers: Vec<Arc<dyn ToolPolicy>> = Vec::new();
            if let Some(existing) = config.tool_policy.take() {
                layers.push(existing);
            }
            if let Some(operator) = &runtime.operator_policy {
                layers.push(operator.clone());
            }
            let peers = (!layers.is_empty()).then(|| {
                Arc::new(adk_core::StrictestPolicy::new(layers.clone())) as Arc<dyn ToolPolicy>
            });
            layers.push(Arc::new(
                runtime
                    .policy
                    .with_audit(config.audit.clone())
                    .with_peers(peers)
                    .with_governance(config.governance.clone()),
            ));
            let composed: Arc<dyn ToolPolicy> = Arc::new(adk_core::StrictestPolicy::new(layers));
            register_composed_layer(owner, &composed);
            config.tool_policy = Some(composed);
        }

        match (&config.approval_service, &runtime.approval_service) {
            (None, Some(service)) => config.approval_service = Some(service.clone()),
            (Some(own), Some(service)) if !Arc::ptr_eq(own, service) => tracing::warn!(
                "the run carries its own approval service; keeping it instead of the harness's"
            ),
            _ => {}
        }
        match &config.approval_store {
            None => {
                let store = runtime
                    .approval_service
                    .as_ref()
                    .and_then(|service| service.approval_store())
                    .unwrap_or_else(|| runtime.approval_store.clone());
                config.approval_store = Some(store);
            }
            Some(own) if !Arc::ptr_eq(own, &runtime.approval_store) => {
                tracing::debug!("the run carries its own approval store; keeping it");
            }
            Some(_) => {}
        }

        // Non-idempotent harness calls run at most once across crashes, retries and resumes
        // (§7.12, I37); a ledger the run carries keeps the records. Per run only, like the
        // policy: a runner installs its own ledger on a run config that carries none.
        if compose_policy {
            config.action_ledger = Some(runtime.ledgers.for_run(config.action_ledger.as_ref()));
        }

        let before = config.tool_approvals.len();
        config.tool_approvals.retain(|fingerprint, _| {
            !names_non_waivable(
                fingerprint,
                &runtime.non_waivable_tools,
                &runtime.non_waivable_prefixes,
                runtime.workspace.root(),
            )
        });
        if config.tool_approvals.len() != before {
            tracing::warn!(
                removed = before - config.tool_approvals.len(),
                "removed run-wide approvals of non-waivable tool calls"
            );
        }

        if config.tool_confirmation_timeout == DEFAULT_TOOL_CONFIRMATION_TIMEOUT {
            config.tool_confirmation_timeout = runtime.confirmation_timeout;
        }
        if config.budget.is_none()
            && let Some(budget) = &runtime.budget
        {
            config.budget = Some(budget.clone());
        }
        // A tracker counts one run (spec 3.1 §7.13): the run's own config gets one here; a
        // runner's base config carries the budget only, and the runner creates a tracker
        // per run from it.
        if compose_policy
            && config.budget_tracker.is_none()
            && let Some(budget) = &config.budget
        {
            config.budget_tracker = Some(Arc::new(adk_core::BudgetTracker::new(budget.clone())));
        }
        if config.spend_ledger.is_none() {
            config.spend_ledger = runtime.spend_ledger.clone();
        }
        #[cfg(feature = "observe")]
        if compose_policy {
            crate::observe::meter_run(&runtime.spend, config);
        }
        if config.history_max_events.is_none() {
            config.history_max_events = runtime.history_max_events;
        }
    }
}

/// The harness agent: a governed `LlmAgent` with the harness policy, approvals and tools
/// (spec 3.1 §7.3).
///
/// `configure_run` carries the policy and approvals into every runtime that runs it — a
/// `Runner`, a transfer, a delegate behind an agent tool — without changing them. `run`
/// refuses unless its policy is in the run's config, because the runner calls
/// `configure_run` on the root agent only (spec 3.1 §9.2 I17).
///
/// # Example
///
/// ```rust,ignore
/// use adk_harness::HarnessAgent;
///
/// let harness = HarnessAgent::builder("harness")
///     .model(model)
///     .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
///     .build()?;
/// let harness = harness.into_harness("adk-rust", sessions)?;
/// ```
pub struct HarnessAgent {
    pub(crate) root: Arc<dyn Agent>,
    pub(crate) runtime: HarnessRuntime,
    pub(crate) info: HarnessInfo,
    pub(crate) manifest: crate::manifest::HarnessManifest,
}

impl std::fmt::Debug for HarnessAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessAgent").field("info", &self.info).finish_non_exhaustive()
    }
}

impl HarnessAgent {
    /// Starts building a harness named `name`.
    ///
    /// # Example
    ///
    /// ```rust
    /// let builder = adk_harness::HarnessAgent::builder("harness");
    /// # let _ = builder;
    /// ```
    pub fn builder(name: impl Into<String>) -> crate::HarnessAgentBuilder {
        crate::HarnessAgentBuilder::new(name)
    }

    /// The harness as an `Arc<dyn Agent>`, to hand to any runtime: it applies the harness
    /// layer in `configure_run` and refuses to run without it.
    pub fn agent(&self) -> Arc<dyn Agent> {
        Arc::new(Self {
            root: self.root.clone(),
            runtime: self.runtime.clone(),
            info: self.info.clone(),
            manifest: self.manifest.clone(),
        })
    }

    /// The governed services.
    pub fn runtime(&self) -> &HarnessRuntime {
        &self.runtime
    }

    /// What the harness is.
    pub fn info(&self) -> &HarnessInfo {
        &self.info
    }

    /// What the harness can do, as the model is told it (spec 3.1 §7.25): generated when
    /// the harness was built from its classes in each mode, its tools, packs, memory, the
    /// surface's commands and what its safety mechanisms cover.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{HarnessAgent, InstructionsConfig, ModeDecision};
    /// use adk_model::{ScriptedLlm, ScriptedResponse};
    /// use std::sync::Arc;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let harness = HarnessAgent::builder("assistant")
    ///     .model(Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("ok")])))
    ///     .workspace(adk_devtools::Workspace::new(dir.path()))
    ///     .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
    ///     .build()?;
    /// let edit = harness.manifest().classes.iter().find(|class| class.class == "edit").unwrap();
    /// assert_eq!(edit.decisions[2], ModeDecision::Ask, "ask mode asks before an edit");
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    pub fn manifest(&self) -> &crate::manifest::HarnessManifest {
        &self.manifest
    }

    /// The partition the harness was built for (spec 3.2 §7.8,
    /// [`HarnessAgentBuilder::partition`](crate::HarnessAgentBuilder::partition)).
    #[cfg(feature = "kernel")]
    pub fn partition(&self) -> Option<&adk_kernel::Partition> {
        self.runtime.partition.as_ref().map(|wiring| &wiring.partition)
    }

    /// The partition's policy, when the harness was built for one: what decides its
    /// crossings (spec 3.2 §7.8, K-I15).
    #[cfg(feature = "kernel")]
    pub fn partition_policy(&self) -> Option<&adk_kernel::PartitionPolicy> {
        self.runtime.partition.as_ref().map(|wiring| &wiring.policy)
    }

    #[cfg(feature = "kernel")]
    pub(crate) fn partition_wiring(&self) -> Option<&crate::partition::PartitionWiring> {
        self.runtime.partition.as_ref()
    }

    /// A run config carrying the harness layer and the confirmation handler, for callers
    /// wiring their own `Runner`. Its policy records no `harness.*` audit records; with the
    /// harness at the root of the runner, `configure_run` binds the run's audit trail.
    pub fn run_config(&self) -> RunConfig {
        let mut config = RunConfig::default();
        if let Some(handler) = &self.runtime.handler {
            config.tool_confirmation_handler = Some(handler.clone());
        }
        self.runtime.layer().apply(&mut config);
        config
    }

    /// `root` as the root agent of a `Runner` under this harness's layer (spec 3.1 §7.10):
    /// a review round from [`HarnessTeam::review_round_with`](crate::HarnessTeam::review_round_with),
    /// or any agent built from the harness's members.
    ///
    /// Its `configure_run` applies `root`'s own configuration and then
    /// [`HarnessRuntime::layer`], so every tool call `root`'s agents make goes through
    /// `authorize_tool_call` under the harness policy and mode, with the run's audit trail,
    /// approvals and budget, and the members record `harness.member.*` in that trail. It
    /// refuses to run without that layer, as the harness does (spec 3.1 §9.2 I17), and with
    /// feature `observe` meters the run into [`HarnessRuntime::spend`] and records
    /// `harness.spend`. It never transfers: the runner always starts at `root`. Its name,
    /// description and sub-agents are `root`'s.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_runner::Runner;
    ///
    /// let team = harness.runtime().team().expect("the harness has members");
    /// let round = team.review_round("review", &["review_correctness", "review_style"], 1)?;
    /// let runner = harness
    ///     .configure_runner(
    ///         Runner::builder().app_name("app").agent(harness.governed_root(round)).session_service(sessions),
    ///     )
    ///     .build()?;
    /// ```
    pub fn governed_root(&self, root: Arc<dyn Agent>) -> Arc<dyn Agent> {
        Arc::new(GovernedRoot {
            runtime: self.runtime.clone(),
            root,
            default_mode: self.info.default_mode,
            isolation: self.info.isolation.clone(),
        })
    }

    /// Applies the harness run config, approval store and compaction to a runner builder.
    /// Call it before setting a run config of your own on the builder.
    ///
    /// A budget set with [`HarnessAgentBuilder::budget`](crate::HarnessAgentBuilder::budget)
    /// bounds each run on its own: the runner's base config carries the budget and the
    /// runner counts every run against fresh counters (spec 3.1 §7.13).
    #[cfg(feature = "runner")]
    pub fn configure_runner<A, G, S>(
        &self,
        builder: adk_runner::RunnerConfigBuilder<A, G, S>,
    ) -> adk_runner::RunnerConfigBuilder<A, G, S> {
        // The policy is composed per run by `configure_run`, which binds it to the run's
        // audit trail; a policy composed here would carry none.
        let mut config = RunConfig::default();
        if let Some(handler) = &self.runtime.handler {
            config.tool_confirmation_handler = Some(handler.clone());
        }
        self.runtime.layer().apply_with(&mut config, false);
        let mut builder =
            builder.run_config(config).approval_store(self.runtime.approval_store.clone());
        if let Some(trail) = &self.runtime.audit {
            builder = builder.audit_log(trail.recorder().clone());
        }
        if let Some(compaction) = &self.runtime.compaction {
            builder = builder.compaction_config(compaction.clone());
        }
        // Explicit context caching for a model that supports it (spec 3.1 §7.23).
        if let Some(model) = &self.runtime.cache_capable {
            builder = builder
                .cache_capable(model.clone())
                .context_cache_config(adk_core::ContextCacheConfig::default());
        }
        builder
    }

    /// The reasoning effort of each mode and model role (spec 3.1 §7.23).
    pub fn effort(&self) -> &crate::posture::EffortPolicy {
        &self.runtime.effort
    }

    /// The todo list of `session` (spec 3.1 §7.6).
    pub fn todos(&self, session: &dyn Session) -> TodoList {
        TodoList::from_state(session.state())
    }

    /// Wraps the harness in a [`Harness`](crate::Harness) that owns a `Runner` over
    /// `sessions` (spec 3.1 §7.3).
    ///
    /// # Errors
    ///
    /// Returns the runner's error when `app_name` is invalid.
    #[cfg(feature = "runner")]
    pub fn into_harness(
        self,
        app_name: impl Into<String>,
        sessions: Arc<dyn adk_session::SessionService>,
    ) -> Result<crate::Harness> {
        crate::Harness::new(Arc::new(self), app_name.into(), sessions)
    }
}

#[async_trait]
impl Agent for HarnessAgent {
    // The builder name, with or without members: the acting principal, fingerprints and
    // standing rules stay the same when a team is added (spec 3.1 §7.10).
    fn name(&self) -> &str {
        &self.info.name
    }

    fn description(&self) -> &str {
        self.root.description()
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        self.root.sub_agents()
    }

    fn topology(&self) -> Option<AgentTopology> {
        self.root.topology()
    }

    // The root consumes the run's runtime toolsets under the harness's policy, so a
    // `TeamSpec` whose coordinator is a harness compiles its delegate edges (spec 3.5
    // §7.19); handoff follows the harness's own transfer support.
    fn capabilities(&self) -> AgentCapabilities {
        AgentCapabilities { handoff: self.supports_agent_transfer(), ..self.root.capabilities() }
    }

    // With members, every turn starts at the harness, never at a member the runner found
    // in history: members run only as isolated delegations (spec 3.1 §7.10).
    fn supports_agent_transfer(&self) -> bool {
        self.info.members.is_empty() && self.root.supports_agent_transfer()
    }

    fn transfer_targets_for(&self, agent_name: &str) -> Option<Vec<String>> {
        self.root.transfer_targets_for(agent_name)
    }

    fn configure_run(&self, agent_name: &str, config: &mut RunConfig) {
        self.root.configure_run(agent_name, config);
        self.runtime.layer().apply(config);
    }

    async fn close(&self) {
        self.root.close().await;
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        // A partition's harness runs only its own partition's sessions (spec 3.2 K-I15).
        #[cfg(feature = "kernel")]
        let mark = match self.partition_wiring() {
            Some(wiring) => wiring.session_mark(ctx.as_ref(), self.name())?,
            None => None,
        };
        // A thread belongs to one agent: the agent's harness stamps it on its first run and
        // every other agent refuses it (spec 3.3 §5.7 "Threads").
        #[cfg(feature = "kernel")]
        let stamp = match &self.runtime.session_stamp {
            Some(stamp) => crate::agents::stamp_event(ctx.as_ref(), stamp, self.name())?,
            None => None,
        };
        // A run that starts in Ask or Auto with a provider-executed web tool declared
        // records that an ungoverned fetch path was live (spec 3.1 §7.20, I49).
        #[cfg(feature = "web")]
        if let Some(provider_web) = &self.runtime.provider_web {
            provider_web.record_run_start(ctx.as_ref()).await?;
        }
        // Operator-declared MCP servers start with the first run; workspace-declared ones
        // only under workspace trust (spec 3.1 §7.21, I3).
        #[cfg(feature = "mcp")]
        if let Some(mcp) = &self.runtime.mcp {
            mcp.ensure_started(self.runtime.workspace_trusted()).await;
        }
        // The workspace as it was before this turn, so /rewind can restore it (spec 3.1 §7.27).
        if let Some(snapshots) = &self.runtime.snapshots {
            #[cfg(feature = "graph")]
            let plan = match &self.runtime.plan_graph {
                Some(graphs) => graphs
                    .mark(ctx.app_name(), ctx.user_id(), ctx.session_id())
                    .await
                    .ok()
                    .flatten(),
                None => None,
            };
            #[cfg(not(feature = "graph"))]
            let plan = None;
            crate::snapshot::before_turn(snapshots, ctx.as_ref(), plan).await;
        }
        let events = self.run_governed(ctx.clone()).await?;
        // What a provider-executed search ran is recorded after each response
        // (spec 3.1 §7.20).
        #[cfg(feature = "web")]
        let events = match &self.runtime.provider_web {
            Some(provider_web) => provider_web.record_searches(ctx.clone(), events),
            None => events,
        };
        let events = self.with_mode_mirror(&ctx, events).await;
        #[cfg(feature = "kernel")]
        let events = match mark {
            Some(mark) => {
                use futures::StreamExt as _;
                Box::pin(futures::stream::iter([Ok(mark)]).chain(events)) as EventStream
            }
            None => events,
        };
        #[cfg(feature = "kernel")]
        let events = match stamp {
            Some(stamp) => {
                use futures::StreamExt as _;
                Box::pin(futures::stream::iter([Ok(stamp)]).chain(events)) as EventStream
            }
            None => events,
        };
        Ok(events)
    }
}

impl HarnessAgent {
    /// Keeps the `harness:mode` display mirror equal to the mode store (spec 3.1 §6 D-3):
    /// re-asserts it when the run starts and replaces any value an event of the run writes,
    /// so neither a client `stateDelta` nor a tool's `state_delta` misreports the mode.
    async fn with_mode_mirror(
        &self,
        ctx: &Arc<dyn InvocationContext>,
        events: EventStream,
    ) -> EventStream {
        use futures::StreamExt as _;
        let store = self.runtime.mode_store.clone();
        let default_mode = self.info.default_mode;
        let identity =
            (ctx.app_name().to_string(), ctx.user_id().to_string(), ctx.session_id().to_string());
        let mirror = move || {
            let store = store.clone();
            let (app, user, session) = identity.clone();
            async move {
                crate::mode::mode_mirror(store.as_ref(), default_mode, (&app, &user, &session))
                    .await
            }
        };
        // A session that never had a mirror needs none; one that disagrees with the store
        // was written by something other than the harness.
        let shown = ctx.session().state().get(crate::mode::MODE_STATE_KEY);
        let prelude = match (shown, mirror().await) {
            (Some(shown), Some(value)) if shown != value => {
                let mut event = adk_core::Event::new(ctx.invocation_id());
                event.author = self.name().to_string();
                event.branch = ctx.branch().to_string();
                event.actions.state_delta.insert(crate::mode::MODE_STATE_KEY.to_string(), value);
                Some(Ok(event))
            }
            _ => None,
        };
        let events = events.then(move |event| {
            let mirror = mirror();
            async move {
                let mut event = event?;
                if event.actions.state_delta.contains_key(crate::mode::MODE_STATE_KEY) {
                    match mirror.await {
                        Some(value) => {
                            event
                                .actions
                                .state_delta
                                .insert(crate::mode::MODE_STATE_KEY.to_string(), value);
                        }
                        None => {
                            event.actions.state_delta.remove(crate::mode::MODE_STATE_KEY);
                        }
                    }
                }
                Ok(event)
            }
        });
        Box::pin(futures::stream::iter(prelude).chain(events))
    }

    async fn run_governed(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let config = ctx.run_config();
        // Bound to this harness's own layer, not to a policy that names itself the harness.
        if config.tool_policy.as_deref().and_then(composed_layer_owner)
            != Some(self.runtime.policy.owner_id())
        {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Forbidden,
                HARNESS_UNGOVERNED_CODE,
                format!(
                    "harness '{}' refuses to run without its policy in the run config; run it as \
                     the root agent of a Runner, or apply HarnessAgent::run_config()",
                    self.name()
                ),
            ));
        }
        #[cfg(feature = "auth")]
        {
            let blocking = config.audit.as_ref().is_some_and(|trail| {
                trail.failure_mode() == adk_core::audit_log::AuditFailureMode::Block
            });
            if !blocking {
                return Err(crate::identity::audit_required(self.name()));
            }
            self.runtime.identity.observe(ctx.as_ref());
        }
        if ctx.user_id() != ctx.session().user_id() {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Forbidden,
                "harness.identity_mismatch",
                "the run's user is not the session's user",
            ));
        }
        // Long-term memory reads the run's audit trail and budget, and counts user turns
        // for distillation (spec 3.1 §7.15.2).
        #[cfg(feature = "memory")]
        if let Some(facts) = self.runtime.toolset.facts() {
            facts.observe_invocation(ctx.as_ref());
        }
        // The payments reminder names the run's remaining budget (spec 3.1 §7.16).
        #[cfg(feature = "payments")]
        if let Some(payments) = &self.runtime.payments {
            payments.observe_invocation(ctx.as_ref());
        }
        if let Some(trail) = config.audit.clone() {
            let state = &self.runtime.instructions;
            let trusted = state.trusted();
            let audit =
                HarnessAudit::new(trail, AuditActor::for_context(ctx.as_ref()), ctx.session_id());
            // A trust decision made on other files than these no longer applies (spec 3.4 P2).
            if let Ok(Some(trust)) = state.trust_store.get(state.instructions.root_digest())
                && trust.is_trusted()
                && !trusted
            {
                tracing::warn!(
                    "the workspace's instruction files changed since they were trusted; they are not loaded"
                );
                audit
                    .record(
                        "harness.instructions.changed",
                        json!({
                            "files": state.instructions.audit_files(false),
                            "rootDigest": state.instructions.root_digest(),
                            "trustedDigest": trust.instructions_digest,
                            "currentDigest": state.instructions.instructions_digest(),
                            "by": trust.by,
                        }),
                    )
                    .await?;
            }
            audit
                .record(
                    "harness.instructions.loaded",
                    json!({
                        "files": state.instructions.audit_files(trusted),
                        "rootDigest": state.instructions.root_digest(),
                        "trusted": trusted,
                        "nested": state.instructions.nested().len(),
                    }),
                )
                .await?;
        }
        // OnSessionStart hooks run once per session; their notes join the <environment> block
        // of every run of the session (spec 3.1 §7.22).
        #[cfg(feature = "hooks")]
        if let Some(hooks) = &self.runtime.hooks {
            let mode = self
                .runtime
                .policy
                .mode_for(ctx.app_name(), ctx.user_id(), ctx.session_id(), ctx.invocation_id())
                .await;
            let scope = crate::hooks::HookScope::from_invocation(ctx.as_ref());
            let audit = config.audit.clone().map(|trail| {
                HarnessAudit::new(trail, AuditActor::for_context(ctx.as_ref()), ctx.session_id())
            });
            hooks.on_session_start(mode, &scope, audit.as_ref()).await;
        }
        #[cfg(feature = "observe")]
        let events = {
            let mode = crate::observe::run_mode(
                self.runtime.mode_store.as_ref(),
                self.info.default_mode,
                ctx.as_ref(),
            )
            .await;
            let span = crate::observe::run_span(
                self.name(),
                ctx.invocation_id(),
                mode,
                &self.info.isolation,
            );
            let events = {
                use tracing::Instrument as _;
                self.turn(ctx.clone()).instrument(span.clone()).await?
            };
            crate::observe::observe_run(
                events,
                &ctx,
                span,
                self.runtime.spend.clone(),
                self.runtime.run_trails.clone(),
                self.runtime.effort.for_mode(mode),
            )
        };
        #[cfg(not(feature = "observe"))]
        let events = self.turn(ctx.clone()).await?;
        // OnHold and OnStop hooks observe the turn; their notes reach the user as events
        // without content, never the model (spec 3.1 §7.22).
        #[cfg(feature = "hooks")]
        let events = match &self.runtime.hooks {
            Some(hooks) => {
                let mode = self
                    .runtime
                    .policy
                    .mode_for(ctx.app_name(), ctx.user_id(), ctx.session_id(), ctx.invocation_id())
                    .await;
                hooks.observe(events, ctx.clone(), mode, self.name().to_string())
            }
            None => events,
        };
        Ok(events)
    }
}

/// An agent run as a runner root under a harness's layer (spec 3.1 §7.10); built by
/// [`HarnessAgent::governed_root`].
struct GovernedRoot {
    runtime: HarnessRuntime,
    root: Arc<dyn Agent>,
    default_mode: HarnessMode,
    isolation: crate::shell::IsolationStatus,
}

#[async_trait]
impl Agent for GovernedRoot {
    fn name(&self) -> &str {
        self.root.name()
    }

    fn description(&self) -> &str {
        self.root.description()
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        self.root.sub_agents()
    }

    fn supports_agent_transfer(&self) -> bool {
        false
    }

    fn configure_run(&self, agent_name: &str, config: &mut RunConfig) {
        self.root.configure_run(agent_name, config);
        self.runtime.layer().apply(config);
    }

    async fn close(&self) {
        self.root.close().await;
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let config = ctx.run_config();
        if config.tool_policy.as_deref().and_then(composed_layer_owner)
            != Some(self.runtime.policy.owner_id())
        {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Forbidden,
                HARNESS_UNGOVERNED_CODE,
                format!(
                    "'{}' runs only under its harness's policy; run it as the root agent of a \
                     Runner, whose configure_run applies the harness layer",
                    self.root.name()
                ),
            ));
        }
        #[cfg(feature = "auth")]
        if !config.audit.as_ref().is_some_and(|trail| {
            trail.failure_mode() == adk_core::audit_log::AuditFailureMode::Block
        }) {
            return Err(crate::identity::audit_required(self.root.name()));
        }
        #[cfg(feature = "observe")]
        {
            use tracing::Instrument as _;
            let mode = crate::observe::run_mode(
                self.runtime.mode_store.as_ref(),
                self.default_mode,
                ctx.as_ref(),
            )
            .await;
            let span = crate::observe::run_span(
                self.root.name(),
                ctx.invocation_id(),
                mode,
                &self.isolation,
            );
            let events = self.root.run(ctx.clone()).instrument(span.clone()).await?;
            Ok(crate::observe::observe_run(
                events,
                &ctx,
                span,
                self.runtime.spend.clone(),
                self.runtime.run_trails.clone(),
                self.runtime.effort.for_mode(mode),
            ))
        }
        #[cfg(not(feature = "observe"))]
        {
            let _ = (self.default_mode, &self.isolation);
            self.root.run(ctx).await
        }
    }
}

impl HarnessAgent {
    /// One turn: the session's running goal, then its running plan, step by step, else the
    /// harness agent (spec 3.1 §7.11).
    async fn turn(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let mode = self
            .runtime
            .policy
            .mode_for(ctx.app_name(), ctx.user_id(), ctx.session_id(), ctx.invocation_id())
            .await;
        if let Some(stream) =
            self.runtime.goals.route(mode, &self.info.name, self.root.clone(), ctx.clone()).await?
        {
            return Ok(stream);
        }
        #[cfg(feature = "graph")]
        if let Some(graphs) = &self.runtime.plan_graph
            && let Some(stream) = graphs.route(mode, self.root.clone(), ctx.clone()).await?
        {
            return Ok(stream);
        }
        self.root.run(ctx).await
    }
}

/// The error of a standing rule that cannot be created for a call.
fn rule_refused(message: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::InvalidInput,
        "harness.decision.invalid",
        message,
    )
}

/// Standing rules created and revoked by a person outside a prompt: `Harness::decide`, a
/// served harness's authenticated REST routes (spec 3.1 §7.5, §8.2).
impl HarnessAgent {
    /// The rule `suggestion` becomes for the held call `request`, owned by `owner`
    /// (spec 3.1 §7.5, §9.2 I2, I8).
    ///
    /// The suggestion must pin the call's tool and exact declaration; an allow rule must be
    /// one [`StandingRule::suggestions`](crate::StandingRule::suggestions) offers for the
    /// call; a call no rule covers — plan exit, a payment, a memory write, a desktop action,
    /// a protected or other non-waivable edit — gets none.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let suggestion = StandingRule::suggestions(&held.request).remove(0);
    /// let rule = harness.rule_for_held_call(&held.request, suggestion, StandingDecision::Allow, &owner)?;
    /// harness.grant_rule(rule, "rest", Some(&held.request)).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `harness.decision.invalid` when the call records no tool origin or no rule
    /// may be created from `suggestion` for it.
    pub fn rule_for_held_call(
        &self,
        request: &adk_core::ToolConfirmationRequest,
        suggestion: crate::RuleSuggestion,
        decision: crate::StandingDecision,
        owner: &crate::RuleOwner,
    ) -> Result<crate::StandingRule> {
        let runtime = &self.runtime;
        let origin = request
            .origin
            .as_ref()
            .ok_or_else(|| rule_refused("the pending call records no tool origin"))?;
        let kind = runtime.policy.registry().classify(
            &self.info.name,
            &request.tool_name,
            Some(origin),
            &request.args,
            adk_core::ToolEffect::NonIdempotent,
            runtime.policy.isolation_status().isolation(),
            false,
        );
        let protected = kind == crate::ToolKind::Edit
            && crate::edits::edit_paths(&request.tool_name, &request.args).is_none_or(|paths| {
                paths.is_empty()
                    || paths.iter().any(|path| is_protected_target(runtime.workspace.root(), path))
            });
        if !kind.rule_eligible() || crate::TableRow::of(&kind, protected).is_non_waivable() {
            return Err(rule_refused("no standing rule applies to this call; decide it once"));
        }
        // "Always allow for this folder": the harness's file-writing tools, each pinned to
        // its declaration (spec 3.3 §8.2).
        if self.folder_suggestion(request).as_ref() == Some(&suggestion) {
            return Ok(suggestion.into_rule(owner, decision, Some(request.fingerprint())));
        }
        if !suggestion.origin.admits(origin)
            || !matches!(suggestion.origin, crate::OriginPin::Exact { .. })
            || suggestion.tool != request.tool_name
        {
            return Err(rule_refused(
                "a remembered rule must pin this call's tool and declaration",
            ));
        }
        if decision == crate::StandingDecision::Allow
            && !crate::StandingRule::suggestions(request).contains(&suggestion)
        {
            return Err(rule_refused("only a suggested rule can be remembered as an allow rule"));
        }
        Ok(suggestion.into_rule(owner, decision, Some(request.fingerprint())))
    }

    /// The rule "Always allow for this folder" makes for the held call `request`
    /// (spec 3.3 §8.2): every call of this harness's file-writing tools — `write_file`,
    /// `edit_file`, and an `apply_patch` that adds files or updates them in place — whose
    /// paths all lie within the folder `request` writes in, each tool pinned to its exact
    /// declaration, for the session. Paths outside the folder, `..` and symlink escapes,
    /// protected paths, deletes, moves, the shell and every other tool still ask.
    ///
    /// `None` when `request` is not a file write of this harness's own tools: another tool,
    /// another declaration, a delete or a move, a protected path or a path with `..`.
    /// [`rule_for_held_call`](Self::rule_for_held_call) makes the rule from it.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let folder = harness.folder_suggestion(&held.request).expect("a file write");
    /// assert_eq!(folder.label, "file writes: within notes/");
    /// let rule = harness.rule_for_held_call(&held.request, folder, StandingDecision::Allow, &owner)?;
    /// harness.grant_rule(rule, "web", Some(&held.request)).await?;
    /// ```
    pub fn folder_suggestion(
        &self,
        request: &adk_core::ToolConfirmationRequest,
    ) -> Option<crate::RuleSuggestion> {
        crate::StandingRule::folder_suggestion(request, |name| self.runtime.toolset.origin_of(name))
    }

    /// Stores `rule`, recording `harness.rule.created` with `surface` (such as `rest`) in
    /// the harness's audit trail before the store write (spec 3.1 §7.5, §9.3, I39).
    /// `from` is the held call the rule was created from, if any.
    ///
    /// The caller authenticates the person the rule's scope and `created_by` name; a rule
    /// is never created for a model-callable tool's request (spec 3.1 §9.2 I2, I5).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let granted = harness.grant_rule(rule, "rest", None).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the audit log's error, and then stores nothing, or the store's error.
    pub async fn grant_rule(
        &self,
        rule: crate::StandingRule,
        surface: &str,
        from: Option<&adk_core::ToolConfirmationRequest>,
    ) -> Result<crate::StandingRule> {
        if let Some(trail) = self.runtime.audit_trail() {
            let mut fields = crate::approvals::rule_audit_fields(
                &rule,
                from.and_then(|request| request.function_call_id.as_deref()),
            );
            fields["surface"] = json!(surface);
            HarnessAudit::new(
                trail.clone(),
                AuditActor::subject(rule.created_by.clone()),
                rule.scope.session_id().unwrap_or_default(),
            )
            .record("harness.rule.created", fields)
            .await?;
        }
        self.runtime.standing_rules.grant(rule).await
    }

    /// Revokes the rule `id` of `user` in `app`, recording `harness.rule.revoked` with
    /// `surface` before the store write (spec 3.1 §7.5, §9.2 I4, §9.3). Returns the revoked
    /// rule, or `None` when `user` has no rule `id`, so a person revokes only their own.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let revoked = harness.revoke_rule("adk-rust", "ana", &rule.id, "rest").await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the audit log's error, and then revokes nothing, or the store's error.
    pub async fn revoke_rule(
        &self,
        app: &str,
        user: &str,
        id: &str,
        surface: &str,
    ) -> Result<Option<crate::StandingRule>> {
        let standing = &self.runtime.standing_rules;
        let Some(rule) = standing.list(app, user).await?.into_iter().find(|rule| rule.id == id)
        else {
            return Ok(None);
        };
        if let Some(trail) = self.runtime.audit_trail() {
            let mut fields = crate::approvals::rule_audit_fields(&rule, None);
            fields["by"] = json!(user);
            fields["surface"] = json!(surface);
            HarnessAudit::new(
                trail.clone(),
                AuditActor::subject(adk_core::principal::SubjectRef::human(user)),
                rule.scope.session_id().unwrap_or_default(),
            )
            .record("harness.rule.revoked", fields)
            .await?;
        }
        Ok(standing.revoke(app, id).await?.then_some(rule))
    }

    /// Moves the end of the rule `id` of `user` in `app` to `until`, recording
    /// `harness.rule.extended` with `surface`, the old end and the new one before the store
    /// write (spec 3.3 §8.1 "Allowed without asking — Extend", spec 3.1 §9.3). Returns the
    /// rule after it, or `None` when `user` has no rule `id` or it was revoked meanwhile: an
    /// extension never brings a revoked rule back.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let until = chrono::Utc::now() + chrono::Duration::days(30);
    /// let extended = harness.extend_rule("adk-rust", "ana", &rule.id, until, "rest").await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `harness.rule.invalid` when the rule has no end date, or `until` is not later
    /// than both its end and now; the audit log's error, and then changes nothing; or the
    /// store's error.
    pub async fn extend_rule(
        &self,
        app: &str,
        user: &str,
        id: &str,
        until: chrono::DateTime<chrono::Utc>,
        surface: &str,
    ) -> Result<Option<crate::StandingRule>> {
        let standing = &self.runtime.standing_rules;
        let Some(rule) = standing.list(app, user).await?.into_iter().find(|rule| rule.id == id)
        else {
            return Ok(None);
        };
        let invalid = |message: String| {
            AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                "harness.rule.invalid",
                message,
            )
        };
        let Some(end) = rule.expires_at else {
            return Err(invalid(
                "this rule has no end date: it applies until it is stopped, so there is \
                 nothing to extend"
                    .into(),
            ));
        };
        let earliest = end.max(chrono::Utc::now());
        if until <= earliest {
            return Err(invalid(format!(
                "a rule is extended to a later end than {}; stop allowing it to end it sooner",
                earliest.to_rfc3339()
            )));
        }
        if let Some(trail) = self.runtime.audit_trail() {
            let mut fields = crate::approvals::rule_audit_fields(&rule, None);
            fields["by"] = json!(user);
            fields["surface"] = json!(surface);
            fields["from"] = json!(end);
            fields["to"] = json!(until);
            HarnessAudit::new(
                trail.clone(),
                AuditActor::subject(adk_core::principal::SubjectRef::human(user)),
                rule.scope.session_id().unwrap_or_default(),
            )
            .record("harness.rule.extended", fields)
            .await?;
        }
        let extended = crate::StandingRule { expires_at: Some(until), ..rule };
        Ok(standing.replace(extended.clone()).await?.then_some(extended))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nw() -> BTreeSet<String> {
        ["exit_plan_mode".to_string()].into_iter().collect()
    }

    #[test]
    fn non_waivable_fingerprints_are_recognized() {
        let key = |tool: &str, args: &str| {
            format!("agent\u{1f}static\u{1f}sha256:x\u{1f}{tool}\u{1f}{args}")
        };
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let strip = |fingerprint: &str| names_non_waivable(fingerprint, &nw(), &[], &root);
        assert!(strip(&key("exit_plan_mode", "{}")));
        assert!(strip(&key("bash", r#"{"command":"ls","unsandboxed":true}"#)));
        assert!(!strip(&key("bash", r#"{"command":"ls"}"#)));
        assert!(strip(&key("write_file", r#"{"path":"AGENTS.md"}"#)));
        assert!(!strip(&key("write_file", r#"{"path":"src/a.rs"}"#)));
        let patch = |paths: &[&str]| {
            let body: String =
                paths.iter().map(|path| format!("*** Add File: {path}\n+x\n")).collect();
            serde_json::json!({ "patch": format!("*** Begin Patch\n{body}*** End Patch") })
                .to_string()
        };
        assert!(!strip(&key("apply_patch", &patch(&["src/a.rs", "src/b.rs"]))));
        assert!(strip(&key("apply_patch", &patch(&["src/a.rs", "AGENTS.md"]))));
        assert!(strip(&key("apply_patch", r#"{"patch":"not a patch"}"#)));
        assert!(strip("garbage"));
    }

    #[test]
    fn a_crossing_tool_listed_at_request_time_is_non_waivable_by_its_prefix() {
        let key =
            |tool: &str| format!("agent\u{1f}driver:notes\u{1f}sha256:x\u{1f}{tool}\u{1f}{{}}");
        let root = std::path::Path::new("/");
        let prefixes = ["work__".to_string()];
        assert!(names_non_waivable(&key("work__read_note"), &nw(), &prefixes, root));
        assert!(!names_non_waivable(&key("read_note"), &nw(), &prefixes, root));
        assert!(!names_non_waivable(&key("home__read_note"), &nw(), &prefixes, root));
    }

    #[cfg(unix)]
    #[test]
    fn an_approval_of_a_write_through_a_symlink_to_a_protected_file_is_stripped() {
        let key =
            |args: &str| format!("agent\u{1f}static\u{1f}sha256:x\u{1f}write_file\u{1f}{args}");
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("AGENTS.md"), "rules").unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::create_dir(root.join("docs")).unwrap();
        std::os::unix::fs::symlink("../AGENTS.md", root.join("docs/notes.md")).unwrap();
        std::os::unix::fs::symlink(".git", root.join("cfg")).unwrap();
        assert!(names_non_waivable(&key(r#"{"path":"docs/notes.md"}"#), &nw(), &[], &root));
        assert!(names_non_waivable(&key(r#"{"path":"cfg/hooks/pre-commit"}"#), &nw(), &[], &root));
        assert!(!names_non_waivable(&key(r#"{"path":"docs/other.md"}"#), &nw(), &[], &root));
    }
}
