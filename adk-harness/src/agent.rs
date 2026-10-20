//! `HarnessAgent`, its runtime, and the run-config layer (spec 3.1 §7.3).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditActor;
use adk_core::{
    AdkError, Agent, AgentTopology, ApprovalService, ApprovalStore,
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
use crate::policy::{HarnessPolicy, has_harness_layer};
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
            Ok(Some(trust)) => trust.is_trusted(),
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
    pub(crate) handler: Option<Arc<dyn ToolConfirmationHandler>>,
    pub(crate) confirmation_timeout: Duration,
    pub(crate) budget: Option<RunBudget>,
    pub(crate) spend_ledger: Option<Arc<dyn SpendLedger>>,
    pub(crate) history_max_events: Option<usize>,
    pub(crate) compaction: Option<EventsCompactionConfig>,
    pub(crate) toolset: Arc<HarnessToolset>,
    pub(crate) instructions: Arc<InstructionState>,
    pub(crate) non_waivable_tools: BTreeSet<String>,
    pub(crate) workspace: Workspace,
    pub(crate) audit: Option<adk_core::audit_log::AuditTrail>,
    #[cfg(feature = "auth")]
    pub(crate) identity: Arc<crate::identity::IdentityGate>,
    #[cfg(feature = "observe")]
    pub(crate) spend: Arc<crate::observe::SpendAccounts>,
    #[cfg(feature = "observe")]
    pub(crate) run_trails: Arc<crate::observe::RunTrails>,
}

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
}

/// What `HarnessAgent::configure_run` applies to a run's config (spec 3.1 §7.3).
///
/// | Rule | Effect |
/// |---|---|
/// | Compose, never replace | the run's policy and the operator policy are kept; the strictest decision wins |
/// | Idempotent | a config that already carries the harness policy is left as it is |
/// | Approvals | the approval service and store are filled only when empty or identical; otherwise the run's own are kept with a warning |
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
    root: &std::path::Path,
) -> bool {
    let parts: Vec<&str> = fingerprint.splitn(5, '\u{1f}').collect();
    if parts.iter().any(|part| non_waivable.contains(*part)) {
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
        "write_file" | "edit_file" => args
            .get("path")
            .and_then(Value::as_str)
            .is_none_or(|path| is_protected_target(root, path)),
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
        if compose_policy && !config.tool_policy.as_deref().is_some_and(has_harness_layer) {
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
            layers
                .push(Arc::new(runtime.policy.with_audit(config.audit.clone()).with_peers(peers)));
            config.tool_policy = Some(Arc::new(adk_core::StrictestPolicy::new(layers)));
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

        let before = config.tool_approvals.len();
        config.tool_approvals.retain(|fingerprint, _| {
            !names_non_waivable(fingerprint, &runtime.non_waivable_tools, runtime.workspace.root())
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
            if config.budget_tracker.is_none() {
                config.budget_tracker =
                    Some(Arc::new(adk_core::BudgetTracker::new(budget.clone())));
            }
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

    /// Applies the harness run config, approval store and compaction to a runner builder.
    /// Call it before setting a run config of your own on the builder.
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
        builder
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
    fn name(&self) -> &str {
        self.root.name()
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
        let config = ctx.run_config();
        if !config.tool_policy.as_deref().is_some_and(has_harness_layer) {
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
        if let Some(trail) = config.audit.clone() {
            let state = &self.runtime.instructions;
            let trusted = state.trusted();
            HarnessAudit::new(trail, AuditActor::for_context(ctx.as_ref()), ctx.session_id())
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
        #[cfg(feature = "observe")]
        {
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
                self.root.run(ctx.clone()).instrument(span.clone()).await?
            };
            return Ok(crate::observe::observe_run(
                events,
                &ctx,
                span,
                self.runtime.spend.clone(),
                self.runtime.run_trails.clone(),
            ));
        }
        #[cfg(not(feature = "observe"))]
        self.root.run(ctx).await
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
        let strip = |fingerprint: &str| names_non_waivable(fingerprint, &nw(), &root);
        assert!(strip(&key("exit_plan_mode", "{}")));
        assert!(strip(&key("bash", r#"{"command":"ls","unsandboxed":true}"#)));
        assert!(!strip(&key("bash", r#"{"command":"ls"}"#)));
        assert!(strip(&key("write_file", r#"{"path":"AGENTS.md"}"#)));
        assert!(!strip(&key("write_file", r#"{"path":"src/a.rs"}"#)));
        assert!(strip("garbage"));
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
        assert!(names_non_waivable(&key(r#"{"path":"docs/notes.md"}"#), &nw(), &root));
        assert!(names_non_waivable(&key(r#"{"path":"cfg/hooks/pre-commit"}"#), &nw(), &root));
        assert!(!names_non_waivable(&key(r#"{"path":"docs/other.md"}"#), &nw(), &root));
    }
}
