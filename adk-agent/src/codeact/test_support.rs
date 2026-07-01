//! Test doubles for the CodeAct module: a scripted [`CodeRuntime`] and fake
//! tools/context. Compiled only under `cfg(test)`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::{
    Agent, Artifacts, CallbackContext, Content, Event, EventStream, InvocationContext, Memory,
    ReadonlyContext, RunConfig, Session, State, Tool, ToolContext, Toolset,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codeact::runtime::{
    CodeRuntime, PendingCall, ResumeWith, RunStep, RuntimeCapabilities, RuntimeError,
};

/// One planned step the scripted runtime will emit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Planned {
    /// The script calls an external function.
    Call {
        /// Function (tool) name.
        name: String,
        /// JSON-marshalled arguments.
        args: Value,
        /// Interpreter call id.
        call_id: u64,
        /// stdout the script printed before reaching this call (empty if none).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        stdout: String,
    },
    /// The script completes, returning this value.
    Complete(Value),
    /// An error propagates to the top, with this (verbatim) message.
    Raised(String),
}

impl Planned {
    /// Convenience constructor for a [`Planned::Call`].
    pub(crate) fn call(name: &str, args: Value, call_id: u64) -> Self {
        Planned::Call { name: name.to_string(), args, call_id, stdout: String::new() }
    }

    /// A [`Planned::Call`] that also reports `stdout` printed before the call.
    pub(crate) fn call_with_stdout(name: &str, args: Value, call_id: u64, stdout: &str) -> Self {
        Planned::Call { name: name.to_string(), args, call_id, stdout: stdout.to_string() }
    }
}

/// A [`CodeRuntime`] that replays predetermined [`Planned`] programs.
///
/// Each call to [`CodeRuntime::start`] pops the next program. The `script`
/// string is ignored; behaviour is driven entirely by the planned steps.
pub(crate) struct ScriptedRuntime {
    programs: Mutex<std::collections::VecDeque<Vec<Planned>>>,
    log: ResumeLog,
    supports_suspension: bool,
}

/// Shared record of the most recent value/error a call was resumed with.
#[derive(Clone, Default)]
pub(crate) struct ResumeLog {
    raise: Arc<Mutex<Option<String>>>,
    value: Arc<Mutex<Option<Value>>>,
}

impl ResumeLog {
    fn record(&self, with: &ResumeWith) {
        match with {
            ResumeWith::Raise(message) => *self.raise.lock().unwrap() = Some(message.clone()),
            ResumeWith::Value(value) => *self.value.lock().unwrap() = Some(value.clone()),
        }
    }
}

impl ScriptedRuntime {
    /// Create a runtime that will replay `programs` in order (no suspension).
    pub(crate) fn new(programs: Vec<Vec<Planned>>) -> Self {
        Self {
            programs: Mutex::new(programs.into()),
            log: ResumeLog::default(),
            supports_suspension: false,
        }
    }

    /// Create a runtime that reports `supports_suspension = true`.
    pub(crate) fn with_suspension(programs: Vec<Vec<Planned>>) -> Self {
        Self {
            programs: Mutex::new(programs.into()),
            log: ResumeLog::default(),
            supports_suspension: true,
        }
    }

    /// The message of the most recent resume-with-error, if any.
    pub(crate) fn last_raise(&self) -> Option<String> {
        self.log.raise.lock().unwrap().clone()
    }

    /// The value of the most recent resume-with-value, if any.
    pub(crate) fn last_value(&self) -> Option<Value> {
        self.log.value.lock().unwrap().clone()
    }
}

fn step_from(mut remaining: Vec<Planned>, log: ResumeLog) -> RunStep {
    if remaining.is_empty() {
        return RunStep::complete(Value::Null);
    }
    let head = remaining.remove(0);
    match head {
        Planned::Call { name, args, call_id, stdout } => {
            let (positional, keyword) = split_planned_args(args);
            RunStep::call(Box::new(FakePendingCall {
                name,
                positional,
                keyword,
                call_id,
                remaining,
                log,
            }))
            .with_stdout(stdout)
        }
        Planned::Complete(value) => RunStep::complete(value),
        Planned::Raised(message) => RunStep::raised(message),
    }
}

/// Split a planned call's JSON args into the positional/keyword shape the seam
/// now uses: an object becomes keyword args, an array becomes positional args.
fn split_planned_args(args: Value) -> (Vec<Value>, Vec<(String, Value)>) {
    match args {
        Value::Object(map) => (Vec::new(), map.into_iter().collect()),
        Value::Array(items) => (items, Vec::new()),
        Value::Null => (Vec::new(), Vec::new()),
        other => (vec![other], Vec::new()),
    }
}

impl CodeRuntime for ScriptedRuntime {
    fn start(&self, _script: &str, _script_name: &str) -> Result<RunStep, RuntimeError> {
        let program = self
            .programs
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| RuntimeError::Internal("no scripted programs left".into()))?;
        Ok(step_from(program, self.log.clone()))
    }

    fn resume(&self, snapshot: &[u8], with: ResumeWith) -> Result<RunStep, RuntimeError> {
        self.log.record(&with);
        let remaining: Vec<Planned> =
            serde_json::from_slice(snapshot).map_err(|e| RuntimeError::Snapshot(e.to_string()))?;
        Ok(step_from(remaining, self.log.clone()))
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities::new(self.supports_suspension, "scripted test runtime")
    }
}

/// A minimal [`InvocationContext`] backed by an in-memory session state map.
pub(crate) struct MockInvocationContext {
    user_content: Content,
    session: MockSession,
    run_config: RunConfig,
}

impl MockInvocationContext {
    /// Create a context with the given incoming message and empty session state.
    pub(crate) fn new(user_content: Content) -> Self {
        Self {
            user_content,
            session: MockSession::new(HashMap::new()),
            run_config: RunConfig::default(),
        }
    }

    /// Seed the session state (e.g. a persisted checkpoint from a prior run).
    pub(crate) fn with_state(mut self, state: HashMap<String, Value>) -> Self {
        self.session = MockSession::new(state);
        self
    }

    /// Set the runner-provided transfer targets on the run config.
    pub(crate) fn with_transfer_targets(mut self, targets: Vec<String>) -> Self {
        self.run_config.transfer_targets = targets;
        self
    }

    /// Set the runner-provided parent agent name on the run config.
    pub(crate) fn with_parent_agent(mut self, parent: &str) -> Self {
        self.run_config.parent_agent = Some(parent.to_string());
        self
    }

    /// Seed the session's conversation history.
    pub(crate) fn with_history(mut self, history: Vec<Content>) -> Self {
        self.session.history = history;
        self
    }
}

#[async_trait]
impl ReadonlyContext for MockInvocationContext {
    fn invocation_id(&self) -> &str {
        "inv-test"
    }
    fn agent_name(&self) -> &str {
        "code-agent"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "app"
    }
    fn session_id(&self) -> &str {
        "sess-test"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for MockInvocationContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockInvocationContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("MockInvocationContext::agent is not used by CodeActAgent")
    }
    fn memory(&self) -> Option<Arc<dyn Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        &self.run_config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

struct MockSession {
    state: MockState,
    history: Vec<Content>,
}

impl MockSession {
    fn new(map: HashMap<String, Value>) -> Self {
        Self { state: MockState { map }, history: Vec::new() }
    }
}

impl Session for MockSession {
    fn id(&self) -> &str {
        "sess-test"
    }
    fn app_name(&self) -> &str {
        "app"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
    fn conversation_history(&self) -> Vec<Content> {
        self.history.clone()
    }
}

struct MockState {
    map: HashMap<String, Value>,
}

impl State for MockState {
    fn get(&self, key: &str) -> Option<Value> {
        self.map.get(key).cloned()
    }
    fn set(&mut self, key: String, value: Value) {
        self.map.insert(key, value);
    }
    fn all(&self) -> HashMap<String, Value> {
        self.map.clone()
    }
}

struct FakePendingCall {
    name: String,
    positional: Vec<Value>,
    keyword: Vec<(String, Value)>,
    call_id: u64,
    remaining: Vec<Planned>,
    log: ResumeLog,
}

impl PendingCall for FakePendingCall {
    fn function_name(&self) -> &str {
        &self.name
    }

    fn positional_args(&self) -> &[Value] {
        &self.positional
    }

    fn keyword_args(&self) -> &[(String, Value)] {
        &self.keyword
    }

    fn call_id(&self) -> u64 {
        self.call_id
    }

    fn dump(&self) -> Result<Vec<u8>, RuntimeError> {
        serde_json::to_vec(&self.remaining).map_err(|e| RuntimeError::Snapshot(e.to_string()))
    }

    fn resume(self: Box<Self>, with: ResumeWith) -> Result<RunStep, RuntimeError> {
        self.log.record(&with);
        Ok(step_from(self.remaining, self.log))
    }
}

/// A [`Toolset`] that yields a fixed set of tools regardless of context.
pub(crate) fn fake_toolset(name: &str, tools: Vec<Arc<dyn Tool>>) -> Arc<dyn Toolset> {
    Arc::new(FakeToolset { name: name.to_string(), tools })
}

struct FakeToolset {
    name: String,
    tools: Vec<Arc<dyn Tool>>,
}

#[async_trait]
impl Toolset for FakeToolset {
    fn name(&self) -> &str {
        &self.name
    }
    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> adk_core::Result<Vec<Arc<dyn Tool>>> {
        Ok(self.tools.clone())
    }
}

/// A no-op [`Agent`] with the given name, usable as a transfer sub-agent.
pub(crate) fn fake_agent(name: &str) -> Arc<dyn Agent> {
    Arc::new(FakeAgent { name: name.to_string() })
}

struct FakeAgent {
    name: String,
}

#[async_trait]
impl Agent for FakeAgent {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "a no-op test sub-agent"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// An [`Agent`] that emits a single event whose text is `text`. Useful as the
/// inner agent of an [`adk_tool::AgentTool`].
pub(crate) fn text_agent(name: &str, text: &str) -> Arc<dyn Agent> {
    Arc::new(TextAgent { name: name.to_string(), text: text.to_string() })
}

struct TextAgent {
    name: String,
    text: String,
}

#[async_trait]
impl Agent for TextAgent {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "emits a fixed line of text"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        let mut event = Event::new(ctx.invocation_id());
        event.author = self.name.clone();
        event.llm_response.content = Some(Content::new("model").with_text(self.text.clone()));
        Ok(Box::pin(futures::stream::once(async move { Ok(event) })))
    }
}

/// A tool named `echo` that returns its arguments unchanged.
pub(crate) fn echo_tool() -> Arc<dyn Tool> {
    Arc::new(EchoTool)
}

/// A tool named `boom` that always fails with a `NotFound` error.
pub(crate) fn failing_tool() -> Arc<dyn Tool> {
    Arc::new(FailingTool)
}

/// A long-running tool named `slow` that returns a pending handle.
pub(crate) fn long_running_tool() -> Arc<dyn Tool> {
    Arc::new(LongRunningTool)
}

/// A built-in (server-side) tool named `web_search`.
pub(crate) fn builtin_tool() -> Arc<dyn Tool> {
    Arc::new(BuiltinTool)
}

/// A tool named `remember` that writes `note = "kept"` to session state via its
/// [`ToolContext`] actions, then returns `{"ok": true}`.
pub(crate) fn state_tool() -> Arc<dyn Tool> {
    Arc::new(StateTool)
}

/// A tool named `panic_button` that escalates (sets `actions.escalate`).
pub(crate) fn escalating_tool() -> Arc<dyn Tool> {
    Arc::new(EscalatingTool)
}

/// A tool named `wrap_up` that sets `actions.skip_summarization`, then returns
/// `{"ok": true}`. Used to verify skip-summarization ends the run.
pub(crate) fn skip_summarization_tool() -> Arc<dyn Tool> {
    Arc::new(SkipSummarizationTool)
}

/// A tool named `whoami` that returns the `function_call_id` it was invoked with,
/// so tests can assert the per-call tool context carries the interpreter id.
pub(crate) fn call_id_tool() -> Arc<dyn Tool> {
    Arc::new(CallIdTool)
}

/// A tool named `set_route` that sets `actions.route = ["next"]`, then returns
/// `{"ok": true}`. Used to verify route propagation onto persisted events.
pub(crate) fn route_tool() -> Arc<dyn Tool> {
    Arc::new(RouteTool)
}

struct StateTool;

#[async_trait]
impl Tool for StateTool {
    fn name(&self) -> &str {
        "remember"
    }
    fn description(&self) -> &str {
        "writes a value to session state"
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        let mut actions = ctx.actions();
        actions.state_delta.insert("note".to_string(), Value::String("kept".to_string()));
        ctx.set_actions(actions);
        Ok(serde_json::json!({"ok": true}))
    }
}

/// A tool named `sleeper` that never returns within a test timeout (sleeps for
/// an hour), used to exercise `tool_timeout`.
pub(crate) fn sleeping_tool() -> Arc<dyn Tool> {
    Arc::new(SleepingTool)
}

struct SleepingTool;

#[async_trait]
impl Tool for SleepingTool {
    fn name(&self) -> &str {
        "sleeper"
    }
    fn description(&self) -> &str {
        "sleeps far longer than any test timeout"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        Ok(Value::Null)
    }
}

/// A tool named `flaky` that fails its first `fail_times` calls, then succeeds
/// with `{"ok": true}`. Used to exercise retry budgets.
pub(crate) fn flaky_tool(fail_times: usize) -> Arc<dyn Tool> {
    Arc::new(FlakyTool { fail_times, calls: AtomicUsize::new(0) })
}

struct FlakyTool {
    fail_times: usize,
    calls: AtomicUsize,
}

#[async_trait]
impl Tool for FlakyTool {
    fn name(&self) -> &str {
        "flaky"
    }
    fn description(&self) -> &str {
        "fails a few times, then succeeds"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fail_times {
            Err(adk_core::AdkError::new(
                adk_core::ErrorComponent::Tool,
                adk_core::ErrorCategory::Unavailable,
                "tool.flaky.transient",
                "transient failure",
            ))
        } else {
            Ok(serde_json::json!({"ok": true}))
        }
    }
}

struct EscalatingTool;

#[async_trait]
impl Tool for EscalatingTool {
    fn name(&self) -> &str {
        "panic_button"
    }
    fn description(&self) -> &str {
        "escalates to a human operator"
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        let mut actions = ctx.actions();
        actions.escalate = true;
        ctx.set_actions(actions);
        Ok(serde_json::json!({"escalated": true}))
    }
}

struct SkipSummarizationTool;

#[async_trait]
impl Tool for SkipSummarizationTool {
    fn name(&self) -> &str {
        "wrap_up"
    }
    fn description(&self) -> &str {
        "signals the run should stop after this tool"
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        let mut actions = ctx.actions();
        actions.skip_summarization = true;
        ctx.set_actions(actions);
        Ok(serde_json::json!({"ok": true}))
    }
}

struct CallIdTool;

#[async_trait]
impl Tool for CallIdTool {
    fn name(&self) -> &str {
        "whoami"
    }
    fn description(&self) -> &str {
        "returns the function_call_id of this invocation"
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(serde_json::json!({ "call_id": ctx.function_call_id() }))
    }
}

struct RouteTool;

#[async_trait]
impl Tool for RouteTool {
    fn name(&self) -> &str {
        "set_route"
    }
    fn description(&self) -> &str {
        "sets a route on its actions"
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        let mut actions = ctx.actions();
        actions.route = Some(vec!["next".to_string()]);
        ctx.set_actions(actions);
        Ok(serde_json::json!({"ok": true}))
    }
}

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "echoes its arguments"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        Ok(args)
    }
}

struct LongRunningTool;

#[async_trait]
impl Tool for LongRunningTool {
    fn name(&self) -> &str {
        "slow"
    }
    fn description(&self) -> &str {
        "a long-running task"
    }
    fn is_long_running(&self) -> bool {
        true
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(serde_json::json!({"status": "pending", "task_id": "task-1"}))
    }
}

/// A long-running tool named `slow_escalate` that sets `actions.escalate` and
/// returns a pending handle. Used to verify a terminal signal ends the run
/// before suspending on the long-running path.
pub(crate) fn escalating_long_running_tool() -> Arc<dyn Tool> {
    Arc::new(EscalatingLongRunningTool)
}

struct EscalatingLongRunningTool;

#[async_trait]
impl Tool for EscalatingLongRunningTool {
    fn name(&self) -> &str {
        "slow_escalate"
    }
    fn description(&self) -> &str {
        "a long-running task that escalates"
    }
    fn is_long_running(&self) -> bool {
        true
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        let mut actions = ctx.actions();
        actions.escalate = true;
        ctx.set_actions(actions);
        Ok(serde_json::json!({"status": "pending", "task_id": "task-1"}))
    }
}

struct BuiltinTool;

#[async_trait]
impl Tool for BuiltinTool {
    fn name(&self) -> &str {
        "web_search"
    }
    fn description(&self) -> &str {
        "server-side web search"
    }
    fn is_builtin(&self) -> bool {
        true
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(Value::Null)
    }
}

struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        "boom"
    }
    fn description(&self) -> &str {
        "always fails"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Err(adk_core::AdkError::not_found(
            adk_core::ErrorComponent::Tool,
            "tool.boom.not_found",
            "resource missing",
        ))
    }
}
