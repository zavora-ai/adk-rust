//! `bash` as the harness declares and governs it (spec 3.1 §7.9).

use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use adk_devtools::{BashTool, Workspace};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::audit::{HARNESS_AUDIT_KIND, HarnessAudit};
use crate::policy::has_harness_layer;
use crate::shell::{IsolationStatus, ShellLedger, ShellLedgerEntry, sandbox_unavailable};

/// How often a running command checks the run's kill switch (spec 3.1 §7.9).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::GOVERNANCE_POLL_INTERVAL, std::time::Duration::from_millis(250));
/// ```
pub const GOVERNANCE_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The harness `bash` tool (spec 3.1 §7.9).
///
/// Wraps the devtools `BashTool`, keeping its timeout ceiling, output cap, streaming and
/// process-group kill, and adds:
///
/// - its own declaration `{ command, timeout_secs? }`, plus `unsandboxed`, `reason` and
///   `retry_of` when a sandbox is active;
/// - `annotations()` of `{enforcer, profileDigest, network}`, so the declaration digest —
///   and every standing rule or held call bound to it — differs per isolation (I9);
/// - a refusal to run unless the harness policy is in the run's config (I17);
/// - a check of the run's kill switch every 250 ms, killing the command on a freeze (I20);
/// - `harness.bash.begin` (write-ahead) and `harness.bash.end` audit records;
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
    inner: BashTool,
    status: IsolationStatus,
    fail_if_unavailable: bool,
    ledger: Arc<dyn ShellLedger>,
    annotations: Value,
    description: String,
}

impl HarnessBashTool {
    /// A `bash` tool over `workspace`, running under `status`.
    pub fn new(
        workspace: Workspace,
        status: IsolationStatus,
        fail_if_unavailable: bool,
        ledger: Arc<dyn ShellLedger>,
    ) -> Self {
        let host = status.behaves_as_host();
        let annotations = json!({
            "enforcer": status.enforcer(),
            "profileDigest": Value::Null,
            "network": host,
        });
        let description = if host {
            "Run a shell command in the workspace root and return stdout, stderr and the exit \
             code. The command runs on the host with the user's full permissions and network \
             access, so each command may need the user's approval. Has a timeout."
                .to_string()
        } else {
            format!(
                "Run a shell command in the workspace root inside an OS sandbox ({}) with no \
                 network. If it fails because the sandbox denied something, you may retry it \
                 once with unsandboxed: true, retry_of: <the failed call id> and a reason; \
                 the user must approve that retry.",
                status.enforcer()
            )
        };
        Self {
            inner: BashTool::new(workspace.clone()),
            workspace,
            status,
            fail_if_unavailable,
            ledger,
            annotations,
            description,
        }
    }

    fn sandbox_active(&self) -> bool {
        !self.status.behaves_as_host()
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
        if self.sandbox_active() {
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
        let retry_of = if args.get("unsandboxed").and_then(Value::as_bool) == Some(true) {
            if !self.sandbox_active() {
                return Err(refuse(
                    "harness.bash.retry_refused",
                    "the shell already runs on the host; there is no sandboxed call to retry",
                ));
            }
            let retry_of = args.get("retry_of").and_then(Value::as_str).ok_or_else(|| {
                refuse("harness.bash.retry_refused", "an unsandboxed retry needs retry_of")
            })?;
            if !self.ledger.mark_retried(&app, &user, &session, retry_of, &call_id).await? {
                return Err(refuse(
                    "harness.bash.retry_refused",
                    format!("the sandboxed call '{retry_of}' was already retried or is unknown"),
                ));
            }
            Some(retry_of.to_string())
        } else {
            None
        };

        let audit = HarnessAudit::for_tool(ctx.as_ref());
        let env_keys: Vec<String> = if self.workspace.inherits_env() {
            vec!["*".to_string()]
        } else {
            self.workspace.bash_env().into_iter().map(|(key, _)| key).collect()
        };
        if let Some(audit) = &audit {
            let draft = AuditRecordDraft::new(
                AuditRecordKind::Other(HARNESS_AUDIT_KIND.to_string()),
                "harness.bash.begin",
            )
            .args_digest(audit.trail().digest(&args));
            audit
                .record_draft(
                    draft,
                    json!({
                        "functionCallId": call_id,
                        "enforcer": self.status.enforcer(),
                        "profileDigest": self.annotations["profileDigest"],
                        "network": self.annotations["network"],
                        "envKeys": env_keys,
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
        let mut run = Box::pin(self.inner.execute(ctx.clone(), inner_args));
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
        let mut entry = ShellLedgerEntry::new(&call_id, &command, self.sandbox_active(), exit_code);
        if self.sandbox_active() {
            entry.enforcer = Some(self.status.enforcer().to_string());
        }
        if let Err(error) = self.ledger.record(&app, &user, &session, entry).await {
            tracing::warn!(error = %error, "could not record the bash call in the shell ledger");
        }
        if let Some(audit) = &audit {
            audit
                .record(
                    "harness.bash.end",
                    json!({
                        "functionCallId": call_id,
                        "exitCode": exit_code,
                        "durationMs": duration_ms,
                        "timedOut": timed_out,
                        "killedByFreeze": killed_by_freeze,
                        "truncated": truncated,
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
