//! The step-wise interpreter seam — language-agnostic.
//!
//! The CodeAct driver does not depend on any concrete interpreter or language.
//! It drives a [`CodeRuntime`]: start a script, and on each external-function
//! call decide whether to resume with a value, resume by raising an error, or
//! suspend (serialize the continuation and stop).
//!
//! # Errors: script vs. host
//!
//! Two error channels, kept deliberately distinct:
//!
//! - **Script errors** — anything the *model* should see and react to (a syntax
//!   error, an uncaught exception, a resource-limit cancellation) are
//!   [`RunStep::Raised`]: an opaque string the runtime renders however its
//!   language expects (a Python traceback, a JS stack, a shell error). The
//!   framework never inspects them; they are fed back to the model verbatim and
//!   the run continues. **Returning `Ok(RunStep::Raised(..))` is always the right
//!   move for a model mistake — including a parse/compile failure.**
//! - **Host failures** — genuine interpreter/host breakage that should abort the
//!   run (snapshot (de)serialization failure, an internal interpreter error) are
//!   [`RuntimeError`].
//!
//! When in doubt: if the model could fix it by writing different code, it is a
//! [`RunStep::Raised`], not a [`RuntimeError`].
//!
//! # Async note
//!
//! Advancing the interpreter (`resume`) is synchronous and fast. Tool execution
//! (which produces the value passed to `resume`) is async and happens in the
//! driver *between* steps, so this trait stays synchronous.
//!
//! # Sequential by design
//!
//! The seam is a *single continuation*: [`RunStep::Call`] surfaces exactly one
//! pending call, and [`PendingCall::resume`] consumes it and yields the next
//! single step. Tool execution is therefore strictly sequential, even if the
//! script's language supports `async`/threads — a concurrency-capable runtime
//! must serialize script-level parallelism at the call boundary.
//!
//! This is deliberate: durability rests on snapshotting *one* continuation at
//! *one* call boundary (see [`PendingCall::dump`]). Multiple in-flight calls
//! would force a checkpoint to capture partial completion (e.g. a long-running
//! tool inside an `asyncio.gather` alongside two finished tools), which has no
//! clean suspend/resume semantics. Concurrent host dispatch would require a
//! different seam (e.g. a multi-call step) and is intentionally out of scope.

use std::sync::Arc;

use adk_core::Tool;
use serde_json::{Map, Value};
use thiserror::Error;

/// A host-level failure of the runtime itself (not a script-level error).
///
/// Script errors — including syntax/parse failures — are modelled as
/// [`RunStep::Raised`] (an opaque string fed back to the model). This type is
/// reserved for genuine interpreter/host failures that should abort the run:
/// snapshot (de)serialization failure or an internal interpreter error.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// A snapshot could not be serialized or restored.
    #[error("snapshot error: {0}")]
    Snapshot(String),
    /// The interpreter failed for an internal reason.
    #[error("interpreter error: {0}")]
    Internal(String),
}

/// How to resume a paused external-function call.
#[derive(Debug, Clone)]
pub enum ResumeWith {
    /// Return this value from the external function back into the script.
    Value(Value),
    /// Raise an error inside the script at the call site, with this message.
    ///
    /// The runtime represents it in its own language (an exception, a thrown
    /// value, etc.). If the script does not catch it, it propagates and the
    /// runtime surfaces it as [`RunStep::Raised`].
    Raise(String),
}

/// The result of advancing the interpreter to its next host-relevant stop.
///
/// Every variant carries the `stdout` (e.g. `print`) the script produced *since
/// the previous step*. Runtimes that capture output attach it (see
/// [`RunStep::with_stdout`]); those that don't leave it empty. The driver
/// surfaces captured output back to the model so it can see what its code
/// printed. Construct values with [`RunStep::call`], [`RunStep::complete`], and
/// [`RunStep::raised`].
pub enum RunStep {
    /// The script called an external function (a tool). Resume it to continue.
    Call {
        /// The paused external-function call.
        call: Box<dyn PendingCall>,
        /// Output the script produced since the previous step (empty if none or
        /// if the runtime does not capture output).
        stdout: String,
    },
    /// The script ran to completion; carries the returned value (decoded to JSON).
    Complete {
        /// The value the script returned, decoded to JSON.
        value: Value,
        /// Output the script produced since the previous step.
        stdout: String,
    },
    /// The script failed: an error propagated to the top. The message is the
    /// runtime's native error rendering, fed back to the model verbatim. This
    /// also covers resource-limit cancellations and parse/compile failures — the
    /// framework does not care which; the message says so.
    Raised {
        /// The runtime's native error rendering.
        message: String,
        /// Output the script produced before the error.
        stdout: String,
    },
}

impl RunStep {
    /// A tool-call step with no captured output.
    #[must_use]
    pub fn call(call: Box<dyn PendingCall>) -> Self {
        Self::Call { call, stdout: String::new() }
    }

    /// A completion step with no captured output.
    #[must_use]
    pub fn complete(value: Value) -> Self {
        Self::Complete { value, stdout: String::new() }
    }

    /// A script-error step with no captured output.
    #[must_use]
    pub fn raised(message: impl Into<String>) -> Self {
        Self::Raised { message: message.into(), stdout: String::new() }
    }

    /// Attach captured `stdout` to this step (builder-style).
    ///
    /// # Example
    ///
    /// ```
    /// use adk_agent::codeact::RunStep;
    /// use serde_json::json;
    ///
    /// let step = RunStep::complete(json!({"type": "final_result", "value": 1}))
    ///     .with_stdout("hello\n");
    /// ```
    #[must_use]
    pub fn with_stdout(mut self, stdout: impl Into<String>) -> Self {
        let slot = match &mut self {
            Self::Call { stdout, .. } => stdout,
            Self::Complete { stdout, .. } => stdout,
            Self::Raised { stdout, .. } => stdout,
        };
        *slot = stdout.into();
        self
    }
}

impl std::fmt::Debug for RunStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Call { call, stdout } => f
                .debug_struct("Call")
                .field("function_name", &call.function_name())
                .field("call_id", &call.call_id())
                .field("stdout", stdout)
                .finish(),
            Self::Complete { value, stdout } => {
                f.debug_struct("Complete").field("value", value).field("stdout", stdout).finish()
            }
            Self::Raised { message, stdout } => {
                f.debug_struct("Raised").field("message", message).field("stdout", stdout).finish()
            }
        }
    }
}

/// A paused external-function call awaiting a result.
///
/// It can be resumed exactly once (consuming it), or its continuation can be
/// serialized with [`dump`](Self::dump) for suspend-to-store before resuming.
///
/// # Arguments
///
/// A call exposes its arguments the way an interpreter actually produces them —
/// [`positional_args`](Self::positional_args) and
/// [`keyword_args`](Self::keyword_args), separately. The driver binds them onto
/// the tool's parameter names (via [`bind_call_args`]) before invoking the tool,
/// so a runtime never needs the tool's schema to name positional arguments. A
/// runtime whose language has only keyword arguments returns an empty positional
/// slice; one with only positional arguments returns an empty keyword slice.
pub trait PendingCall: Send {
    /// The name of the external function (tool) the script called.
    fn function_name(&self) -> &str;
    /// The positional arguments, in call order, marshalled to JSON.
    fn positional_args(&self) -> &[Value];
    /// The keyword arguments, in call order, marshalled to JSON.
    fn keyword_args(&self) -> &[(String, Value)];
    /// The interpreter-assigned unique id for this call.
    fn call_id(&self) -> u64;
    /// Serialize the suspended continuation to bytes (valid while paused here).
    ///
    /// The returned bytes can later be passed to [`CodeRuntime::resume`].
    fn dump(&self) -> Result<Vec<u8>, RuntimeError>;
    /// Resume execution, consuming this call. Advances to the next [`RunStep`].
    fn resume(self: Box<Self>, with: ResumeWith) -> Result<RunStep, RuntimeError>;
}

/// A step-wise, language-agnostic code interpreter capable of suspend/resume at
/// call boundaries.
pub trait CodeRuntime: Send + Sync {
    /// Parse and begin executing `script`, running until the first external
    /// call, completion, or error.
    ///
    /// `script_name` is used by the runtime for error messages (e.g. a
    /// traceback filename). A parse/compile failure is a *model* mistake and
    /// should be returned as `Ok(`[`RunStep::Raised`]`)`, not as a
    /// [`RuntimeError`].
    fn start(&self, script: &str, script_name: &str) -> Result<RunStep, RuntimeError>;

    /// Restore a suspended continuation (from [`PendingCall::dump`]) and resume
    /// it with the given value or error.
    ///
    /// Used on a later turn to resume after a human confirmation decision or a
    /// long-running tool completion. Interpreter-captured callbacks (e.g. a
    /// stdout sink) are not serialized and must be re-attached by the adapter.
    fn resume(&self, snapshot: &[u8], with: ResumeWith) -> Result<RunStep, RuntimeError>;

    /// Report what this runtime can do and which language/environment it
    /// accepts.
    ///
    /// The default assumes a fully capable runtime with an empty prompt;
    /// concrete adapters override this to describe their real environment.
    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities::default()
    }

    /// Render the tool catalog for the system prompt, in this runtime's language
    /// and conventions.
    ///
    /// How a tool is *named and called* is language-dependent, so the runtime
    /// owns this rendering. This is a **pure function of `tools`** — called once
    /// per invocation purely to produce prompt text. A runtime does not need to
    /// remember anything from it: argument binding is handled centrally by the
    /// driver (see [`bind_call_args`]), so there is no need to stash schemas or
    /// parameter orderings here.
    ///
    /// The default emits a generic, language-neutral listing via
    /// [`default_tool_catalog`]; a language-specific runtime (e.g. a Python
    /// runtime) overrides this to emit idiomatic signatures or stubs the model
    /// should call. Returns an empty string when there are no tools.
    fn render_tools(&self, tools: &[Arc<dyn Tool>]) -> String {
        let catalog = default_tool_catalog(tools);
        if catalog.trim().is_empty() {
            return String::new();
        }
        format!("Available tools:\n{catalog}")
    }
}

/// Bind a call's positional and keyword arguments onto a tool's parameters,
/// producing the single JSON object a [`Tool`] expects.
///
/// This is the *one* place positional arguments are mapped onto names, so no
/// runtime needs the tool's schema at the call boundary. Keyword arguments are
/// inserted by name; positional arguments are matched, in order, against the
/// tool's parameter names (the schema's `required` list first, then any
/// remaining `properties`). A positional argument with no corresponding name
/// (or for a tool with no schema) falls back to `arg0`, `arg1`, ... so nothing
/// is silently dropped. A keyword always wins over a positional for the same
/// name.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_agent::codeact::bind_call_args;
/// # use adk_core::{Tool, ToolContext};
/// # use async_trait::async_trait;
/// use serde_json::{json, Value};
///
/// # struct Add;
/// # #[async_trait]
/// # impl Tool for Add {
/// #     fn name(&self) -> &str { "add" }
/// #     fn description(&self) -> &str { "add" }
/// #     fn parameters_schema(&self) -> Option<Value> {
/// #         Some(json!({"type": "object", "properties": {"a": {}, "b": {}}, "required": ["a", "b"]}))
/// #     }
/// #     async fn execute(&self, _c: Arc<dyn ToolContext>, _a: Value) -> adk_core::Result<Value> { Ok(Value::Null) }
/// # }
/// let tool = Add;
/// let bound = bind_call_args(&tool, &[json!(1), json!(2)], &[]);
/// assert_eq!(bound, json!({"a": 1, "b": 2}));
/// ```
pub fn bind_call_args(tool: &dyn Tool, positional: &[Value], keyword: &[(String, Value)]) -> Value {
    let mut map = Map::new();
    for (name, value) in keyword {
        map.insert(name.clone(), value.clone());
    }
    if !positional.is_empty() {
        let names = ordered_parameter_names(tool);
        for (index, value) in positional.iter().enumerate() {
            let key = names.get(index).cloned().unwrap_or_else(|| format!("arg{index}"));
            map.entry(key).or_insert_with(|| value.clone());
        }
    }
    Value::Object(map)
}

/// The tool's parameter names in positional order: `required` (ordered) first,
/// then any remaining `properties` keys.
fn ordered_parameter_names(tool: &dyn Tool) -> Vec<String> {
    let Some(schema) = tool.parameters_schema() else {
        return Vec::new();
    };
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut ordered = required.clone();
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for key in props.keys() {
            if !ordered.contains(key) {
                ordered.push(key.clone());
            }
        }
    }
    ordered
}

/// A generic, language-neutral tool listing.
///
/// Rendering branches on the kind of tool:
///
/// - **Built-in tools** ([`Tool::is_builtin`]) are executed server-side by the
///   model provider and cannot be invoked from scripted code, so they are
///   omitted entirely.
/// - **Long-running tools** ([`Tool::is_long_running`]) are annotated, since
///   their result arrives out-of-band.
///
/// Each remaining tool is listed as one line with its parameter names and
/// description. Runtimes may build on this for their own
/// [`CodeRuntime::render_tools`].
pub fn default_tool_catalog(tools: &[Arc<dyn Tool>]) -> String {
    let mut out = String::new();
    for tool in tools {
        // Built-in (server-side) tools are not callable from a script.
        if tool.is_builtin() {
            continue;
        }
        let decl = tool.declaration();
        let params = decl
            .get("parameters")
            .and_then(|p| p.get("properties"))
            .and_then(|p| p.as_object())
            .map(|props| props.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let annotation = if tool.is_long_running() { " [long-running]" } else { "" };
        out.push_str(&format!(
            "- {}({}): {}{}\n",
            tool.name(),
            params,
            tool.description(),
            annotation
        ));
    }
    out
}

/// What a [`CodeRuntime`] reports about itself.
///
/// [`prompt`](Self::prompt) is freeform text the runtime supplies describing its
/// language and environment (e.g. "you are writing Python for the Monty
/// interpreter; no `class`/`match`; stdlib limited to ..."); the agent injects
/// it verbatim into the system prompt so the model knows what code it may write.
/// There is no fixed schema on purpose.
#[derive(Debug, Clone, Default)]
pub struct RuntimeCapabilities {
    /// Whether the runtime can serialize a paused continuation and resume it.
    ///
    /// Required for HITL confirmation and long-running tool deferral. When
    /// `false`, the agent warns that those features cannot pause execution.
    pub supports_suspension: bool,
    /// Freeform description of the runtime/language environment for the model.
    pub prompt: String,
}

impl RuntimeCapabilities {
    /// Construct capabilities from a suspension flag and a prompt description.
    pub fn new(supports_suspension: bool, prompt: impl Into<String>) -> Self {
        Self { supports_suspension, prompt: prompt.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_capabilities_do_not_claim_suspension() {
        // Default is conservative: a runtime must opt in to suspension.
        assert!(!RuntimeCapabilities::default().supports_suspension);
        assert!(RuntimeCapabilities::default().prompt.is_empty());
    }

    #[test]
    fn new_carries_prompt_and_flag() {
        let caps = RuntimeCapabilities::new(true, "Monty: no class/match");
        assert!(caps.supports_suspension);
        assert_eq!(caps.prompt, "Monty: no class/match");
    }

    #[test]
    fn default_catalog_lists_function_tools() {
        let catalog = default_tool_catalog(&[crate::codeact::test_support::echo_tool()]);
        assert!(catalog.contains("echo"));
        assert!(catalog.contains("echoes its arguments"));
    }

    #[test]
    fn default_catalog_omits_builtin_tools() {
        use crate::codeact::test_support::{builtin_tool, echo_tool};
        let catalog = default_tool_catalog(&[echo_tool(), builtin_tool()]);
        assert!(catalog.contains("echo"));
        assert!(!catalog.contains("web_search"));
    }

    #[test]
    fn default_catalog_annotates_long_running() {
        let catalog = default_tool_catalog(&[crate::codeact::test_support::long_running_tool()]);
        assert!(catalog.contains("slow"));
        assert!(catalog.contains("[long-running]"));
    }

    #[test]
    fn render_tools_empty_when_only_builtin_or_none() {
        use crate::codeact::test_support::{ScriptedRuntime, builtin_tool, echo_tool};
        let rt = ScriptedRuntime::new(vec![]);
        assert!(rt.render_tools(&[]).is_empty());
        // A roster of only server-side tools yields no callable catalog.
        assert!(rt.render_tools(&[builtin_tool()]).is_empty());
        assert!(rt.render_tools(&[echo_tool()]).starts_with("Available tools:"));
    }

    #[test]
    fn bind_call_args_maps_positional_and_prefers_keyword() {
        use crate::codeact::test_support::echo_tool;
        use serde_json::json;
        // echo_tool's schema has no parameters, so positional args fall back to
        // arg0/arg1; keyword args bind by name.
        let tool = echo_tool();
        let bound =
            bind_call_args(tool.as_ref(), &[json!(1)], &[("label".to_string(), json!("x"))]);
        assert_eq!(bound, json!({"arg0": 1, "label": "x"}));
    }

    #[test]
    fn run_step_constructors_and_with_stdout() {
        use serde_json::json;
        let step = RunStep::complete(json!(1)).with_stdout("hi");
        match step {
            RunStep::Complete { value, stdout } => {
                assert_eq!(value, json!(1));
                assert_eq!(stdout, "hi");
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }
}
