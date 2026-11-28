//! The managed runtime's built-in tools run confined, and its turns run under the operator's
//! kill switch, tool policy and action ledger.
//!
//! | Property | Tests |
//! |----------|-------|
//! | `bash` under the OS sandbox writes and reads only its session's workspace | `a_confined_command_keeps_to_its_workspace`, `confined_code_execution_runs_in_the_workspace` |
//! | Where no enforcer runs, the built-in commands are refused | `the_default_is_refused_where_no_enforcer_runs`, `windows_refuses_the_default` |
//! | No host variable reaches a command | `a_host_variable_does_not_reach_a_command` |
//! | `filesystem` refuses paths that escape the session workspace, symbolic links included | `file_tools_keep_to_the_session_workspace` |
//! | The opt-in host mode runs unconfined | `the_opted_in_host_mode_is_unconfined` |
//! | A freeze refuses a managed tool call mid-session, and a turn started while frozen | `a_freeze_refuses_a_managed_tool_call_mid_session`, `a_turn_started_while_frozen_is_refused` |
//! | The supplied `ToolPolicy` and action ledger govern every turn | `a_tool_policy_denial_is_honoured`, `the_action_ledger_records_allowed_calls` |
//!
//! Tests that run commands under the platform enforcer (Seatbelt on macOS, bubblewrap on
//! Linux) skip where it cannot start, unless `ADK_SANDBOX_REQUIRE_ENFORCER` is set.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, GovernanceControl, InMemoryActionLedger, Llm,
    LlmRequest, LlmResponseStream, Part, PolicyDecision, Tool, ToolContext, ToolPolicy,
    ToolPolicyRequest,
};
use adk_managed::builtin::{BuiltinTools, SANDBOX_UNAVAILABLE_CODE};
use adk_managed::resolver::{ModelResolver, ResolverResult};
use adk_managed::testing::{ScriptedLlm, ScriptedToolCall, ScriptedTurn};
use adk_managed::types::{
    ContentBlock, ManagedAgentDef, ModelRef, SessionEvent, ToolConfig, UserEvent,
};
use adk_managed::{
    DefaultManagedAgentRuntime, ManagedAgentRuntime, ManagedBuiltinTool, ManagedOwner,
    SessionHandle,
};
use adk_sandbox::SandboxError;
use adk_sandbox::sandbox::{NetworkSupport, SandboxEnforcer, SandboxPolicy, WrappedCommand};
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};

// ─── Built-in tools ───────────────────────────────────────────────────────────

fn ctx(session: &str) -> Arc<dyn ToolContext> {
    Arc::new(adk_tool::SimpleToolContext::new("ops").with_session_id(session))
}

fn tool(name: &str, tools: &BuiltinTools) -> ManagedBuiltinTool {
    ManagedBuiltinTool::new(name, "built-in", tools.clone())
}

/// A canonical temporary directory; both enforcers match resolved paths.
fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("tempdir");
    let canonical = std::fs::canonicalize(directory.path()).expect("canonicalize");
    (directory, canonical)
}

/// The default built-in tools under `root`, or `None` where the platform's enforcer cannot
/// start. With `ADK_SANDBOX_REQUIRE_ENFORCER` set, a host without one fails the test instead.
fn confined(root: &Path) -> Option<BuiltinTools> {
    let tools = BuiltinTools::os_sandbox().with_workspace_root(root);
    match tools.probe() {
        Ok(_) => Some(tools),
        Err(error) => {
            assert!(
                std::env::var_os("ADK_SANDBOX_REQUIRE_ENFORCER").is_none(),
                "ADK_SANDBOX_REQUIRE_ENFORCER is set and no OS sandbox runs here: {error}"
            );
            eprintln!("no OS sandbox on this host, skipping: {error}");
            None
        }
    }
}

/// An enforcer whose probe fails, as a missing `bwrap` does.
struct Unavailable;

impl SandboxEnforcer for Unavailable {
    fn name(&self) -> &str {
        "unavailable"
    }
    fn probe(&self) -> Result<(), SandboxError> {
        Err(SandboxError::EnforcerUnavailable {
            enforcer: "unavailable".into(),
            message: "probe refused for the test".into(),
        })
    }
    fn network_support(&self) -> NetworkSupport {
        NetworkSupport::NONE
    }
    fn wrap_command(
        &self,
        _program: &OsStr,
        _args: &[OsString],
        _policy: &SandboxPolicy,
    ) -> Result<WrappedCommand, SandboxError> {
        Err(SandboxError::PolicyViolation("unavailable".into()))
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_confined_command_keeps_to_its_workspace() {
    let (_root_guard, root) = canonical_tempdir();
    let Some(tools) = confined(&root) else { return };
    let (_outside_guard, outside) = canonical_tempdir();
    std::fs::write(outside.join("secret.txt"), "top-secret").unwrap();
    let escaped = outside.join("escaped.txt");

    let script = format!(
        "echo inside > inside.txt; echo out > '{}'; cat '{}'",
        escaped.display(),
        outside.join("secret.txt").display()
    );
    let result = tool("bash", &tools).execute(ctx("s1"), json!({ "command": script })).await;
    let result = result.expect("the confined command runs");

    let workspace = tools.session_workspace("s1").unwrap();
    assert_eq!(std::fs::read_to_string(workspace.join("inside.txt")).unwrap(), "inside\n");
    assert!(!escaped.exists(), "a write outside the workspace went through: {result}");
    assert!(
        !result["stdout"].as_str().unwrap_or_default().contains("top-secret"),
        "a read outside the workspace went through: {result}"
    );
    assert_ne!(result["exit_code"], 0, "{result}");
}

#[cfg(unix)]
#[tokio::test]
async fn confined_code_execution_runs_in_the_workspace() {
    let (_root_guard, root) = canonical_tempdir();
    let Some(tools) = confined(&root) else { return };

    let result = tool("code_execution", &tools)
        .execute(
            ctx("s1"),
            json!({ "language": "bash", "code": "echo $((6 * 7)) > answer.txt; cat answer.txt" }),
        )
        .await
        .expect("the confined code runs");

    assert_eq!(result["exit_code"], 0, "{result}");
    assert_eq!(result["stdout"].as_str().unwrap().trim(), "42", "{result}");
    let workspace = tools.session_workspace("s1").unwrap();
    assert_eq!(std::fs::read_to_string(workspace.join("answer.txt")).unwrap(), "42\n");
}

#[tokio::test]
async fn the_default_is_refused_where_no_enforcer_runs() {
    let (_root_guard, root) = canonical_tempdir();
    let tools =
        BuiltinTools::os_sandbox().with_workspace_root(&root).with_enforcer(Arc::new(Unavailable));

    let probe = tools.probe().unwrap_err();
    assert_eq!(probe.code, SANDBOX_UNAVAILABLE_CODE);
    for (name, args) in [
        ("bash", json!({ "command": "echo ran > ran.txt" })),
        ("code_execution", json!({ "language": "bash", "code": "echo ran > ran.txt" })),
    ] {
        let error = tool(name, &tools).execute(ctx("s1"), args).await.unwrap_err();
        assert_eq!(error.code, SANDBOX_UNAVAILABLE_CODE, "{name}: {error}");
        assert!(error.message.contains(&format!("`{name}`")), "{error}");
        assert!(error.message.contains("unsafe_unconfined_host"), "{error}");
    }
    assert!(!root.join("s1").exists(), "nothing is created for a refused call");
}

/// Windows has no enforcer for the generic confined command, so the default refuses.
#[cfg(windows)]
#[tokio::test]
async fn windows_refuses_the_default() {
    let (_root_guard, root) = canonical_tempdir();
    let tools = BuiltinTools::os_sandbox().with_workspace_root(&root);

    assert_eq!(tools.probe().unwrap_err().code, SANDBOX_UNAVAILABLE_CODE);
    let error = tool("bash", &tools)
        .execute(ctx("s1"), json!({ "command": "echo ran" }))
        .await
        .unwrap_err();
    assert_eq!(error.code, SANDBOX_UNAVAILABLE_CODE, "{error}");
}

/// Cargo sets `CARGO_MANIFEST_DIR` for the test process; neither mode passes it on.
#[cfg(unix)]
#[tokio::test]
async fn a_host_variable_does_not_reach_a_command() {
    const HOST_VARIABLE: &str = "CARGO_MANIFEST_DIR";
    assert!(std::env::var_os(HOST_VARIABLE).is_some(), "cargo sets {HOST_VARIABLE} for tests");
    let (_root_guard, root) = canonical_tempdir();
    let mut modes = vec![BuiltinTools::unsafe_unconfined_host().with_workspace_root(&root)];
    modes.extend(confined(&root));

    for tools in modes {
        let result = tool("bash", &tools)
            .execute(ctx("s1"), json!({ "command": "env" }))
            .await
            .expect("env runs");
        let environment = result["stdout"].as_str().unwrap_or_default();
        assert!(environment.contains("PATH="), "{tools:?}: {environment}");
        assert!(
            !environment.lines().any(|line| line.starts_with(&format!("{HOST_VARIABLE}="))),
            "{tools:?} passed a host variable: {environment}"
        );
    }
}

#[tokio::test]
async fn file_tools_keep_to_the_session_workspace() {
    let (_root_guard, root) = canonical_tempdir();
    let (_outside_guard, outside) = canonical_tempdir();
    std::fs::write(outside.join("secret.txt"), "top-secret").unwrap();
    // `filesystem` needs no enforcer, so it works wherever the runtime does.
    let tools = BuiltinTools::os_sandbox().with_workspace_root(&root);
    let files = tool("filesystem", &tools);
    let call = |args: Value| files.execute(ctx("s1"), args);

    let written = call(json!({ "operation": "write", "path": "notes/a.txt", "content": "hello" }))
        .await
        .unwrap();
    assert_eq!(written["bytes_written"], 5, "{written}");
    let read = call(json!({ "operation": "read", "path": "notes/a.txt" })).await.unwrap();
    assert!(read["content"].as_str().unwrap().contains("hello"), "{read}");
    let listed = call(json!({ "operation": "list", "path": "notes" })).await.unwrap();
    assert_eq!(listed["files"], json!(["a.txt"]), "{listed}");

    let secret = outside.join("secret.txt").display().to_string();
    for args in [
        json!({ "operation": "read", "path": "../../secret.txt" }),
        json!({ "operation": "read", "path": secret }),
        json!({ "operation": "write", "path": "../escaped.txt", "content": "x" }),
        json!({ "operation": "list", "path": outside.display().to_string() }),
    ] {
        let error = call(args.clone()).await.unwrap_err();
        assert_eq!(error.code, "devtools.path_escape", "{args}: {error}");
    }
    assert!(!root.join("s1").join("escaped.txt").exists());

    // Another session has a workspace of its own.
    let other = files.execute(ctx("s2"), json!({ "operation": "list" })).await.unwrap();
    assert_eq!(other["files"], json!([]), "{other}");

    #[cfg(unix)]
    {
        let workspace = tools.session_workspace("s1").unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("door")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), workspace.join("alias")).unwrap();
        for args in [
            json!({ "operation": "read", "path": "door/secret.txt" }),
            json!({ "operation": "read", "path": "alias" }),
            json!({ "operation": "write", "path": "door/planted.txt", "content": "x" }),
            json!({ "operation": "list", "path": "door" }),
        ] {
            let error = call(args.clone()).await.unwrap_err();
            assert_eq!(error.code, "devtools.path_escape", "{args}: {error}");
        }
        assert!(!outside.join("planted.txt").exists(), "a write followed a symbolic link out");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_opted_in_host_mode_is_unconfined() {
    let (_root_guard, root) = canonical_tempdir();
    let (_outside_guard, outside) = canonical_tempdir();
    let tools = BuiltinTools::unsafe_unconfined_host().with_workspace_root(&root);
    assert_eq!(tools.probe().unwrap(), "unsafe-unconfined-host");

    let target = outside.join("written-from-host.txt");
    let result = tool("bash", &tools)
        .execute(
            ctx("s1"),
            json!({ "command": format!("pwd; echo host > '{}'", target.display()) }),
        )
        .await
        .expect("the host command runs");

    assert_eq!(result["exit_code"], 0, "{result}");
    let workspace = tools.session_workspace("s1").unwrap();
    assert_eq!(result["stdout"].as_str().unwrap().trim(), workspace.display().to_string());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "host\n", "nothing confines host mode");
}

// ─── Governance through the runtime ──────────────────────────────────────────

/// A scripted model that records its requests and, when given a control, freezes it while
/// answering the request numbered `freeze_on` (from 1), as an operator acting mid-turn would.
struct Model {
    scripted: ScriptedLlm,
    requests: Mutex<Vec<LlmRequest>>,
    freeze: Option<(GovernanceControl, usize)>,
}

impl Model {
    fn new(turns: Vec<ScriptedTurn>) -> Arc<Self> {
        Arc::new(Self {
            scripted: ScriptedLlm::new("scripted", turns),
            requests: Mutex::default(),
            freeze: None,
        })
    }

    fn freezing(turns: Vec<ScriptedTurn>, control: GovernanceControl, on: usize) -> Arc<Self> {
        Arc::new(Self {
            scripted: ScriptedLlm::new("scripted", turns),
            requests: Mutex::default(),
            freeze: Some((control, on)),
        })
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The function responses the model was shown in its last request.
    fn last_responses(&self) -> Vec<Value> {
        let requests = self.requests.lock().unwrap();
        requests
            .last()
            .expect("the model was called")
            .contents
            .iter()
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, .. } => {
                    Some(function_response.response.clone())
                }
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl Llm for Model {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let call = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        if let Some((control, on)) = &self.freeze
            && call == *on
        {
            control.freeze("incident-42: managed tools halted");
        }
        self.scripted.generate_content(request, stream).await
    }
}

struct Resolver(Arc<Model>);

#[async_trait]
impl ModelResolver for Resolver {
    async fn resolve(&self, _model: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        Ok(self.0.clone() as Arc<dyn Llm>)
    }
}

/// Refuses `bash`, allows everything else.
#[derive(Debug)]
struct NoShell;

#[async_trait]
impl ToolPolicy for NoShell {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        if request.tool_name == "bash" {
            PolicyDecision::deny("bash is off for this tenant")
        } else {
            PolicyDecision::Allow
        }
    }
}

/// An in-memory ledger that also keeps every record it was asked to begin.
#[derive(Debug, Default)]
struct RecordingLedger {
    inner: InMemoryActionLedger,
    begun: Mutex<Vec<ActionRecord>>,
}

#[async_trait]
impl ActionLedger for RecordingLedger {
    async fn begin(&self, record: &ActionRecord) -> adk_core::Result<()> {
        self.begun.lock().unwrap().push(record.clone());
        self.inner.begin(record).await
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> adk_core::Result<()> {
        self.inner.complete(key, outcome).await
    }

    async fn get(&self, key: &str) -> adk_core::Result<Option<ActionRecord>> {
        self.inner.get(key).await
    }
}

fn call(id: &str, command: &str) -> ScriptedTurn {
    ScriptedTurn {
        text: None,
        tool_calls: vec![ScriptedToolCall {
            name: "bash".into(),
            input: json!({ "command": command }),
            id: Some(id.into()),
        }],
    }
}

fn say(text: &str) -> ScriptedTurn {
    ScriptedTurn { text: Some(text.into()), tool_calls: vec![] }
}

fn message(text: &str) -> UserEvent {
    UserEvent::Message { content: vec![ContentBlock::Text { text: text.into() }] }
}

/// A runtime over `model` whose built-in tools run on the host under `root`, so the governed
/// path is what these tests observe on every platform.
fn runtime(model: &Arc<Model>, root: &Path) -> DefaultManagedAgentRuntime {
    DefaultManagedAgentRuntime::new(
        Arc::new(Resolver(model.clone())),
        Arc::new(adk_session::InMemorySessionService::new()),
    )
    .with_builtin_tools(BuiltinTools::unsafe_unconfined_host().with_workspace_root(root))
}

async fn start(
    runtime: &DefaultManagedAgentRuntime,
) -> (SessionHandle, BoxStream<'static, SessionEvent>) {
    let def = ManagedAgentDef::new("operator", ModelRef::Shorthand("gpt-5.4-mini".into()))
        .with_tools(vec![ToolConfig::Bash {}]);
    let agent = runtime.create(def).await.unwrap();
    let owner = ManagedOwner::new("ops-console", "user-7").unwrap();
    let session = runtime.start_session(&agent, &owner, None).await.unwrap();
    let stream = runtime.stream_events(&session, None).await.unwrap();
    (session, stream)
}

/// Every event of one turn, up to and including its `status.idle`.
async fn turn(stream: &mut BoxStream<'static, SessionEvent>) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), stream.next())
            .await
            .unwrap_or_else(|_| panic!("no status.idle within 30 s; saw {events:?}"))
            .expect("stream ended");
        let idle = matches!(event, SessionEvent::StatusIdle { .. });
        events.push(event);
        if idle {
            return events;
        }
    }
}

fn errors(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Error { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_freeze_refuses_a_managed_tool_call_mid_session() {
    let (_root_guard, root) = canonical_tempdir();
    let (_marker_guard, markers) = canonical_tempdir();
    let marker = markers.join("ran.txt");
    let control = GovernanceControl::new();
    let model = Model::freezing(
        vec![
            say("Ready."),
            call("call_touch", &format!("touch '{}'", marker.display())),
            say("Back."),
        ],
        control.clone(),
        2,
    );
    let runtime = runtime(&model, &root).with_governance(control.clone());
    let (session, mut stream) = start(&runtime).await;

    runtime.send_event(&session, message("hello")).await.unwrap();
    assert!(errors(&turn(&mut stream).await).is_empty(), "the session runs before the freeze");

    // The operator freezes while the model answers with the tool call.
    runtime.send_event(&session, message("touch the marker")).await.unwrap();
    let refused = errors(&turn(&mut stream).await);
    assert!(
        refused.iter().any(|message| message.contains("incident-42")),
        "the freeze is reported: {refused:?}"
    );
    assert!(!marker.exists(), "the tool call ran while frozen");
    assert_eq!(model.calls(), 2, "no model call follows the refused tool call");

    // The session outlives the freeze.
    control.unfreeze();
    runtime.send_event(&session, message("still there?")).await.unwrap();
    assert!(errors(&turn(&mut stream).await).is_empty());
    assert_eq!(model.calls(), 3);
}

#[tokio::test]
async fn a_turn_started_while_frozen_is_refused() {
    let (_root_guard, root) = canonical_tempdir();
    let model = Model::new(vec![say("Never said.")]);
    let runtime = runtime(&model, &root);
    let (session, mut stream) = start(&runtime).await;

    // The runtime's own control reaches every turn's Runner.
    runtime.governance().freeze("change freeze");
    runtime.send_event(&session, message("hello")).await.unwrap();

    let refused = errors(&turn(&mut stream).await);
    assert!(refused.iter().any(|message| message.contains("change freeze")), "{refused:?}");
    assert_eq!(model.calls(), 0, "a frozen turn never reaches the model");
}

#[tokio::test]
async fn a_tool_policy_denial_is_honoured() {
    let (_root_guard, root) = canonical_tempdir();
    let (_marker_guard, markers) = canonical_tempdir();
    let marker = markers.join("ran.txt");
    let model = Model::new(vec![
        call("call_touch", &format!("touch '{}'", marker.display())),
        say("Understood."),
    ]);
    let runtime = runtime(&model, &root).with_tool_policy(Arc::new(NoShell));
    let (session, mut stream) = start(&runtime).await;

    runtime.send_event(&session, message("touch the marker")).await.unwrap();
    turn(&mut stream).await;

    assert!(!marker.exists(), "the denied call ran");
    let responses = model.last_responses();
    assert_eq!(responses.len(), 1, "{responses:?}");
    assert!(responses[0].to_string().contains("bash is off for this tenant"), "{responses:?}");
}

#[tokio::test]
async fn the_action_ledger_records_allowed_calls() {
    let (_root_guard, root) = canonical_tempdir();
    let model = Model::new(vec![call("call_echo", "echo ledgered"), say("Done.")]);
    let ledger = Arc::new(RecordingLedger::default());
    let runtime = runtime(&model, &root).with_action_ledger(ledger.clone());
    let (session, mut stream) = start(&runtime).await;

    runtime.send_event(&session, message("echo it")).await.unwrap();
    let events = turn(&mut stream).await;
    assert!(errors(&events).is_empty(), "{events:?}");

    let begun = ledger.begun.lock().unwrap().clone();
    assert_eq!(begun.len(), 1, "{begun:?}");
    assert_eq!(begun[0].tool_name, "bash");
    let stored = ledger.get(&begun[0].key).await.unwrap().expect("the call is in the ledger");
    assert!(stored.is_completed(), "the outcome is recorded: {stored:?}");
    assert!(
        model.last_responses()[0]["stdout"].as_str().unwrap_or_default().contains("ledgered"),
        "the allowed call ran"
    );
}
