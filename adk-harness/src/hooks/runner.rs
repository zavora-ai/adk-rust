//! `HookRunner`: runs hooks through a `CommandLauncher` and records every run
//! (spec 3.1 §7.22, §9.2 I53–I54, §9.3 `harness.hook.ran`).

use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use adk_core::{
    ActingContext, Artifacts, CallbackContext, Content, Event, EventActions, EventStream,
    InvocationContext, MemoryEntry, ReadonlyContext, Result, RunConfig, ToolConfirmationRequest,
    ToolContext, ToolPolicyRequest,
};
use adk_devtools::{CommandLauncher, HostLauncher, Workspace};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use super::{
    HOOK_EVENT_ENV, HOOK_NOTICE_KEY, HookEvent, HookIsolation, HookNote, HookOutcome, HookSet,
    HookSource, HookSpec, PostToolVerdict, PreToolVerdict,
};
use crate::audit::{HARNESS_AUDIT_KIND, HarnessAudit};
use crate::mode::HarnessMode;
use crate::shell::IsolationStatus;

/// Hook input that is serialized for stdin is cut to this many bytes when it carries a tool
/// result or a diff.
const INPUT_PAYLOAD_MAX_BYTES: usize = 64 * 1024;
/// Entries kept per bookkeeping map before it is cleared.
const MAP_LIMIT: usize = 4096;

/// Whether the workspace is trusted under the content-keyed trust of spec 3.1 §7.7.
pub(crate) type TrustCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Runs the hooks of a [`HookSet`] (spec 3.1 §7.22).
///
/// | Rule | Mechanism |
/// |---|---|
/// | Same launcher as `bash` | `Inherit` hooks run through the launcher given to [`with_launcher`](Self::with_launcher) — the sandbox when the shell is sandboxed — and through the read-only one in Plan and ReadOnly; `Host` hooks through `HostLauncher` |
/// | Same bounds as `bash` | the hook's timeout is capped by the workspace's bash timeout; stdout and stderr by its output cap; the process runs in a new session with no terminal and its group is killed when it exits or times out |
/// | Trust | a `Workspace` hook runs only while the trust check passes; an untrusted one is skipped and never recorded as run |
/// | Host isolation | needs workspace trust for a workspace hook, and in Auto an operator hook |
/// | Failure | a hook that exits non-zero, crashes, times out or cannot be launched applies its [`HookFailure`](super::HookFailure): by default a `PreTool` hook's failure denies the call (recorded `outcome: deny`) and every other event's is `Continue` (recorded `outcome: error`); `on_failure = "continue"` opts a `PreTool` hook out, `"block"` makes a `PostTool` or `PostEdit` hook's failure fail the result |
/// | Budget | `max_per_event` hooks run per event; once the hooks of a turn have used `max_time_per_turn`, a `PreTool` dispatch denies and a `PostTool` one fails with the reason |
/// | Audit | `harness.hook.ran` per run, written before the outcome takes effect; an outcome whose record cannot be written is `error` |
///
/// The runner holds no mode store, rule store or approval store: nothing a hook prints can
/// reach them (spec 3.1 §9.2 I53).
///
/// # Example
///
/// ```rust
/// use adk_harness::hooks::{HookEvent, HookRunner, HookSet, HookSpec};
/// use adk_harness::IsolationStatus;
///
/// let set = HookSet::new().with(HookSpec::new(HookEvent::PreTool, "./check.sh").matching("bash"));
/// let workspace = adk_devtools::Workspace::new(".").allow_bash(true);
/// let runner = HookRunner::new(set, workspace, IsolationStatus::Host);
/// assert_eq!(runner.set().hooks.len(), 1);
/// ```
pub struct HookRunner {
    set: HookSet,
    workspace: Workspace,
    launcher: Arc<dyn CommandLauncher>,
    read_only_launcher: Option<Arc<dyn CommandLauncher>>,
    host: Arc<dyn CommandLauncher>,
    isolation: IsolationStatus,
    trusted: TrustCheck,
    spent: Mutex<HashMap<String, Duration>>,
    session_notes: Mutex<HashMap<String, Vec<HookNote>>>,
}

impl std::fmt::Debug for HookRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookRunner")
            .field("hooks", &self.set.hooks.len())
            .field("isolation", &self.isolation)
            .finish_non_exhaustive()
    }
}

/// What one hook process did.
struct HookRun {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    duration: Duration,
    timed_out: bool,
    /// Why the hook did not run or did not finish normally.
    error: Option<String>,
}

/// The outcomes of one event's hooks.
struct Dispatch {
    outcomes: Vec<(HookSpec, HookOutcome)>,
    /// The reason the turn's hook budget ended the dispatch.
    over_budget: Option<String>,
}

/// A tool call as the `PreTool` and `PostTool` hooks see it (spec 3.1 §7.22).
#[derive(Clone, Debug)]
pub struct HookCall<'a> {
    /// The tool name.
    pub tool: &'a str,
    /// The final arguments.
    pub args: &'a Value,
    /// The harness class of the call, as a label such as `shell_host` or `edit`.
    pub class: &'a str,
    /// The acting agent.
    pub acting: Option<&'a ActingContext>,
    /// The function call id, when known.
    pub function_call_id: Option<&'a str>,
}

/// The result a `PostTool` hook sees.
#[derive(Clone, Debug)]
pub struct HookResult<'a> {
    /// The tool's result, or `{"error": …}` when it failed.
    pub result: &'a Value,
    /// Whether the tool returned an error.
    pub is_error: bool,
    /// The command's exit code, for `bash`.
    pub exit_code: Option<i64>,
    /// How long the tool ran.
    pub duration: Duration,
}

/// An edit as the `PostEdit` hooks see it.
#[derive(Clone, Debug)]
pub struct HookEdit<'a> {
    /// The tool that edited.
    pub tool: &'a str,
    /// The workspace-relative path.
    pub path: &'a str,
    /// Hex SHA-256 before the edit; `None` for a new file.
    pub before_sha256: Option<&'a str>,
    /// Hex SHA-256 after the edit.
    pub after_sha256: &'a str,
    /// A capped line diff.
    pub diff: &'a str,
}

/// What a harness turn did, as the `OnStop` hooks see it (spec 3.1 §7.22).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSummary {
    /// Tool calls by tool name.
    pub tool_calls: BTreeMap<String, u32>,
    /// Tools whose calls were held for a decision.
    pub held_calls: Vec<String>,
    /// Model responses in the turn.
    pub model_calls: u32,
    /// Prompt tokens the turn used, when the provider reported them.
    pub prompt_tokens: u64,
    /// Completion tokens the turn used, when the provider reported them.
    pub completion_tokens: u64,
}

impl HookRunner {
    /// A runner for `set` over `workspace`, with `isolation` describing where the shell runs.
    /// Both launchers default to the host; the workspace counts as untrusted until
    /// [`with_workspace_trust`](Self::with_workspace_trust) says otherwise.
    pub fn new(set: HookSet, workspace: Workspace, isolation: IsolationStatus) -> Self {
        Self {
            set,
            workspace,
            launcher: Arc::new(HostLauncher),
            read_only_launcher: None,
            host: Arc::new(HostLauncher),
            isolation,
            trusted: Arc::new(|| false),
            spent: Mutex::new(HashMap::new()),
            session_notes: Mutex::new(HashMap::new()),
        }
    }

    /// Runs `Inherit` hooks through `launcher`: the shell's sandbox launcher, or the host.
    #[must_use]
    pub fn with_launcher(mut self, launcher: Arc<dyn CommandLauncher>) -> Self {
        self.launcher = launcher;
        self
    }

    /// Runs `Inherit` hooks through `launcher` in Plan and ReadOnly, where the shell's
    /// workspace is read-only.
    #[must_use]
    pub fn with_read_only_launcher(mut self, launcher: Arc<dyn CommandLauncher>) -> Self {
        self.read_only_launcher = Some(launcher);
        self
    }

    /// Decides whether workspace hooks may run: the content-keyed workspace trust.
    #[must_use]
    pub fn with_workspace_trust(
        mut self,
        trusted: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        self.trusted = Arc::new(trusted);
        self
    }

    /// The hooks.
    pub fn set(&self) -> &HookSet {
        &self.set
    }

    /// The workspace hooks run in.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Whether the hooks of `invocation_id` have used the turn's time budget.
    pub fn over_budget(&self, invocation_id: &str) -> bool {
        self.spent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(invocation_id)
            .is_some_and(|spent| *spent >= self.set.max_time_per_turn)
    }

    fn scope_input(mode: HarnessMode, scope: &dyn ToolContext, isolation: &str) -> Value {
        json!({
            "mode": mode.as_str(),
            "isolation": isolation,
            "session": {
                "app": scope.app_name(),
                "user": scope.user_id(),
                "session": scope.session_id(),
                "invocation": scope.invocation_id(),
            },
            "actingAgent": scope.acting_context(),
        })
    }

    /// Runs the `PreTool` hooks of `call` (spec 3.1 §7.4 step 4a). A `Deny` can only tighten
    /// the decision it is applied to.
    pub(crate) async fn pre_tool(
        &self,
        call: &HookCall<'_>,
        mode: HarnessMode,
        scope: &dyn ToolContext,
        audit: Option<&HarnessAudit>,
    ) -> PreToolVerdict {
        let mut input = Self::scope_input(mode, scope, self.isolation.enforcer());
        input["tool"] = json!(call.tool);
        input["args"] = call.args.clone();
        input["class"] = json!(call.class);
        input["functionCallId"] = json!(call.function_call_id);
        if let Some(acting) = call.acting {
            input["actingAgent"] = json!(acting);
        }
        let dispatch =
            self.dispatch(HookEvent::PreTool, Some(call.tool), mode, input, scope, audit).await;
        if let Some(reason) = dispatch.over_budget {
            return PreToolVerdict::Deny { reason };
        }
        dispatch
            .outcomes
            .into_iter()
            .map(|(_, outcome)| PreToolVerdict::from_outcome(outcome))
            .find(|verdict| matches!(verdict, PreToolVerdict::Deny { .. }))
            .unwrap_or(PreToolVerdict::Continue)
    }

    /// Runs the `PostTool` hooks of `call` on `result`.
    pub(crate) async fn post_tool(
        &self,
        call: &HookCall<'_>,
        result: &HookResult<'_>,
        mode: HarnessMode,
        scope: &dyn ToolContext,
        audit: Option<&HarnessAudit>,
    ) -> PostToolVerdict {
        let mut input = Self::scope_input(mode, scope, self.isolation.enforcer());
        input["tool"] = json!(call.tool);
        input["args"] = call.args.clone();
        input["class"] = json!(call.class);
        input["functionCallId"] = json!(call.function_call_id);
        if let Some(acting) = call.acting {
            input["actingAgent"] = json!(acting);
        }
        input["result"] = capped(result.result);
        input["isError"] = json!(result.is_error);
        input["exitCode"] = json!(result.exit_code);
        input["durationMs"] = json!(u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX));
        let dispatch =
            self.dispatch(HookEvent::PostTool, Some(call.tool), mode, input, scope, audit).await;
        post_verdict(dispatch)
    }

    /// Runs the `PostEdit` hooks of `edit`. They report; nothing here reverts the edit.
    pub(crate) async fn post_edit(
        &self,
        edit: &HookEdit<'_>,
        mode: HarnessMode,
        scope: &dyn ToolContext,
        audit: Option<&HarnessAudit>,
    ) -> PostToolVerdict {
        let mut input = Self::scope_input(mode, scope, self.isolation.enforcer());
        input["tool"] = json!(edit.tool);
        input["path"] = json!(edit.path);
        input["beforeSha256"] = json!(edit.before_sha256);
        input["afterSha256"] = json!(edit.after_sha256);
        input["diff"] = json!(cut(edit.diff, INPUT_PAYLOAD_MAX_BYTES));
        let dispatch =
            self.dispatch(HookEvent::PostEdit, Some(edit.tool), mode, input, scope, audit).await;
        post_verdict(dispatch)
    }

    /// Runs the `OnHold` hooks for `held`: notifiers whose notes go to the user.
    pub(crate) async fn on_hold(
        &self,
        held: &ToolConfirmationRequest,
        mode: HarnessMode,
        scope: &dyn ToolContext,
        audit: Option<&HarnessAudit>,
    ) -> Vec<HookNote> {
        let mut input = Self::scope_input(mode, scope, self.isolation.enforcer());
        input["heldCall"] = json!(held);
        let dispatch = self
            .dispatch(HookEvent::OnHold, Some(&held.tool_name), mode, input, scope, audit)
            .await;
        notes(dispatch)
    }

    /// Runs the `OnStop` hooks with the turn's summary: notes go to the user, not the model.
    pub(crate) async fn on_stop(
        &self,
        summary: &TurnSummary,
        mode: HarnessMode,
        scope: &dyn ToolContext,
        audit: Option<&HarnessAudit>,
    ) -> Vec<HookNote> {
        let mut input = Self::scope_input(mode, scope, self.isolation.enforcer());
        input["summary"] = json!(summary);
        let dispatch = self.dispatch(HookEvent::OnStop, None, mode, input, scope, audit).await;
        notes(dispatch)
    }

    /// Runs the `OnSessionStart` hooks once for the session `scope` names and keeps their
    /// notes for [`with_session_notes`](Self::with_session_notes); later calls return the
    /// kept notes without running anything.
    pub(crate) async fn on_session_start(
        &self,
        mode: HarnessMode,
        scope: &dyn ToolContext,
        audit: Option<&HarnessAudit>,
    ) -> Vec<HookNote> {
        let key = session_key(scope.app_name(), scope.user_id(), scope.session_id());
        if let Some(notes) =
            self.session_notes.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned()
        {
            return notes;
        }
        if self.set.for_event(HookEvent::OnSessionStart).next().is_none() {
            return Vec::new();
        }
        let mut input = Self::scope_input(mode, scope, self.isolation.enforcer());
        input["workspace"] = json!(self.workspace.root().display().to_string());
        let dispatch =
            self.dispatch(HookEvent::OnSessionStart, None, mode, input, scope, audit).await;
        let notes = notes(dispatch);
        let mut cache = self.session_notes.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= MAP_LIMIT {
            cache.clear();
        }
        cache.insert(key, notes.clone());
        notes
    }

    /// `instruction` with the session's `OnSessionStart` notes inside its `<environment>`
    /// block, when the session has any (spec 3.1 §7.22).
    pub fn with_session_notes(
        &self,
        instruction: String,
        app: &str,
        user: &str,
        session: &str,
    ) -> String {
        let key = session_key(app, user, session);
        let notes = self.session_notes.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
        let Some(notes) = notes.filter(|notes| !notes.is_empty()) else {
            return instruction;
        };
        let rendered: String = notes.iter().map(|note| format!("{}\n", note.render())).collect();
        match instruction.find("</environment>") {
            Some(at) => format!("{}{rendered}{}", &instruction[..at], &instruction[at..]),
            None => format!("{instruction}\n\n<environment>\n{rendered}</environment>"),
        }
    }

    /// Wraps a harness turn's events: runs the `OnHold` hooks when a call is held and the
    /// `OnStop` hooks when the turn ends, appending each note as an event carrying
    /// [`HOOK_NOTICE_KEY`] in its provider metadata and no content, so the user sees it and
    /// the model never does.
    pub(crate) fn observe(
        self: &Arc<Self>,
        events: EventStream,
        ctx: Arc<dyn InvocationContext>,
        mode: HarnessMode,
        author: String,
    ) -> EventStream {
        use futures::StreamExt as _;
        let runner = self.clone();
        let has_hold = self.set.for_event(HookEvent::OnHold).next().is_some();
        let has_stop = self.set.for_event(HookEvent::OnStop).next().is_some();
        if !has_hold && !has_stop {
            return events;
        }
        Box::pin(async_stream::stream! {
            let mut events = events;
            let mut summary = TurnSummary::default();
            let scope = HookScope::from_invocation(ctx.as_ref());
            let audit = ctx.run_config().audit.clone().map(|trail| {
                HarnessAudit::new(trail, AuditActor::for_context(ctx.as_ref()), ctx.session_id())
            });
            while let Some(event) = events.next().await {
                let held = match &event {
                    Ok(event) => {
                        observe_event(&mut summary, event);
                        event.actions.tool_confirmation.clone()
                    }
                    Err(_) => None,
                };
                yield event;
                if let (true, Some(held)) = (has_hold, held) {
                    summary.held_calls.push(held.tool_name.clone());
                    for note in runner.on_hold(&held, mode, &scope, audit.as_ref()).await {
                        yield Ok(notice_event(&ctx, &author, &note));
                    }
                }
            }
            if has_stop {
                for note in runner.on_stop(&summary, mode, &scope, audit.as_ref()).await {
                    yield Ok(notice_event(&ctx, &author, &note));
                }
            }
        })
    }

    /// Runs the hooks of `event` that match `tool`, serially in source order.
    async fn dispatch(
        &self,
        event: HookEvent,
        tool: Option<&str>,
        mode: HarnessMode,
        input: Value,
        scope: &dyn ToolContext,
        audit: Option<&HarnessAudit>,
    ) -> Dispatch {
        let mut dispatch = Dispatch { outcomes: Vec::new(), over_budget: None };
        let hooks: Vec<&HookSpec> = self
            .set
            .for_event(event)
            .filter(|hook| tool.is_none_or(|tool| hook.matches(tool)))
            .take(usize::from(self.set.max_per_event))
            .collect();
        if hooks.is_empty() {
            return dispatch;
        }
        let trusted = (self.trusted)();
        for hook in hooks {
            if hook.source == HookSource::Workspace && !trusted {
                tracing::info!(
                    hook.event = %event,
                    hook.command_digest = %hook.command_digest(),
                    "workspace hook skipped: the workspace is not trusted"
                );
                continue;
            }
            if self.over_budget(scope.invocation_id()) {
                dispatch.over_budget = Some(format!(
                    "the hooks of this turn used their {} s budget; the turn ends here",
                    self.set.max_time_per_turn.as_secs()
                ));
                break;
            }
            let launcher = match hook.isolation {
                HookIsolation::Host
                    if mode == HarnessMode::Auto && hook.source != HookSource::Operator =>
                {
                    Err("a host-isolated hook runs in auto only from the operator's configuration"
                        .to_string())
                }
                HookIsolation::Host => Ok(self.host.clone()),
                HookIsolation::Inherit => Ok(match (&self.read_only_launcher, mode) {
                    (Some(read_only), HarnessMode::ReadOnly | HarnessMode::Plan) => {
                        read_only.clone()
                    }
                    _ => self.launcher.clone(),
                }),
            };
            let (run, isolation) = match launcher {
                Ok(launcher) => {
                    let isolation = launcher.isolation().to_string();
                    (self.run_hook(hook, launcher.as_ref(), event, &input, scope).await, isolation)
                }
                Err(reason) => (
                    HookRun {
                        exit_code: None,
                        stdout: Vec::new(),
                        duration: Duration::ZERO,
                        timed_out: false,
                        error: Some(reason),
                    },
                    "refused".to_string(),
                ),
            };
            self.charge(scope.invocation_id(), run.duration);

            let failed = run.error.is_some() || run.timed_out || run.exit_code != Some(0);
            let parsed = if failed { None } else { Some(HookOutcome::parse(&run.stdout)) };
            let outcome = match &parsed {
                Some(parsed) => event.narrow(parsed.outcome.clone()),
                None => blocked(
                    hook,
                    &format!(
                        "the {} hook failed ({}), and a failed {} hook blocks",
                        hook.event,
                        run.error.clone().unwrap_or_else(|| describe_failure(&run)),
                        hook.event
                    ),
                ),
            };
            let recorded_outcome = match (&parsed, &outcome) {
                (Some(parsed), _) if parsed.ignored_grant => "ignored_grant",
                (None, HookOutcome::Continue) => "error",
                (_, outcome) => outcome.as_str(),
            };
            if parsed.as_ref().is_some_and(|parsed| parsed.ignored_grant) {
                tracing::warn!(
                    hook.event = %event,
                    hook.source = hook.source.as_str(),
                    hook.command_digest = %hook.command_digest(),
                    "hook tried to allow a call; a hook can only deny, fail or annotate (spec 3.1 §9.2 I53)"
                );
            }
            if failed {
                tracing::warn!(
                    hook.event = %event,
                    hook.command_digest = %hook.command_digest(),
                    hook.exit_code = ?run.exit_code,
                    hook.timed_out = run.timed_out,
                    error = run.error.as_deref().unwrap_or(""),
                    hook.on_failure = hook.failure().as_str(),
                    "hook did not complete; its on_failure applies"
                );
            }

            // Written before the outcome takes effect: an outcome the trail cannot record
            // does not apply.
            let outcome = match audit {
                Some(audit) => {
                    let draft = AuditRecordDraft::new(
                        AuditRecordKind::Other(HARNESS_AUDIT_KIND.to_string()),
                        "harness.hook.ran",
                    )
                    .outcome(if failed {
                        if run.timed_out {
                            AuditRecordOutcome::OutcomeUnknown
                        } else {
                            AuditRecordOutcome::Failed
                        }
                    } else {
                        AuditRecordOutcome::Succeeded
                    })
                    .payload_ref(audit.trail().digest(&json!({
                        "stdout": String::from_utf8_lossy(&run.stdout),
                    })));
                    let recorded = audit
                        .record_draft(
                            draft,
                            json!({
                                "event": event.as_str(),
                                "matcher": hook.matcher,
                                "tool": tool,
                                "functionCallId": input.get("functionCallId").cloned().unwrap_or(Value::Null),
                                "commandDigest": hook.command_digest(),
                                "source": hook.source.as_str(),
                                "isolation": isolation,
                                "exitCode": run.exit_code,
                                "durationMs": u64::try_from(run.duration.as_millis()).unwrap_or(u64::MAX),
                                "timedOut": run.timed_out,
                                "outcome": recorded_outcome,
                                "error": run.error,
                            }),
                        )
                        .await;
                    match recorded {
                        Ok(()) => outcome,
                        Err(error) => {
                            tracing::warn!(error = %error, "could not record harness.hook.ran; the hook's outcome does not apply");
                            blocked(hook, "the hook's audit record could not be written")
                        }
                    }
                }
                None => outcome,
            };
            dispatch.outcomes.push((hook.clone(), outcome));
        }
        dispatch
    }

    fn charge(&self, invocation_id: &str, duration: Duration) {
        let mut spent = self.spent.lock().unwrap_or_else(|e| e.into_inner());
        if spent.len() >= MAP_LIMIT && !spent.contains_key(invocation_id) {
            spent.clear();
        }
        let entry = spent.entry(invocation_id.to_string()).or_default();
        *entry = entry.saturating_add(duration);
    }

    /// Runs one hook process: the event JSON on stdin, the outcome on stdout, stderr logged.
    async fn run_hook(
        &self,
        hook: &HookSpec,
        launcher: &dyn CommandLauncher,
        event: HookEvent,
        input: &Value,
        scope: &dyn ToolContext,
    ) -> HookRun {
        let timeout = hook.timeout.min(self.workspace.bash_timeout_value());
        let cap = self.workspace.max_output();
        let started = Instant::now();
        let failed = |error: String, started: Instant| HookRun {
            exit_code: None,
            stdout: Vec::new(),
            duration: started.elapsed(),
            timed_out: false,
            error: Some(error),
        };
        let mut cmd = match launcher.command(&self.workspace, &hook.command, scope) {
            Ok(cmd) => cmd,
            Err(error) => {
                return failed(format!("the hook command could not be built: {error}"), started);
            }
        };
        cmd.env(HOOK_EVENT_ENV, event.as_str());
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(unix)]
        {
            // SAFETY: the closure runs in the forked child before `exec` and calls only
            // `setsid`, which is async-signal-safe and touches no memory of the parent.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => return failed(format!("the hook could not start: {error}"), started),
        };
        let mut group = ProcessGroupGuard { pid: child.id() };
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let payload = serde_json::to_vec(input).unwrap_or_default();
        let result = tokio::time::timeout(timeout, async {
            let feed = async {
                if let Some(mut stdin) = stdin {
                    // A hook that never reads its input closes the pipe; that is not an error.
                    let _ = stdin.write_all(&payload).await;
                    let _ = stdin.shutdown().await;
                }
            };
            let exited = async {
                // The hook has exited but is not reaped, so its pid, which is the group id,
                // cannot be reused while the rest of the group is killed; that also closes
                // the pipes a background process holds open.
                if wait_for_exit(child.id()).await {
                    group.kill();
                }
            };
            let ((), out, err, ()) =
                tokio::join!(feed, capture(stdout, cap), capture(stderr, cap), exited);
            let status = child.wait().await?;
            group.disarm();
            Ok::<_, std::io::Error>((status, out, err))
        })
        .await;
        let duration = started.elapsed();
        match result {
            Ok(Ok((status, stdout, stderr))) => {
                if !stderr.is_empty() {
                    tracing::debug!(
                        hook.event = %event,
                        hook.command_digest = %hook.command_digest(),
                        hook.stderr = %String::from_utf8_lossy(&stderr),
                        "hook stderr"
                    );
                }
                HookRun {
                    exit_code: status.code(),
                    stdout,
                    duration,
                    timed_out: false,
                    error: None,
                }
            }
            Ok(Err(error)) => HookRun {
                exit_code: None,
                stdout: Vec::new(),
                duration,
                timed_out: false,
                error: Some(format!("the hook could not be waited for: {error}")),
            },
            Err(_) => {
                group.kill();
                let _ = child.start_kill();
                HookRun {
                    exit_code: None,
                    stdout: Vec::new(),
                    duration,
                    timed_out: true,
                    error: Some(format!("the hook timed out after {} s", timeout.as_secs())),
                }
            }
        }
    }
}

fn describe_failure(run: &HookRun) -> String {
    match (run.timed_out, run.exit_code) {
        (true, _) => format!("timed out after {} ms", run.duration.as_millis()),
        (false, Some(code)) => format!("exited with status {code}"),
        (false, None) => "crashed: it ended without an exit status".to_string(),
    }
}

/// What `hook`'s failure, described by `why`, does under its [`HookFailure`]: a `PreTool`
/// hook that blocks denies, a `PostTool` or `PostEdit` one fails the result, and anything
/// else continues.
fn blocked(hook: &HookSpec, why: &str) -> HookOutcome {
    match hook.failure() {
        super::HookFailure::Block => hook.event.narrow(match hook.event {
            HookEvent::PreTool => HookOutcome::Deny { reason: why.to_string() },
            _ => HookOutcome::Fail { message: why.to_string() },
        }),
        super::HookFailure::Continue => HookOutcome::Continue,
    }
}

fn session_key(app: &str, user: &str, session: &str) -> String {
    format!("{app}\u{1f}{user}\u{1f}{session}")
}

/// `value` as JSON, cut to [`INPUT_PAYLOAD_MAX_BYTES`] when serialized.
fn capped(value: &Value) -> Value {
    let text = value.to_string();
    if text.len() <= INPUT_PAYLOAD_MAX_BYTES {
        return value.clone();
    }
    json!({ "truncated": true, "text": cut(&text, INPUT_PAYLOAD_MAX_BYTES) })
}

fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = text[..text.floor_char_boundary(max)].to_string();
    cut.push_str("\n…[truncated]");
    cut
}

/// The `PostTool`/`PostEdit` verdict of a dispatch: over budget fails with the reason;
/// the first `Fail` wins; otherwise the notes of every `Annotate`.
fn post_verdict(dispatch: Dispatch) -> PostToolVerdict {
    if let Some(reason) = dispatch.over_budget {
        return PostToolVerdict::Fail { message: reason };
    }
    let mut notes = Vec::new();
    for (hook, outcome) in dispatch.outcomes {
        match outcome {
            HookOutcome::Fail { message } => return PostToolVerdict::Fail { message },
            HookOutcome::Annotate { text } => {
                notes.push(HookNote::new(hook.source, hook.command_digest(), &text));
            }
            HookOutcome::Continue | HookOutcome::Deny { .. } => {}
        }
    }
    if notes.is_empty() { PostToolVerdict::Continue } else { PostToolVerdict::Annotate { notes } }
}

/// The notes of a notifier dispatch.
fn notes(dispatch: Dispatch) -> Vec<HookNote> {
    dispatch
        .outcomes
        .into_iter()
        .filter_map(|(hook, outcome)| match outcome {
            HookOutcome::Annotate { text } => {
                Some(HookNote::new(hook.source, hook.command_digest(), &text))
            }
            HookOutcome::Continue | HookOutcome::Deny { .. } | HookOutcome::Fail { .. } => None,
        })
        .collect()
}

fn observe_event(summary: &mut TurnSummary, event: &Event) {
    if let Some(content) = &event.llm_response.content {
        let mut model_call = false;
        for part in &content.parts {
            match part {
                adk_core::Part::FunctionCall { name, .. } => {
                    *summary.tool_calls.entry(name.clone()).or_default() += 1;
                    model_call = true;
                }
                adk_core::Part::Text { .. } if content.role == "model" => model_call = true,
                _ => {}
            }
        }
        if model_call && !event.llm_response.partial {
            summary.model_calls += 1;
        }
    }
    if let Some(usage) = &event.llm_response.usage_metadata {
        summary.prompt_tokens += u64::try_from(usage.prompt_token_count).unwrap_or(0);
        summary.completion_tokens += u64::try_from(usage.candidates_token_count).unwrap_or(0);
    }
}

/// An event carrying a hook's note for the user and nothing for the model.
fn notice_event(ctx: &Arc<dyn InvocationContext>, author: &str, note: &HookNote) -> Event {
    let mut event = Event::new(ctx.invocation_id());
    event.author = author.to_string();
    event.branch = ctx.branch().to_string();
    event.provider_metadata.insert(HOOK_NOTICE_KEY.to_string(), note.render());
    event
}

/// Kills the hook's whole process group, including when dropped; disarmed once the child
/// is reaped, because its pid may then be reused.
struct ProcessGroupGuard {
    pid: Option<u32>,
}

impl ProcessGroupGuard {
    fn kill(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // SAFETY: `killpg` takes a process-group id and a signal, and cannot violate
            // memory safety. A failure means the group already exited.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = self.pid;
    }

    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Waits until the child `pid` has exited, without reaping it (`waitid` with `WNOWAIT`).
#[cfg(unix)]
async fn wait_for_exit(pid: Option<u32>) -> bool {
    let Some(pid) = pid else { return false };
    let waited = tokio::task::spawn_blocking(move || {
        loop {
            // SAFETY: `info` is a zeroed, writable `siginfo_t` that outlives the call,
            // and `waitid` writes only into it.
            let status = unsafe {
                let mut info: libc::siginfo_t = std::mem::zeroed();
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if status == 0 {
                return true;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return false;
            }
        }
    })
    .await;
    waited.unwrap_or(false)
}

/// Off Unix there is no process group to kill, so the exit is observed by `Child::wait`.
#[cfg(not(unix))]
async fn wait_for_exit(_pid: Option<u32>) -> bool {
    false
}

/// Reads one output stream to EOF, keeping at most `cap` bytes and discarding the rest so
/// a writer blocked on a full pipe cannot hang the hook until its timeout.
async fn capture(pipe: Option<impl AsyncRead + Unpin>, cap: usize) -> Vec<u8> {
    let mut kept = Vec::new();
    let Some(mut pipe) = pipe else {
        return kept;
    };
    let mut chunk = [0u8; 8192];
    loop {
        let read = match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let room = cap.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..read.min(room)]);
    }
    kept
}

/// A tool context for a hook that runs outside a tool call: inside the policy decision,
/// at a hold, or at the start or end of a turn (spec 3.1 §7.22).
///
/// It carries the identity a launcher and an audit record need and nothing a hook could
/// use to reach session state or memory.
///
/// # Example
///
/// ```rust
/// use adk_core::{ReadonlyContext, ToolContext};
/// use adk_harness::hooks::HookScope;
///
/// let scope = HookScope::new("app", "alice", "s1", "inv-1", "harness");
/// assert_eq!(scope.session_id(), "s1");
/// assert_eq!(scope.function_call_id(), "hook");
/// ```
pub struct HookScope {
    app: String,
    user: String,
    session: String,
    invocation: String,
    agent: String,
    branch: String,
    function_call_id: String,
    acting: Option<ActingContext>,
    run_config: Option<RunConfig>,
    content: Content,
    actions: Mutex<EventActions>,
}

impl HookScope {
    /// A scope for `agent` in the run `invocation` of the session.
    pub fn new(
        app: impl Into<String>,
        user: impl Into<String>,
        session: impl Into<String>,
        invocation: impl Into<String>,
        agent: impl Into<String>,
    ) -> Self {
        let agent = agent.into();
        Self {
            app: app.into(),
            user: user.into(),
            session: session.into(),
            invocation: invocation.into(),
            branch: agent.clone(),
            agent,
            function_call_id: "hook".to_string(),
            acting: None,
            run_config: None,
            content: Content::new("user"),
            actions: Mutex::new(EventActions::default()),
        }
    }

    /// The scope of the call `request` describes.
    pub(crate) fn from_request(request: &ToolPolicyRequest, call_id: Option<&str>) -> Self {
        let mut scope = Self::new(
            &request.app_name,
            &request.user_id,
            &request.session_id,
            &request.invocation_id,
            &request.agent_name,
        );
        scope.acting = request.acting.clone();
        if let Some(call_id) = call_id {
            scope.function_call_id = call_id.to_string();
        }
        scope
    }

    /// The scope of the run `ctx` describes, with its run config.
    pub(crate) fn from_invocation(ctx: &dyn InvocationContext) -> Self {
        let mut scope = Self::new(
            ctx.app_name(),
            ctx.user_id(),
            ctx.session_id(),
            ctx.invocation_id(),
            ctx.agent_name(),
        );
        scope.branch = ctx.branch().to_string();
        scope.acting = Some(ctx.acting_context());
        scope.run_config = Some(ctx.run_config().clone());
        scope
    }

    /// Carries `config` as the scope's run config.
    #[must_use]
    pub fn with_run_config(mut self, config: RunConfig) -> Self {
        self.run_config = Some(config);
        self
    }
}

#[async_trait]
impl ReadonlyContext for HookScope {
    fn invocation_id(&self) -> &str {
        &self.invocation
    }

    fn agent_name(&self) -> &str {
        &self.agent
    }

    fn user_id(&self) -> &str {
        &self.user
    }

    fn app_name(&self) -> &str {
        &self.app
    }

    fn session_id(&self) -> &str {
        &self.session
    }

    fn branch(&self) -> &str {
        &self.branch
    }

    fn user_content(&self) -> &Content {
        &self.content
    }

    fn acting_context(&self) -> ActingContext {
        match &self.acting {
            Some(acting) => acting.clone(),
            None => ActingContext::derived(
                &self.app,
                &self.agent,
                &self.user,
                adk_core::SubjectKind::Human,
            ),
        }
    }
}

#[async_trait]
impl CallbackContext for HookScope {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for HookScope {
    fn function_call_id(&self) -> &str {
        &self.function_call_id
    }

    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap_or_else(|e| e.into_inner()) = actions;
    }

    async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    fn run_config(&self) -> Option<&RunConfig> {
        self.run_config.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{HookEvent, HookFailure, HookSpec, PRE_TOOL_HOOK_TIMEOUT};

    fn workspace() -> Workspace {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // The directory lives as long as the test; leak it so the workspace stays valid.
        std::mem::forget(dir);
        Workspace::new(root).allow_bash(true)
    }

    fn runner(hooks: Vec<HookSpec>) -> HookRunner {
        let mut set = HookSet::new();
        for hook in hooks {
            set = set.with(hook);
        }
        HookRunner::new(set, workspace(), IsolationStatus::Host).with_workspace_trust(|| true)
    }

    fn call<'a>(tool: &'a str, args: &'a Value) -> HookCall<'a> {
        HookCall { tool, args, class: "shell_host", acting: None, function_call_id: Some("c1") }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pre_tool_hook_denies_from_stdout_and_sees_its_input() {
        let runner = runner(vec![
            HookSpec::new(
                HookEvent::PreTool,
                r#"grep -q '"command":"rm -rf /"' && echo '{"decision":"deny","reason":"no rm -rf"}' || echo '{}'"#,
            )
            .matching("bash"),
        ]);
        let scope = HookScope::new("app", "alice", "s1", "inv", "harness");
        let args = json!({ "command": "rm -rf /" });
        let verdict = runner.pre_tool(&call("bash", &args), HarnessMode::Auto, &scope, None).await;
        assert_eq!(verdict, PreToolVerdict::Deny { reason: "no rm -rf".into() });
        let args = json!({ "command": "ls" });
        let verdict = runner.pre_tool(&call("bash", &args), HarnessMode::Auto, &scope, None).await;
        assert_eq!(verdict, PreToolVerdict::Continue);
        let verdict =
            runner.pre_tool(&call("write_file", &args), HarnessMode::Auto, &scope, None).await;
        assert_eq!(verdict, PreToolVerdict::Continue, "the matcher excludes other tools");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_allow_is_continue_and_a_failed_pre_tool_hook_blocks_by_default() {
        let scope = HookScope::new("app", "alice", "s1", "inv", "harness");
        let args = json!({});
        let allow =
            runner(vec![HookSpec::new(HookEvent::PreTool, r#"echo '{"decision":"allow"}'"#)]);
        assert_eq!(
            allow.pre_tool(&call("bash", &args), HarnessMode::Ask, &scope, None).await,
            PreToolVerdict::Continue
        );
        // An error (a non-zero exit, whose stdout is discarded), a crash (killed by a
        // signal) and a timeout each deny the call, naming the failure.
        for (command, timeout, named) in [
            (
                r#"echo '{"decision":"continue"}'; exit 3"#,
                PRE_TOOL_HOOK_TIMEOUT,
                "exited with status 3",
            ),
            ("kill -9 $$", PRE_TOOL_HOOK_TIMEOUT, "crashed"),
            ("sleep 5", Duration::from_millis(200), "timed out"),
        ] {
            let failing = HookSpec::new(HookEvent::PreTool, command).timeout(timeout);
            let started = Instant::now();
            let verdict = runner(vec![failing.clone()])
                .pre_tool(&call("bash", &args), HarnessMode::Ask, &scope, None)
                .await;
            assert!(
                matches!(&verdict, PreToolVerdict::Deny { reason } if reason.contains(named)),
                "{command}: {verdict:?}"
            );
            assert!(started.elapsed() < Duration::from_secs(3), "the timeout kills the hook");
            // An explicit opt-out fails open.
            let open = runner(vec![failing.on_failure(HookFailure::Continue)]);
            assert_eq!(
                open.pre_tool(&call("bash", &args), HarnessMode::Ask, &scope, None).await,
                PreToolVerdict::Continue,
                "{command}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_post_tool_hook_continues_unless_it_blocks() {
        let scope = HookScope::new("app", "alice", "s1", "inv", "harness");
        let args = json!({});
        let value = json!({ "ok": true });
        let result = HookResult {
            result: &value,
            is_error: false,
            exit_code: Some(0),
            duration: Duration::ZERO,
        };
        let observer = runner(vec![HookSpec::new(HookEvent::PostTool, "exit 2")]);
        assert_eq!(
            observer.post_tool(&call("bash", &args), &result, HarnessMode::Ask, &scope, None).await,
            PostToolVerdict::Continue
        );
        let blocking = runner(vec![
            HookSpec::new(HookEvent::PostTool, "exit 2").on_failure(HookFailure::Block),
        ]);
        assert!(matches!(
            blocking.post_tool(&call("bash", &args), &result, HarnessMode::Ask, &scope, None).await,
            PostToolVerdict::Fail { message } if message.contains("exited with status 2")
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_tool_fail_and_annotate_and_event_env() {
        let scope = HookScope::new("app", "alice", "s1", "inv", "harness");
        let args = json!({});
        let result = json!({ "exit_code": 1 });
        let runner = runner(vec![
            HookSpec::new(
                HookEvent::PostTool,
                r#"echo "{\"decision\":\"annotate\",\"text\":\"event=$ADK_HOOK_EVENT\"}""#,
            ),
            HookSpec::new(HookEvent::PostTool, r#"echo '{"decision":"deny","reason":"ignored"}'"#),
        ]);
        let verdict = runner
            .post_tool(
                &call("bash", &args),
                &HookResult {
                    result: &result,
                    is_error: false,
                    exit_code: Some(1),
                    duration: Duration::ZERO,
                },
                HarnessMode::Ask,
                &scope,
                None,
            )
            .await;
        match verdict {
            PostToolVerdict::Annotate { notes } => {
                assert_eq!(notes.len(), 1);
                assert_eq!(notes[0].text, "event=post_tool");
            }
            other => panic!("{other:?}"),
        }
        let failing = self::runner(vec![HookSpec::new(
            HookEvent::PostTool,
            r#"echo '{"decision":"fail","message":"lint"}'"#,
        )]);
        let verdict = failing
            .post_tool(
                &call("bash", &args),
                &HookResult {
                    result: &result,
                    is_error: false,
                    exit_code: Some(0),
                    duration: Duration::ZERO,
                },
                HarnessMode::Ask,
                &scope,
                None,
            )
            .await;
        assert_eq!(verdict, PostToolVerdict::Fail { message: "lint".into() });
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn untrusted_workspace_hooks_do_not_run_and_host_hooks_need_operator_in_auto() {
        let scope = HookScope::new("app", "alice", "s1", "inv", "harness");
        let args = json!({});
        let set = HookSet::new().with(
            HookSpec::new(HookEvent::PreTool, r#"echo '{"decision":"deny"}'"#)
                .source(HookSource::Workspace),
        );
        let untrusted = HookRunner::new(set.clone(), workspace(), IsolationStatus::Host);
        assert_eq!(
            untrusted.pre_tool(&call("bash", &args), HarnessMode::Ask, &scope, None).await,
            PreToolVerdict::Continue
        );
        let trusted =
            HookRunner::new(set, workspace(), IsolationStatus::Host).with_workspace_trust(|| true);
        assert!(matches!(
            trusted.pre_tool(&call("bash", &args), HarnessMode::Ask, &scope, None).await,
            PreToolVerdict::Deny { .. }
        ));

        let host_user = runner(vec![
            HookSpec::new(HookEvent::PreTool, r#"echo '{"decision":"deny"}'"#)
                .isolation(HookIsolation::Host)
                .source(HookSource::User),
        ]);
        let in_auto =
            host_user.pre_tool(&call("bash", &args), HarnessMode::Auto, &scope, None).await;
        assert!(
            matches!(&in_auto, PreToolVerdict::Deny { reason } if reason.contains("operator")),
            "{in_auto:?}"
        );
        assert!(
            matches!(host_user.pre_tool(&call("bash", &args), HarnessMode::Ask, &scope, None).await, PreToolVerdict::Deny { reason } if !reason.contains("operator"))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_turn_budget_denies_once_spent() {
        let mut set =
            HookSet::new().with(HookSpec::new(HookEvent::PreTool, "sleep 0.3; echo '{}'"));
        set.max_time_per_turn = Duration::from_millis(250);
        let runner = HookRunner::new(set, workspace(), IsolationStatus::Host);
        let scope = HookScope::new("app", "alice", "s1", "inv", "harness");
        let args = json!({});
        assert_eq!(
            runner.pre_tool(&call("bash", &args), HarnessMode::Ask, &scope, None).await,
            PreToolVerdict::Continue
        );
        assert!(runner.over_budget("inv"));
        let verdict = runner.pre_tool(&call("bash", &args), HarnessMode::Ask, &scope, None).await;
        assert!(
            matches!(&verdict, PreToolVerdict::Deny { reason } if reason.contains("budget")),
            "{verdict:?}"
        );
        let other = HookScope::new("app", "alice", "s1", "inv-2", "harness");
        assert_eq!(
            runner.pre_tool(&call("bash", &args), HarnessMode::Ask, &other, None).await,
            PreToolVerdict::Continue
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_start_runs_once_and_joins_the_environment() {
        let runner = runner(vec![HookSpec::new(
            HookEvent::OnSessionStart,
            r#"echo '{"decision":"annotate","text":"node 22 available"}'"#,
        )]);
        let scope = HookScope::new("app", "alice", "s1", "inv", "harness");
        let first = runner.on_session_start(HarnessMode::Ask, &scope, None).await;
        assert_eq!(first.len(), 1);
        let again = runner.on_session_start(HarnessMode::Ask, &scope, None).await;
        assert_eq!(again, first, "cached; the hook did not run again");
        let instruction = runner.with_session_notes(
            "<environment>\nroot: /x\n</environment>\n\nrest".to_string(),
            "app",
            "alice",
            "s1",
        );
        assert!(instruction.contains("<hook-note source=\"operator\""));
        assert!(
            instruction.find("</environment>").unwrap() > instruction.find("<hook-note").unwrap()
        );
        let other = runner.with_session_notes("x".to_string(), "app", "alice", "s2");
        assert_eq!(other, "x");
    }

    #[test]
    fn capped_payloads_and_cuts() {
        let small = json!({ "a": 1 });
        assert_eq!(capped(&small), small);
        let big = Value::String("x".repeat(INPUT_PAYLOAD_MAX_BYTES + 1));
        let capped = capped(&big);
        assert_eq!(capped["truncated"], true);
        assert!(cut("héllo", 2).starts_with('h'));
    }
}
