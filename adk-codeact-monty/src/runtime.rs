//! [`MontyRuntime`] — a Python [`CodeRuntime`] backed by Pydantic Monty.
//!
//! Monty's iterative execution model is a near-perfect fit for the CodeAct seam:
//! [`MontyRun::start`] runs a script until it calls an external function, hands
//! the host a [`FunctionCall`] it can inspect, run a tool for, and resume — and
//! the whole suspended continuation can be serialized to bytes with
//! [`RunProgress::dump`] and restored later with [`RunProgress::load`]. That is
//! exactly what [`PendingCall::dump`] / [`CodeRuntime::resume`] need to persist a
//! paused run to session state and continue it on a later invocation.
//!
//! # The driver
//!
//! Tools are the only suspension the *agent* handles, so [`drive`] runs the
//! interpreter forward until it reaches a tool call ([`RunStep::Call`]) or
//! finishes ([`RunStep::Complete`]). The other Monty suspension points are
//! resolved in-place by the runtime:
//!
//! - an **OS call** (filesystem, environment, clock) is serviced against the
//!   host's [`OsAccess`] policy and resumed immediately — OS calls are never
//!   tools and never pause the agent loop;
//! - a **name lookup** for an undefined name raises `NameError`;
//! - a blocked **`await`** on external futures is refused, steering the model
//!   toward synchronous tool calls.
//!
//! A Python exception that propagates to the top of a script is **not** a host
//! error — it is the model's mistake. It surfaces as [`RunStep::Raised`] carrying
//! Monty's traceback (and any parse/compile failure does too), which the agent
//! feeds back verbatim for the model to fix. [`RuntimeError`] is reserved for
//! genuine host failures (snapshot (de)serialization).
//!
//! # No shared state
//!
//! Because the CodeAct driver binds tool arguments centrally (it maps a call's
//! positional/keyword arguments onto the tool schema for us), this runtime is
//! entirely stateless: it converts a call's arguments to JSON and hands them up,
//! and never needs to remember tool schemas between [`CodeRuntime::render_tools`]
//! and the call boundary.
//!
//! # stdout
//!
//! Monty captures `print()` output per step (via `PrintWriter::CollectString`),
//! which is attached to each [`RunStep`] so the agent can surface it back to the
//! model.

use std::sync::Arc;
use std::time::Duration;

use adk_agent::codeact::{
    CodeRuntime, PendingCall, ResumeWith, RunStep, RuntimeCapabilities, RuntimeError,
};
use adk_core::Tool;
use monty::{
    ExcType, ExtFunctionResult, FunctionCall, LimitedTracker, MontyException, MontyObject,
    MontyRun, NameLookupResult, PrintWriter, ResourceLimits, RunProgress,
};
use serde_json::Value;

use crate::convert::{json_to_monty, monty_to_json};
use crate::os_access::{OsAccess, OsAccessBuilder, PathAccess};
use crate::prompt::{MONTY_PROMPT, TOOL_DISPATCH_FN, tool_entry};

/// The resource tracker every run is created with. `LimitedTracker` serializes
/// cleanly (so it rides along inside a dumped continuation) and enforces the
/// configured [`ResourceLimits`].
type Tracker = LimitedTracker;

/// A suspended/finished Monty run, parameterised on our tracker.
type Progress = RunProgress<Tracker>;

/// A Python [`CodeRuntime`] for the [`CodeActAgent`](adk_agent::codeact::CodeActAgent),
/// backed by the Monty interpreter.
///
/// Build one with [`MontyRuntime::new`] for sensible defaults, or
/// [`MontyRuntime::builder`] to set resource limits, grant OS access (mounted
/// paths, an environment map, the host clock), or extend the language briefing.
/// Hand the result to
/// [`CodeActAgentBuilder::runtime`](adk_agent::codeact::CodeActAgentBuilder::runtime).
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_codeact_monty::MontyRuntime;
///
/// let runtime = Arc::new(MontyRuntime::new());
/// // CodeActAgent::builder().runtime(runtime)...
/// ```
pub struct MontyRuntime {
    limits: ResourceLimits,
    extra_prompt: Option<String>,
    /// Host-controlled OS-access policy (mounted paths, environment, clock).
    /// Shared into every paused tool call so a resumed run keeps the same
    /// policy. See [`OsAccess`].
    os: Arc<OsAccess>,
}

/// Conservative per-advance resource limits applied by default.
///
/// LLM-generated Python can contain an accidental infinite loop or a runaway
/// allocation. Advancing the interpreter (`start`/`resume`) is synchronous, so
/// an unbounded loop would block the calling task; these caps keep a single
/// advance bounded. They apply *per advance* (time spent in your tools between
/// steps does not count) and restart after deserialization, so a resumed run
/// stays bounded too. Override any of them with the builder, or remove them
/// entirely with [`MontyRuntimeBuilder::unlimited`].
///
/// Defaults: 5s wall-clock per advance, 256 MiB memory. Recursion keeps Monty's
/// own default guard (1000 frames).
fn default_resource_limits() -> ResourceLimits {
    ResourceLimits::new().max_duration(Duration::from_secs(5)).max_memory(256 * 1024 * 1024)
}

impl MontyRuntime {
    /// Create a runtime with conservative default resource limits suitable for
    /// untrusted, LLM-generated code (see [`default_resource_limits`]): a
    /// per-advance time cap, a memory cap, and Monty's recursion guard.
    ///
    /// Relax or tighten these with [`MontyRuntime::builder`]; remove them
    /// entirely (trusted scripts only) with
    /// [`MontyRuntimeBuilder::unlimited`].
    #[must_use]
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Start building a runtime with custom limits or prompt additions.
    #[must_use]
    pub fn builder() -> MontyRuntimeBuilder {
        MontyRuntimeBuilder::new()
    }

    /// A fresh tracker for a new run, carrying the configured limits.
    fn tracker(&self) -> Tracker {
        LimitedTracker::new(self.limits.clone())
    }
}

impl Default for MontyRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for [`MontyRuntime`].
pub struct MontyRuntimeBuilder {
    limits: ResourceLimits,
    extra_prompt: Option<String>,
    os: OsAccessBuilder,
}

impl MontyRuntimeBuilder {
    /// Create a builder seeded with the conservative default limits (see
    /// [`default_resource_limits`]) and a fully sandboxed OS policy (no
    /// filesystem access, empty environment, host clock enabled).
    #[must_use]
    pub fn new() -> Self {
        Self { limits: default_resource_limits(), extra_prompt: None, os: OsAccessBuilder::new() }
    }

    /// Replace the full set of resource limits.
    #[must_use]
    pub fn resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Remove all resource caps except Monty's built-in recursion guard.
    ///
    /// Use this only for *trusted* scripts. LLM-generated code should keep the
    /// default time/memory caps so an accidental infinite loop or runaway
    /// allocation cannot block the calling task. Equivalent to
    /// `resource_limits(ResourceLimits::new())`.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_codeact_monty::MontyRuntime;
    ///
    /// let runtime = MontyRuntime::builder().unlimited().build();
    /// # let _ = runtime;
    /// ```
    #[must_use]
    pub fn unlimited(mut self) -> Self {
        self.limits = ResourceLimits::new();
        self
    }

    /// Cap wall-clock execution time for each interpreter step.
    ///
    /// The limit applies per `start`/`resume` advance (time spent in your tools
    /// between steps does not count), and restarts after deserialization.
    #[must_use]
    pub fn max_duration(mut self, duration: Duration) -> Self {
        self.limits.max_duration = Some(duration);
        self
    }

    /// Cap approximate heap memory (bytes) a script may use.
    #[must_use]
    pub fn max_memory(mut self, bytes: usize) -> Self {
        self.limits.max_memory = Some(bytes);
        self
    }

    /// Cap the number of heap allocations a script may make.
    #[must_use]
    pub fn max_allocations(mut self, allocations: usize) -> Self {
        self.limits.max_allocations = Some(allocations);
        self
    }

    /// Append extra text to the language briefing in the system prompt (e.g.
    /// domain conventions, additional usage rules).
    #[must_use]
    pub fn additional_prompt(mut self, text: impl Into<String>) -> Self {
        self.extra_prompt = Some(text.into());
        self
    }

    /// Replace the whole OS-access policy.
    ///
    /// Use this when you have built an [`OsAccess`] separately; otherwise reach
    /// for the per-aspect shortcuts [`Self::allow_path`], [`Self::environ`],
    /// [`Self::environ_var`], and [`Self::system_clock`].
    #[must_use]
    pub fn os_access(mut self, access: OsAccess) -> Self {
        self.os = access.into_builder();
        self
    }

    /// Make a host directory available to scripts at `virtual_path`, read-only
    /// or read-write.
    ///
    /// Scripts reach it through `pathlib.Path` against `virtual_path` (e.g.
    /// `/data`); Monty enforces the mount boundary so a script can never escape
    /// it. By default no paths are accessible. See
    /// [`OsAccessBuilder::allow_path`].
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_codeact_monty::{MontyRuntime, PathAccess};
    ///
    /// let runtime = MontyRuntime::builder()
    ///     .allow_path("/data", "/srv/agent/data", PathAccess::ReadOnly)
    ///     .build();
    /// # let _ = runtime;
    /// ```
    #[must_use]
    pub fn allow_path(
        mut self,
        virtual_path: impl Into<String>,
        host_path: impl Into<std::path::PathBuf>,
        access: PathAccess,
    ) -> Self {
        self.os = self.os.allow_path(virtual_path, host_path, access);
        self
    }

    /// Replace the environment map exposed to scripts via `os.getenv` /
    /// `os.environ`. Empty by default — the host process environment is never
    /// exposed implicitly.
    #[must_use]
    pub fn environ<K, V>(mut self, vars: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.os = self.os.environ(vars);
        self
    }

    /// Add or overwrite a single environment variable visible to scripts.
    #[must_use]
    pub fn environ_var(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.os = self.os.environ_var(key, value);
        self
    }

    /// Enable or disable host-clock access (`date.today()` / `datetime.now()`).
    /// Enabled by default.
    #[must_use]
    pub fn system_clock(mut self, enabled: bool) -> Self {
        self.os = self.os.system_clock(enabled);
        self
    }

    /// Finish building the runtime.
    #[must_use]
    pub fn build(self) -> MontyRuntime {
        MontyRuntime {
            limits: self.limits,
            extra_prompt: self.extra_prompt,
            os: Arc::new(self.os.build()),
        }
    }
}

impl Default for MontyRuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeRuntime for MontyRuntime {
    fn start(&self, script: &str, script_name: &str) -> Result<RunStep, RuntimeError> {
        // A parse/compile failure is the model's mistake: surface it as a
        // `RunStep::Raised` (fed back to the model), never a host `RuntimeError`.
        let run = match MontyRun::new(script.to_string(), script_name, Vec::new()) {
            Ok(run) => run,
            Err(exc) => return Ok(RunStep::raised(render_exception(&exc))),
        };
        let mut stdout = String::new();
        match run.start(Vec::new(), self.tracker(), PrintWriter::CollectString(&mut stdout)) {
            Ok(progress) => drive(progress, stdout, &self.os),
            // An exception raised during the first stretch of execution is a
            // script error: surface it as a Raised traceback, not a host error.
            Err(exc) => Ok(RunStep::raised(render_exception(&exc)).with_stdout(stdout)),
        }
    }

    fn resume(&self, snapshot: &[u8], with: ResumeWith) -> Result<RunStep, RuntimeError> {
        let progress =
            Progress::load(snapshot).map_err(|err| RuntimeError::Snapshot(err.to_string()))?;
        let call = progress.into_function_call().ok_or_else(|| {
            RuntimeError::Snapshot("snapshot is not a paused external function call".to_string())
        })?;
        resume_call(call, with, &self.os)
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        let mut prompt = MONTY_PROMPT.to_string();
        prompt.push_str("\n\n");
        prompt.push_str(&self.os.prompt_section());
        if let Some(extra) = &self.extra_prompt {
            prompt.push_str("\n\n");
            prompt.push_str(extra);
        }
        // Monty can serialize a paused continuation, so HITL confirmation and
        // long-running tool deferral are both supported.
        RuntimeCapabilities::new(true, prompt)
    }

    fn render_tools(&self, tools: &[Arc<dyn Tool>]) -> String {
        // Pure rendering: no state is kept. Argument binding is the driver's job.
        let mut entries = String::new();
        for tool in tools {
            // Built-in (server-side) tools cannot be called from a script.
            if tool.is_builtin() {
                continue;
            }
            entries.push_str(&tool_entry(tool.as_ref()));
        }
        if entries.trim().is_empty() {
            return String::new();
        }
        // Every tool is invoked the same way: there is no bare-callable form.
        format!(
            "The following tools are available. Invoke each one with the built-in \
             `{TOOL_DISPATCH_FN}` function — the first argument is the tool name and \
             the rest are passed by keyword; a tool is never callable as a bare name. \
             Each returns a JSON-compatible value.\n\n```python\n{entries}```"
        )
    }
}

/// Run the interpreter forward to the next *agent-relevant* stop, carrying any
/// captured `stdout` along.
///
/// Tool calls and completion are returned to the agent; OS calls, name lookups,
/// and blocked futures are resolved in-place (see the module docs) so the loop
/// continues until a tool call or completion is reached. A Python exception that
/// propagates out becomes [`RunStep::Raised`].
///
/// OS calls (filesystem, environment, clock) are serviced in-place against the
/// [`OsAccess`] policy and resumed immediately — they are never tools and never
/// pause the agent loop. A fresh [`MountTable`](monty::fs::MountTable) is built
/// once per drive so concurrent runs of the same runtime never share mount
/// state.
fn drive(
    mut progress: Progress,
    mut stdout: String,
    os: &Arc<OsAccess>,
) -> Result<RunStep, RuntimeError> {
    let mut mounts = os.build_mount_table()?;
    loop {
        match progress {
            RunProgress::Complete(value) => {
                return Ok(RunStep::complete(monty_to_json(&value)).with_stdout(stdout));
            }
            RunProgress::FunctionCall(call) => match resolve_dispatch(&call) {
                Ok((name, keyword)) => {
                    let pending = MontyPendingCall::from_call(call, name, keyword, os.clone());
                    return Ok(RunStep::call(Box::new(pending)).with_stdout(stdout));
                }
                // Not a well-formed `call_tool(...)` dispatch: raise a corrective
                // error into the script so the model can fix its call. There is
                // exactly one way to call a tool — no lenient bare-name fallback.
                Err(message) => {
                    progress = match call.resume(
                        ExtFunctionResult::Error(monty_error(&message)),
                        PrintWriter::CollectString(&mut stdout),
                    ) {
                        Ok(next) => next,
                        Err(exc) => {
                            return Ok(RunStep::raised(render_exception(&exc)).with_stdout(stdout));
                        }
                    };
                }
            },
            RunProgress::OsCall(call) => {
                // Resolve filesystem/env/clock access in-place against the
                // host policy and resume immediately — never surfaced as a tool.
                let result = os.resolve(&call.function_call, &mut mounts);
                progress = match call.resume(result, PrintWriter::CollectString(&mut stdout)) {
                    Ok(next) => next,
                    Err(exc) => {
                        return Ok(RunStep::raised(render_exception(&exc)).with_stdout(stdout));
                    }
                };
            }
            RunProgress::NameLookup(lookup) => {
                // The runtime exposes tools only as *called* functions; a bare
                // reference to an unknown name is a genuine NameError.
                progress = match lookup
                    .resume(NameLookupResult::Undefined, PrintWriter::CollectString(&mut stdout))
                {
                    Ok(next) => next,
                    Err(exc) => {
                        return Ok(RunStep::raised(render_exception(&exc)).with_stdout(stdout));
                    }
                };
            }
            RunProgress::ResolveFutures(futures) => {
                let denied: Vec<(u32, ExtFunctionResult)> = futures
                    .pending_call_ids()
                    .iter()
                    .map(|id| {
                        (
                            *id,
                            ExtFunctionResult::Error(monty_error(
                                "asynchronous external calls are not supported; call tools synchronously, without `await`",
                            )),
                        )
                    })
                    .collect();
                progress = match futures.resume(denied, PrintWriter::CollectString(&mut stdout)) {
                    Ok(next) => next,
                    Err(exc) => {
                        return Ok(RunStep::raised(render_exception(&exc)).with_stdout(stdout));
                    }
                };
            }
        }
    }
}

/// A paused tool call: the agent inspects it, runs the tool, then resumes (or
/// dumps it to session state to resume on a later invocation).
struct MontyPendingCall {
    name: String,
    keyword: Vec<(String, Value)>,
    call_id: u64,
    /// Always the [`RunProgress::FunctionCall`] variant for this call.
    progress: Progress,
    /// OS-access policy to apply when the resumed run hits an OS call. Carried
    /// here so an in-process resume keeps the same policy without a runtime
    /// reference.
    os: Arc<OsAccess>,
}

impl MontyPendingCall {
    /// Build a pending call from a validated `call_tool(...)` dispatch: the real
    /// tool `name` and the `keyword` arguments (the entries of the call's
    /// arguments dict), both already extracted by [`resolve_dispatch`].
    ///
    /// Monty always passes arguments as a single named dict, so the seam's
    /// positional slice is always empty and the driver binds the keyword entries
    /// onto the tool's parameters by name — exactly, with no positional
    /// inference.
    fn from_call(
        call: FunctionCall<Tracker>,
        name: String,
        keyword: Vec<(String, Value)>,
        os: Arc<OsAccess>,
    ) -> Self {
        let call_id = u64::from(call.call_id);
        Self { name, keyword, call_id, progress: RunProgress::FunctionCall(call), os }
    }
}

/// Resolve a Monty function call against the single tool-calling convention:
/// `call_tool("<tool-name>", {"arg": value, ...})`.
///
/// There is exactly one way to call a tool, so anything else is the model's
/// mistake and is reported as an `Err(message)` describing the correct form
/// (which the caller raises back into the script). Rejected, with no silent
/// coercion: a bare call to some other name; a missing/non-string tool name;
/// keyword arguments to `call_tool` itself; more than the name and one dict; a
/// non-dict arguments value; or an arguments dict with a non-string key.
///
/// On success returns the real tool name and the dict's entries as exact
/// name→value pairs.
fn resolve_dispatch(
    call: &FunctionCall<Tracker>,
) -> Result<(String, Vec<(String, Value)>), String> {
    if call.function_name != TOOL_DISPATCH_FN {
        return Err(format!(
            "'{}' is not defined. Call tools only via {TOOL_DISPATCH_FN}(\"<tool-name>\", {{...}}).",
            call.function_name
        ));
    }
    let Some(MontyObject::String(name)) = call.args.first() else {
        return Err(format!(
            "{TOOL_DISPATCH_FN}(...) needs the tool name as the first positional string argument, \
             e.g. {TOOL_DISPATCH_FN}(\"my_tool\", {{\"arg\": value}})."
        ));
    };
    let name = name.clone();

    if !call.kwargs.is_empty() {
        return Err(format!(
            "{TOOL_DISPATCH_FN}(...) takes the tool name and a single arguments dict; put tool \
             arguments inside the dict, not as keyword arguments: \
             {TOOL_DISPATCH_FN}(\"{name}\", {{\"arg\": value}})."
        ));
    }
    if call.args.len() > 2 {
        return Err(format!(
            "{TOOL_DISPATCH_FN}(...) takes exactly the tool name and one arguments dict: \
             {TOOL_DISPATCH_FN}(\"{name}\", {{\"arg\": value}})."
        ));
    }

    let keyword = match call.args.get(1) {
        None => Vec::new(),
        Some(MontyObject::Dict(pairs)) => {
            let mut keyword = Vec::with_capacity(pairs.len());
            for (key, value) in pairs {
                let MontyObject::String(key) = key else {
                    return Err(format!(
                        "{TOOL_DISPATCH_FN}(\"{name}\", ...) argument keys must be strings; \
                         pass arguments as {{\"arg\": value}}."
                    ));
                };
                keyword.push((key.clone(), monty_to_json(value)));
            }
            keyword
        }
        Some(_) => {
            return Err(format!(
                "{TOOL_DISPATCH_FN}(\"{name}\", ...) needs a single arguments dict, \
                 e.g. {TOOL_DISPATCH_FN}(\"{name}\", {{\"arg\": value}})."
            ));
        }
    };

    Ok((name, keyword))
}

impl PendingCall for MontyPendingCall {
    fn function_name(&self) -> &str {
        &self.name
    }

    fn positional_args(&self) -> &[Value] {
        // Monty always passes arguments as a named dict; there are no positionals.
        &[]
    }

    fn keyword_args(&self) -> &[(String, Value)] {
        &self.keyword
    }

    fn call_id(&self) -> u64 {
        self.call_id
    }

    fn dump(&self) -> Result<Vec<u8>, RuntimeError> {
        self.progress.dump().map_err(|err| RuntimeError::Snapshot(err.to_string()))
    }

    fn resume(self: Box<Self>, with: ResumeWith) -> Result<RunStep, RuntimeError> {
        let os = self.os.clone();
        let call = self
            .progress
            .into_function_call()
            .expect("MontyPendingCall always wraps a function call");
        resume_call(call, with, &os)
    }
}

/// Feed a tool result (or a raised error) back into a paused [`FunctionCall`]
/// and drive the interpreter onward, capturing any `print` output.
fn resume_call(
    call: FunctionCall<Tracker>,
    with: ResumeWith,
    os: &Arc<OsAccess>,
) -> Result<RunStep, RuntimeError> {
    let result = match with {
        ResumeWith::Value(value) => ExtFunctionResult::Return(json_to_monty(value)),
        // Raise the framework's error message into the script as an exception the
        // model's code can `try`/`except`, exactly like a real tool failure.
        ResumeWith::Raise(message) => ExtFunctionResult::Error(monty_error(&message)),
    };
    let mut stdout = String::new();
    match call.resume(result, PrintWriter::CollectString(&mut stdout)) {
        Ok(progress) => drive(progress, stdout, os),
        Err(exc) => Ok(RunStep::raised(render_exception(&exc)).with_stdout(stdout)),
    }
}

/// Build a Monty exception carrying `message`, raised into a script at a call
/// site. Uses `RuntimeError` as the Python type — the message is what matters to
/// the model, and the framework's error strings are already self-describing.
fn monty_error(message: &str) -> MontyException {
    MontyException::new(ExcType::RuntimeError, Some(message.to_string()))
}

/// Render a Monty exception for the model: a CPython-style traceback plus the
/// `Type: message` line. Fed back verbatim as the opaque error string.
fn render_exception(exc: &MontyException) -> String {
    exc.to_string()
}
