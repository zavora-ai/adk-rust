//! `bash` as the harness declares and governs it (spec 3.1 §7.9).

use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use adk_devtools::{BashTool, HostLauncher, Workspace};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::audit::{HARNESS_AUDIT_KIND, HarnessAudit};
use crate::mode::{HarnessMode, ModeStore};
use crate::policy::has_harness_layer;
#[cfg(feature = "sandbox")]
use crate::shell::SandboxLauncher;
use crate::shell::{
    FileToolGate, IsolationStatus, ShellLedger, ShellLedgerEntry, sandbox_unavailable,
};

/// How often a running command checks the run's kill switch (spec 3.1 §7.9).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::GOVERNANCE_POLL_INTERVAL, std::time::Duration::from_millis(250));
/// ```
pub const GOVERNANCE_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The MSYS2 runtime's fatal error when it cannot write its namespace: under the Windows
/// sandbox, a program outside MSYS2 that starts an installation's MSYS2 program itself, as
/// `git` starts a hook's `sh`, meets the namespace the call holds (spec 3.2 §10 W5).
const MSYS2_NAMESPACE_HELD: &str = "NtCreateDirectoryObject(";

/// Output that suggests the sandbox refused a command (spec 3.1 §7.9): permission and
/// read-only errors, Seatbelt's `deny(1)` log line, an unreachable network, and an MSYS2
/// program the Windows sandbox kept from the user's MSYS2 namespace.
const DENIAL_MARKERS: &[&str] = &[
    MSYS2_NAMESPACE_HELD,
    "Operation not permitted",
    "Read-only file system",
    "deny(1)",
    "Network is unreachable",
    "Could not resolve host",
    "Couldn't resolve host",
    "nodename nor servname provided",
    "Temporary failure in name resolution",
    "Name or service not known",
];

/// Whether command output suggests the sandbox refused something (spec 3.1 §7.9).
pub(crate) fn denial_suspected(output: &str) -> bool {
    DENIAL_MARKERS.iter().any(|marker| output.contains(marker))
}

/// The sandboxed runners of a `bash` tool over an active OS sandbox.
#[cfg(feature = "sandbox")]
struct SandboxedBash {
    launcher: SandboxLauncher,
    read_write: BashTool,
    read_only: BashTool,
}

/// What one call runs through.
struct Selected<'a> {
    bash: &'a BashTool,
    sandboxed: bool,
    read_only: bool,
    env_keys: Vec<String>,
}

/// The harness `bash` tool (spec 3.1 §7.9).
///
/// Runs commands through the devtools `BashTool` and a `CommandLauncher` (spec 3.1 §10 D1):
/// `HostLauncher` on the host, [`SandboxLauncher`](crate::SandboxLauncher) under an active OS
/// sandbox — read-write in Ask and Auto, read-only in Plan and ReadOnly. The devtools tool
/// keeps its timeout ceiling, output cap, streaming, empty standard input, new session and
/// process-group kill (D3). The harness tool adds:
///
/// - its own declaration `{ command, timeout_secs? }`, plus `unsandboxed`, `reason` and
///   `retry_of` when a sandbox is active and the unsandboxed retry is on;
/// - `annotations()` of `{enforcer, profileDigest, network}`, so the declaration digest —
///   and every standing rule or held call bound to it — differs per isolation (I9);
/// - a refusal to run unless the harness policy is in the run's config (I17);
/// - a per-call probe of the sandbox, refusing rather than falling back to the host (I24);
/// - a check of the run's kill switch every 250 ms, killing the command's process group on
///   a freeze (I20);
/// - on a failed sandboxed command, `sandbox: { enforcer, denialSuspected, hint }` in the
///   result;
/// - the unsandboxed retry of a failed sandboxed call, on the host, consumed once (I25);
/// - `harness.bash.begin` (write-ahead), `harness.bash.end` and
///   `harness.bash.retry_unsandboxed` audit records;
/// - a per-session shell ledger entry for every call, which unsandboxed retries consume.
///
/// # Example
///
/// ```rust
/// use adk_core::Tool;
/// use adk_harness::{HarnessBashTool, InMemoryShellLedger, IsolationStatus};
/// use std::sync::Arc;
///
/// let ws = adk_devtools::Workspace::new(".").allow_bash(true);
/// let bash = HarnessBashTool::new(ws, IsolationStatus::Host, false, Arc::new(InMemoryShellLedger::new()));
/// assert_eq!(bash.name(), "bash");
/// assert_eq!(bash.annotations().unwrap()["enforcer"], "host");
/// assert!(bash.parameters_schema().unwrap()["properties"].get("unsandboxed").is_none());
/// ```
pub struct HarnessBashTool {
    workspace: Workspace,
    host: BashTool,
    #[cfg(feature = "sandbox")]
    sandbox: Option<SandboxedBash>,
    status: IsolationStatus,
    fail_if_unavailable: bool,
    allow_unsandboxed_retry: bool,
    ledger: Arc<dyn ShellLedger>,
    #[cfg(feature = "sandbox")]
    modes: Option<(Arc<dyn ModeStore>, HarnessMode)>,
    annotations: Value,
    description: String,
    /// Held exclusive while a sandboxed command runs unapproved (spec 3.1 §9.2 I26).
    file_gate: Option<FileToolGate>,
}

impl HarnessBashTool {
    /// A `bash` tool over `workspace`, running under `status`.
    ///
    /// The host and a degraded sandbox run commands through `HostLauncher`. An active OS
    /// sandbox needs its launcher from [`with_sandbox`](Self::with_sandbox); without one,
    /// every call is refused.
    pub fn new(
        workspace: Workspace,
        status: IsolationStatus,
        fail_if_unavailable: bool,
        ledger: Arc<dyn ShellLedger>,
    ) -> Self {
        let mut tool = Self {
            host: BashTool::new(workspace.clone()).with_launcher(Arc::new(HostLauncher)),
            #[cfg(feature = "sandbox")]
            sandbox: None,
            workspace,
            status,
            fail_if_unavailable,
            allow_unsandboxed_retry: true,
            ledger,
            #[cfg(feature = "sandbox")]
            modes: None,
            annotations: Value::Null,
            description: String::new(),
            file_gate: None,
        };
        tool.declare();
        tool
    }

    /// Runs each sandboxed command while no host file tool runs (spec 3.1 §9.2 I26).
    #[must_use]
    pub(crate) fn with_file_gate(mut self, gate: Option<FileToolGate>) -> Self {
        self.file_gate = gate;
        self
    }

    /// Runs sandboxed commands through `launcher` (spec 3.1 §7.9): a read-write copy in Ask
    /// and Auto, a read-only one in Plan and ReadOnly. Its profile digest and network setting
    /// go into the annotations.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use adk_core::Tool;
    /// use adk_harness::{HarnessBashTool, InMemoryShellLedger, OsSandboxConfig, SandboxLauncher};
    ///
    /// let launcher = SandboxLauncher::new(OsSandboxConfig::default());
    /// let ws = adk_devtools::Workspace::new(".").allow_bash(true);
    /// let bash = HarnessBashTool::new(ws, launcher.status(), true, Arc::new(InMemoryShellLedger::new()))
    ///     .with_sandbox(launcher);
    /// assert_eq!(bash.annotations().unwrap()["network"], false);
    /// ```
    #[cfg(feature = "sandbox")]
    #[must_use]
    pub fn with_sandbox(mut self, launcher: SandboxLauncher) -> Self {
        self.sandbox = Some(SandboxedBash {
            read_write: BashTool::new(self.workspace.clone())
                .with_launcher(Arc::new(launcher.clone())),
            read_only: BashTool::new(self.workspace.clone())
                .with_launcher(Arc::new(launcher.read_only())),
            launcher,
        });
        self.declare();
        self
    }

    /// Reads the session's mode from `store` (`default_mode` when it has no record) to give
    /// sandboxed commands a read-only workspace in Plan and ReadOnly. An unreadable store
    /// counts as ReadOnly. Without the `sandbox` feature there is no sandboxed workspace to
    /// restrict, and the store is not read.
    #[must_use]
    #[cfg_attr(not(feature = "sandbox"), allow(unused_mut))]
    pub fn with_mode_store(mut self, store: Arc<dyn ModeStore>, default_mode: HarnessMode) -> Self {
        #[cfg(feature = "sandbox")]
        {
            self.modes = Some((store, default_mode));
        }
        #[cfg(not(feature = "sandbox"))]
        let _ = (store, default_mode);
        self
    }

    /// Whether a failed sandboxed command may be retried unsandboxed (default `true`); the
    /// description and the failure hint say so.
    #[must_use]
    pub fn allow_unsandboxed_retry(mut self, yes: bool) -> Self {
        self.allow_unsandboxed_retry = yes;
        self.declare();
        self
    }

    fn sandbox_active(&self) -> bool {
        !self.status.behaves_as_host()
    }

    /// Recomputes the annotations and the description from the isolation.
    fn declare(&mut self) {
        let host = self.status.behaves_as_host();
        #[cfg(feature = "sandbox")]
        let (profile_digest, network, protects) = match &self.sandbox {
            Some(sandbox) => (
                Value::String(sandbox.launcher.profile_digest()),
                !sandbox.launcher.denies_network(),
                sandbox.launcher.keeps_read_only(),
            ),
            None => (Value::Null, host, true),
        };
        #[cfg(not(feature = "sandbox"))]
        let (profile_digest, network, protects) = (Value::Null, host, true);
        // Reads join the declaration beside the network, so a rule granted under one profile
        // never matches a weaker one (spec 3.1 §9.2 I9, spec 3.2 §10 W5).
        self.annotations = json!({
            "enforcer": self.status.enforcer(),
            "profileDigest": profile_digest,
            "network": network,
            "reads": if self.status.confines_reads() { "confined" } else { "unconfined" },
            "msys2Isolated": self.status.isolates_msys2(),
        });
        self.description = if host {
            "Run a shell command in the workspace root and return stdout, stderr and the exit \
             code. The command runs on the host with the user's full permissions and network \
             access, so each command may need the user's approval. Has a timeout."
                .to_string()
        } else {
            let network = if network { "network access" } else { "no network" };
            let retry = if self.allow_unsandboxed_retry {
                " If it fails because the sandbox denied something, you may retry it once with \
                 unsandboxed: true, retry_of: <the failed call id> and a reason; the user must \
                 approve that retry."
            } else {
                ""
            };
            let paths = if protects {
                "The workspace is writable (read-only in plan mode), but AGENTS.md, git's hooks \
                 and config, and other protected paths are read-only (the rest of .git is \
                 writable, so git commit works), nothing outside the workspace is writable, and \
                 HOME is a scratch directory."
            } else {
                "The workspace is writable, including .git and AGENTS.md; nothing outside it is \
                 writable, and HOME is a scratch directory."
            };
            format!(
                "Run a shell command in the workspace root inside an OS sandbox ({}) with {network}. \
                 {paths}{retry}",
                self.status.enforcer()
            )
        };
    }

    /// The mode governing this call, as the policy reads it.
    #[cfg(feature = "sandbox")]
    async fn mode_of(&self, ctx: &dyn ToolContext) -> HarnessMode {
        let Some((store, default_mode)) = &self.modes else { return HarnessMode::Ask };
        match store.get(ctx.app_name(), ctx.user_id(), ctx.session_id()).await {
            Ok(Some(record)) => record.mode_for_invocation(ctx.invocation_id()),
            Ok(None) => *default_mode,
            Err(error) => {
                tracing::warn!(error = %error, "mode store unavailable; running bash read-only");
                HarnessMode::ReadOnly
            }
        }
    }

    /// The sandboxed runner of this call, or why the sandbox cannot run it.
    #[cfg(feature = "sandbox")]
    async fn sandboxed(&self, ctx: &dyn ToolContext) -> std::result::Result<Selected<'_>, String> {
        let Some(sandbox) = &self.sandbox else {
            return Err("no sandbox launcher is configured for this bash tool".to_string());
        };
        if let IsolationStatus::Degraded { reason, .. } = sandbox.launcher.status() {
            return Err(reason);
        }
        let read_only =
            matches!(self.mode_of(ctx).await, HarnessMode::ReadOnly | HarnessMode::Plan);
        let env_keys = sandbox
            .launcher
            .scratch_dir(ctx.app_name(), ctx.user_id(), ctx.session_id())
            .map(|scratch| {
                sandbox.launcher.environment(&self.workspace, &scratch).into_keys().collect()
            })
            .unwrap_or_default();
        let bash = if read_only { &sandbox.read_only } else { &sandbox.read_write };
        Ok(Selected { bash, sandboxed: true, read_only, env_keys })
    }

    #[cfg(not(feature = "sandbox"))]
    async fn sandboxed(&self, _ctx: &dyn ToolContext) -> std::result::Result<Selected<'_>, String> {
        Err("adk-harness was built without the `sandbox` feature".to_string())
    }

    fn host_env_keys(&self) -> Vec<String> {
        if self.workspace.inherits_env() {
            vec!["*".to_string()]
        } else {
            self.workspace.bash_env().into_iter().map(|(key, _)| key).collect()
        }
    }

    /// Builds each command of every launcher — host, and the sandbox's read-write and
    /// read-only ones — through `layer`, such as the credential helper of spec 3.1 §7.14.
    #[cfg(feature = "auth")]
    #[must_use]
    pub(crate) fn with_launcher_layer(
        mut self,
        layer: impl Fn(Arc<dyn adk_devtools::CommandLauncher>) -> Arc<dyn adk_devtools::CommandLauncher>,
    ) -> Self {
        self.host =
            BashTool::new(self.workspace.clone()).with_launcher(layer(Arc::new(HostLauncher)));
        #[cfg(feature = "sandbox")]
        if let Some(sandbox) = self.sandbox.as_mut() {
            sandbox.read_write = BashTool::new(self.workspace.clone())
                .with_launcher(layer(Arc::new(sandbox.launcher.clone())));
            sandbox.read_only = BashTool::new(self.workspace.clone())
                .with_launcher(layer(Arc::new(sandbox.launcher.read_only())));
        }
        self
    }
}

fn refuse(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Tool, ErrorCategory::Forbidden, code, message)
}

#[async_trait]
impl Tool for HarnessBashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        let mut properties = json!({
            "command": { "type": "string", "description": "The shell command to run." },
            "timeout_secs": { "type": "integer", "description": "Optional timeout in seconds; cannot exceed the workspace limit." }
        });
        // Declared only when the retry is on, as the container shell does.
        if self.sandbox_active() && self.allow_unsandboxed_retry {
            properties["unsandboxed"] = json!({ "type": "boolean", "description": "Retry a failed sandboxed command on the host. Needs retry_of and reason, and the user's approval." });
            properties["reason"] = json!({ "type": "string", "description": "Why the command needs to run outside the sandbox." });
            properties["retry_of"] = json!({ "type": "string", "description": "Function call id of the failed sandboxed call." });
        }
        Some(json!({ "type": "object", "properties": properties, "required": ["command"] }))
    }

    fn annotations(&self) -> Option<&Value> {
        Some(&self.annotations)
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(config) = ctx.run_config() else {
            return Err(refuse("harness.bash.ungoverned", "bash runs only inside a harness run"));
        };
        if !config.tool_policy.as_deref().is_some_and(has_harness_layer) {
            return Err(refuse(
                "harness.bash.ungoverned",
                "bash refuses to run without the harness policy in the run config",
            ));
        }
        config.check_governance()?;
        if self.fail_if_unavailable
            && let IsolationStatus::Degraded { requested, reason } = &self.status
        {
            return Err(sandbox_unavailable(requested, reason));
        }
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| AdkError::tool("bash needs a command"))?
            .to_string();
        let (app, user, session) =
            (ctx.app_name().to_string(), ctx.user_id().to_string(), ctx.session_id().to_string());
        let call_id = ctx.function_call_id().to_string();
        let audit = HarnessAudit::for_tool(ctx.as_ref());

        let mut retry_of = None;
        let selected = if args.get("unsandboxed").and_then(Value::as_bool) == Some(true) {
            if !self.sandbox_active() {
                return Err(refuse(
                    "harness.bash.retry_refused",
                    "the shell already runs on the host; there is no sandboxed call to retry",
                ));
            }
            if !self.allow_unsandboxed_retry {
                return Err(refuse(
                    "harness.bash.retry_refused",
                    "unsandboxed retries are off for this harness; commands run only in the \
                     sandbox",
                ));
            }
            let failed_id = args.get("retry_of").and_then(Value::as_str).ok_or_else(|| {
                refuse("harness.bash.retry_refused", "an unsandboxed retry needs retry_of")
            })?;
            let failed = self.ledger.get(&app, &user, &session, failed_id).await?;
            let Some(failed) = failed.filter(|entry| {
                entry.sandboxed
                    && entry.failed()
                    && entry.command == command
                    && entry.retried_by.is_none()
            }) else {
                return Err(refuse(
                    "harness.bash.retry_refused",
                    format!(
                        "'{failed_id}' is not a failed sandboxed call with this exact command that \
                         has not been retried"
                    ),
                ));
            };
            if !self.ledger.mark_retried(&app, &user, &session, failed_id, &call_id).await? {
                return Err(refuse(
                    "harness.bash.retry_refused",
                    format!("the sandboxed call '{failed_id}' was already retried or is unknown"),
                ));
            }
            if let Some(audit) = &audit {
                audit
                    .record(
                        "harness.bash.retry_unsandboxed",
                        json!({
                            "functionCallId": call_id,
                            "retryOf": failed_id,
                            "failedEnforcer": failed.enforcer,
                            "failureDigest": failed.digest(),
                            "decision": "consumed",
                            "source": "tool",
                        }),
                    )
                    .await?;
            }
            retry_of = Some(failed_id.to_string());
            Selected {
                bash: &self.host,
                sandboxed: false,
                read_only: false,
                env_keys: self.host_env_keys(),
            }
        } else if self.sandbox_active() {
            match self.sandboxed(ctx.as_ref()).await {
                Ok(selected) => selected,
                Err(reason) => {
                    let requested = self.status.enforcer().to_string();
                    if let Some(audit) = &audit {
                        audit
                            .record(
                                "harness.sandbox.unavailable",
                                json!({ "functionCallId": call_id, "requested": requested, "error": reason }),
                            )
                            .await?;
                    }
                    // A sandboxed declaration never falls back to the host (I24).
                    return Err(sandbox_unavailable(&requested, &reason));
                }
            }
        } else {
            Selected {
                bash: &self.host,
                sandboxed: false,
                read_only: false,
                env_keys: self.host_env_keys(),
            }
        };
        let enforcer = if selected.sandboxed { self.status.enforcer() } else { "host" };
        // No host file tool runs while a sandboxed command does (I26).
        let _exclusive = match (&self.file_gate, selected.sandboxed) {
            (Some(gate), true) => Some(gate.exclusive(config).await?),
            _ => None,
        };

        if let Some(audit) = &audit {
            let draft = AuditRecordDraft::new(
                AuditRecordKind::Other(HARNESS_AUDIT_KIND.to_string()),
                "harness.bash.begin",
            )
            .args_digest(audit.trail().digest(&args))
            .payload_ref(audit.trail().digest(&json!({ "command": command })));
            audit
                .record_draft(
                    draft,
                    json!({
                        "functionCallId": call_id,
                        "enforcer": enforcer,
                        "profileDigest": if selected.sandboxed { self.annotations["profileDigest"].clone() } else { Value::Null },
                        "network": if selected.sandboxed { self.annotations["network"].clone() } else { Value::Bool(true) },
                        "reads": if selected.sandboxed { self.annotations["reads"].clone() } else { json!("unconfined") },
                        "msys2Isolated": if selected.sandboxed { self.annotations["msys2Isolated"].clone() } else { Value::Bool(false) },
                        "workspaceAccess": if !selected.sandboxed { Value::Null } else if selected.read_only { json!("read_only") } else { json!("read_write") },
                        "envKeys": selected.env_keys,
                        "retryOf": retry_of,
                        "goalCheck": false,
                    }),
                )
                .await?;
        }

        let mut inner_args = json!({ "command": command });
        if let Some(timeout) = args.get("timeout_secs") {
            inner_args["timeout_secs"] = timeout.clone();
        }
        let started = Instant::now();
        let mut run = Box::pin(selected.bash.execute(ctx.clone(), inner_args));
        let mut poll = tokio::time::interval(GOVERNANCE_POLL_INTERVAL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let outcome = loop {
            tokio::select! {
                result = &mut run => break Ok(result),
                _ = poll.tick() => {
                    if let Err(frozen) = config.check_governance() {
                        break Err(frozen);
                    }
                }
            }
        };
        // Dropping the devtools future kills the command's whole process group.
        drop(run);
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let (exit_code, timed_out, killed_by_freeze, truncated) = match &outcome {
            Ok(Ok(value)) => (
                value
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .and_then(|code| i32::try_from(code).ok()),
                false,
                false,
                value.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            ),
            Ok(Err(error)) => (None, error.message.contains("timed out after"), false, false),
            Err(_) => (None, false, true, false),
        };
        let mut outcome = outcome;
        let mut suspected = false;
        if selected.sandboxed
            && let Ok(Ok(value)) = &mut outcome
            && exit_code != Some(0)
        {
            let output = format!(
                "{}\n{}",
                value.get("stderr").and_then(Value::as_str).unwrap_or_default(),
                value.get("stdout").and_then(Value::as_str).unwrap_or_default()
            );
            suspected = denial_suspected(&output);
            let refused = if output.contains(MSYS2_NAMESPACE_HELD) {
                "an MSYS2 program that a program outside MSYS2 starts itself, such as a hook's \
                 `sh` that git starts, cannot start in the sandbox"
            } else {
                "writes outside the workspace or to protected paths, and network access, are \
                 denied"
            };
            let hint = match (suspected, self.allow_unsandboxed_retry) {
                (true, true) => format!(
                    "The sandbox ({enforcer}) may have refused this: {refused}. If the command \
                     needs that, retry it once with unsandboxed: true, retry_of: \"{call_id}\" \
                     and a reason; the user must approve the retry."
                ),
                (true, false) => format!(
                    "The sandbox ({enforcer}) may have refused this: {refused}. Unsandboxed \
                     retries are turned off; ask the user to run it."
                ),
                (false, _) => "The command failed inside the sandbox; nothing in its output \
                               suggests the sandbox refused it."
                    .to_string(),
            };
            value["sandbox"] =
                json!({ "enforcer": enforcer, "denialSuspected": suspected, "hint": hint });
        }

        let mut entry = ShellLedgerEntry::new(&call_id, &command, selected.sandboxed, exit_code);
        if selected.sandboxed {
            entry.enforcer = Some(enforcer.to_string());
            entry.denial_suspected = suspected;
        }
        if let Err(error) = self.ledger.record(&app, &user, &session, entry).await {
            tracing::warn!(error = %error, "could not record the bash call in the shell ledger");
        }
        if let Some(audit) = &audit {
            let result = match &outcome {
                Ok(Ok(_)) => {
                    if exit_code == Some(0) {
                        AuditRecordOutcome::Succeeded
                    } else {
                        AuditRecordOutcome::Failed
                    }
                }
                Ok(Err(_)) if !timed_out => AuditRecordOutcome::Failed,
                Ok(Err(_)) | Err(_) => AuditRecordOutcome::OutcomeUnknown,
            };
            let mut draft = AuditRecordDraft::new(
                AuditRecordKind::Other(HARNESS_AUDIT_KIND.to_string()),
                "harness.bash.end",
            )
            .outcome(result);
            if let Ok(Ok(value)) = &outcome {
                draft = draft.payload_ref(audit.trail().digest(value));
            }
            audit
                .record_draft(
                    draft,
                    json!({
                        "functionCallId": call_id,
                        "enforcer": enforcer,
                        "exitCode": exit_code,
                        "durationMs": duration_ms,
                        "timedOut": timed_out,
                        "killedByFreeze": killed_by_freeze,
                        "truncated": truncated,
                        "denialSuspected": selected.sandboxed.then_some(suspected),
                        "retryOf": retry_of,
                    }),
                )
                .await?;
        }
        match outcome {
            Ok(result) => result,
            Err(frozen) => Err(frozen),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declaration carries whether reads are confined, so a standing rule bound to a
    /// confining sandbox's declaration never matches the Windows restricted-token one, which
    /// reads what the user may read (spec 3.1 §9.2 I9, spec 3.2 §10 W5).
    #[test]
    fn the_declaration_says_whether_reads_are_confined() {
        let bash = |status: IsolationStatus| {
            let ws = Workspace::new(".").allow_bash(true);
            HarnessBashTool::new(ws, status, false, Arc::new(crate::InMemoryShellLedger::new()))
        };
        let confining = bash(IsolationStatus::OsSandbox { enforcer: "seatbelt".into() });
        let windows = bash(IsolationStatus::OsSandbox { enforcer: "restricted-token".into() });
        let host = bash(IsolationStatus::Host);
        assert_eq!(confining.annotations().unwrap()["reads"], "confined");
        assert_eq!(windows.annotations().unwrap()["reads"], "unconfined");
        assert_eq!(host.annotations().unwrap()["reads"], "unconfined");
        assert_eq!(confining.annotations().unwrap()["msys2Isolated"], true);
        assert_eq!(windows.annotations().unwrap()["msys2Isolated"], false);
        assert_eq!(host.annotations().unwrap()["msys2Isolated"], false);
        let windows_only_reads = {
            let mut annotations = windows.annotations().unwrap().clone();
            annotations["enforcer"] = "seatbelt".into();
            annotations
        };
        assert_ne!(&windows_only_reads, confining.annotations().unwrap(), "reads alone differ");
        assert_ne!(
            adk_core::ToolOrigin::of(&confining).declaration_digest,
            adk_core::ToolOrigin::of(&windows).declaration_digest
        );
    }

    /// Review of #1045, finding 5: with the retry off, `bash` declares no retry arguments,
    /// whatever the host's enforcer.
    #[test]
    fn without_the_retry_bash_declares_no_retry_arguments() {
        let declared = |retry: bool| {
            let ws = Workspace::new(".").allow_bash(true);
            let status = IsolationStatus::OsSandbox { enforcer: "seatbelt".into() };
            HarnessBashTool::new(ws, status, false, Arc::new(crate::InMemoryShellLedger::new()))
                .allow_unsandboxed_retry(retry)
                .parameters_schema()
                .unwrap()["properties"]
                .clone()
        };
        assert!(declared(true).get("unsandboxed").is_some());
        let off = declared(false);
        for name in ["unsandboxed", "retry_of", "reason"] {
            assert!(off.get(name).is_none(), "{name}: {off}");
        }
    }

    #[test]
    fn denials_are_recognized_and_ordinary_failures_are_not() {
        for output in [
            "touch: /etc/x: Operation not permitted",
            "sh: .git/config: Read-only file system",
            "curl: (6) Could not resolve host: example.com",
            "Sandbox: sh(123) deny(1) file-write-create /Users/x",
            "connect: Network is unreachable",
            "0 [main] sh (5448) C:\\Program Files\\Git\\usr\\bin\\sh.exe: *** fatal error - \
             NtCreateDirectoryObject(\\BaseNamedObjects\\msys-2.0S5-1888ae32e00d56aa): 0xC0000022",
        ] {
            assert!(denial_suspected(output), "{output}");
        }
        for output in ["error[E0425]: cannot find value `x`", "test result: FAILED. 1 failed", ""] {
            assert!(!denial_suspected(output), "{output}");
        }
    }
}
