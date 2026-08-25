//! The segmented drive loop shared by both Monty executors.
//!
//! The interpreter runs on a blocking thread (`spawn_blocking`), but
//! [`HostFunction::call`](super::HostFunction::call) is async. Every Monty
//! pause struct is serializable, so a drive is **segmented**: a blocking
//! segment advances the interpreter until it completes or pauses at a
//! registered host-function call; a pause serializes the in-flight progress
//! and returns [`PausedCall`] to the async caller, which awaits the host
//! function and starts the next blocking segment by loading the bytes and
//! resuming. Only `Send` types (`Vec<u8>`, JSON values, `String`) cross the
//! boundary.
//!
//! All *other* Monty suspension points are resolved in place, inside the
//! segment:
//!
//! - an **OS call** (filesystem, environment, clock) is serviced against the
//!   effective [`OsAccess`] policy and resumed immediately;
//! - a **name lookup** resolves a registered host-function name to a callable
//!   [`MontyObject::Function`], and anything else to a genuine `NameError`;
//! - a call to an **unknown function** raises a corrective, catchable
//!   exception listing the registered names;
//! - blocked **`await`s** on external futures are denied — host functions are
//!   called synchronously from the script's perspective.

use std::borrow::Cow;

use monty::{
    Dump, MontyRepl, MontyRun, ReplProgress, ReplStartError, RunProgress, Session, SessionRef, dump,
};
use monty_types::{
    CompileOptions, ExcType, ExtFunctionResult, MontyException, MontyObject, NameLookupResult,
    PrintWriter, PrintWriterCallback, ResourceTracker,
};
use serde_json::{Map, Value};
use tracing::debug;

use super::convert::{json_to_monty, monty_key_string, monty_to_json};
use super::host_fn::{FunctionRegistry, INPUT_BINDING};
use super::os_access::OsAccess;
use crate::ExecutionError;

/// The resource tracker every drive uses. `ResourceTracker` serializes cleanly
/// (so it rides along inside dumped progress and REPL state) and enforces the
/// configured `ResourceLimits`.
pub(crate) type Tracker = ResourceTracker;

/// The stdout collector every drive segment writes through: a `String` with a
/// hard byte cap taken from `SandboxPolicy::max_stdout_bytes`.
///
/// The buffer sits on the host, *outside* Monty's `ResourceLimits::max_memory`
/// accounting, so an uncapped collector would let a print loop grow host
/// memory unboundedly while sandbox limits stay green. Output beyond the cap
/// is silently discarded (truncation — the run keeps executing and reports
/// `stdout_truncated`), never an error.
#[derive(Debug)]
pub(crate) struct CappedStdout {
    buf: String,
    cap: usize,
    truncated: bool,
}

impl CappedStdout {
    pub(crate) fn new(cap: usize) -> Self {
        Self { buf: String::new(), cap, truncated: false }
    }

    /// The collected output and whether anything was discarded at the cap.
    pub(crate) fn into_parts(self) -> (String, bool) {
        (self.buf, self.truncated)
    }

    /// Append up to the remaining capacity, cutting on a char boundary.
    fn append(&mut self, text: &str) {
        if self.truncated {
            return;
        }
        let remaining = self.cap - self.buf.len();
        if text.len() <= remaining {
            self.buf.push_str(text);
            return;
        }
        let mut end = remaining;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        self.buf.push_str(&text[..end]);
        self.truncated = true;
    }
}

impl PrintWriterCallback for CappedStdout {
    fn stdout_write(&mut self, output: Cow<'_, str>) -> Result<(), MontyException> {
        self.append(&output);
        Ok(())
    }

    fn stdout_push(&mut self, end: char) -> Result<(), MontyException> {
        let mut bytes = [0u8; 4];
        self.append(end.encode_utf8(&mut bytes));
        Ok(())
    }
}

/// A drive segment paused at a registered host-function call.
///
/// Everything crossing the `spawn_blocking` boundary is `Send`: the call's
/// JSON-converted arguments and the postcard-serialized in-flight progress.
#[derive(Debug)]
pub(crate) struct PausedCall {
    /// The registered host-function name.
    pub(crate) name: String,
    /// Positional arguments, JSON-converted.
    pub(crate) args: Vec<Value>,
    /// Keyword arguments, JSON-converted.
    pub(crate) kwargs: Map<String, Value>,
    /// The serialized paused progress (`RunProgress` or `ReplProgress` bytes,
    /// depending on the mode that produced it).
    pub(crate) progress_bytes: Vec<u8>,
}

/// A finished (non-paused) drive: the script completed or raised.
#[derive(Debug)]
pub(crate) struct DriveEnd {
    /// The final expression value, JSON-converted. `None` when the script
    /// raised.
    pub(crate) value: Option<Value>,
    /// The rendered traceback when the script raised.
    pub(crate) error: Option<String>,
    /// Whether the raise was Monty's `TimeoutError` from an exceeded
    /// `ResourceLimits::max_duration`.
    pub(crate) timed_out: bool,
}

impl DriveEnd {
    fn complete(value: &MontyObject) -> Self {
        Self { value: Some(monty_to_json(value)), error: None, timed_out: false }
    }

    /// Classify a propagated exception. A `TimeoutError` is treated as an
    /// exceeded time budget (a script raising `TimeoutError` itself is
    /// indistinguishable and shares the classification).
    fn raised(exc: &MontyException) -> Self {
        Self {
            value: None,
            error: Some(exc.to_string()),
            timed_out: exc.exc_type() == ExcType::TimeoutError,
        }
    }
}

/// Outcome of one blocking one-shot segment.
#[derive(Debug)]
pub(crate) enum RunSegment {
    Finished(DriveEnd),
    Paused(PausedCall),
}

/// Outcome of one blocking REPL segment. Finished segments carry the updated
/// session bytes — Monty preserves the REPL through Python-level exceptions,
/// so a failed snippet still yields a usable session.
#[derive(Debug)]
pub(crate) enum ReplSegment {
    Finished { end: DriveEnd, repl_bytes: Vec<u8> },
    Paused(PausedCall),
}

fn internal(context: &str, err: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::InternalError(format!("{context}: {err}"))
}

/// Build the exception raised into a script for host-side failures (unknown
/// function, host function error/timeout, denied `await`). `RuntimeError` is
/// used as the Python type — the message is what matters to the model.
fn runtime_error(message: &str) -> MontyException {
    MontyException::new(ExcType::RuntimeError, Some(message.to_string()))
}

/// Convert an awaited host-function outcome into the interpreter resume value.
fn host_result(outcome: Result<Value, String>) -> ExtFunctionResult {
    match outcome {
        Ok(value) => ExtFunctionResult::Return(json_to_monty(value)),
        // The script can `try`/`except` this, exactly like a real tool failure.
        Err(message) => ExtFunctionResult::Error(runtime_error(&message)),
    }
}

/// Resolve a name lookup: a registered host function becomes a callable
/// `Function` object (Monty caches it in the namespace slot; subsequent calls
/// yield `FunctionCall` directly), anything else is a genuine `NameError`.
fn lookup_result(registry: &FunctionRegistry, name: &str) -> NameLookupResult {
    match registry.get(name) {
        Some(function) => NameLookupResult::Value(MontyObject::Function {
            name: name.to_string(),
            docstring: Some(function.description().to_string()),
        }),
        None => NameLookupResult::Undefined,
    }
}

/// Project a Monty kwargs list to a JSON map (keys are Python identifiers,
/// always strings in practice; anything else degrades to its `repr`).
fn kwargs_to_json(kwargs: &[(MontyObject, MontyObject)]) -> Map<String, Value> {
    let mut map = Map::new();
    for (key, value) in kwargs {
        map.insert(monty_key_string(key), monty_to_json(value));
    }
    map
}

const AWAIT_DENIED: &str = "asynchronous external calls are not supported; call host functions synchronously, \
     without `await`";

/// The optional `input` binding for one call.
fn input_bindings(input: Option<Value>) -> Vec<(String, MontyObject)> {
    input.map(|value| vec![(INPUT_BINDING.to_string(), json_to_monty(value))]).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// One-shot segments
// ---------------------------------------------------------------------------

/// First one-shot segment: parse, start, and drive to the first pause or the
/// end. Parse/compile failures are the script's mistake and surface as a
/// raised traceback (data), never a host `ExecutionError`.
pub(crate) fn start_run(
    code: &str,
    script_name: &str,
    input: Option<Value>,
    tracker: Tracker,
    os: &OsAccess,
    registry: &FunctionRegistry,
    stdout: &mut CappedStdout,
) -> Result<RunSegment, ExecutionError> {
    let (input_names, inputs): (Vec<String>, Vec<MontyObject>) =
        input_bindings(input).into_iter().unzip();
    let run = match MontyRun::new(
        code.to_string(),
        script_name,
        input_names,
        CompileOptions::default(),
    ) {
        Ok(run) => run,
        Err(exc) => return Ok(RunSegment::Finished(DriveEnd::raised(&exc))),
    };
    match run.start(inputs, tracker, PrintWriter::Callback(stdout)) {
        Ok(progress) => drive_run(progress, script_name, os, registry, stdout),
        Err(exc) => Ok(RunSegment::Finished(DriveEnd::raised(&exc))),
    }
}

/// Resume a paused one-shot drive with an awaited host-function outcome.
pub(crate) fn resume_run(
    progress_bytes: &[u8],
    outcome: Result<Value, String>,
    os: &OsAccess,
    registry: &FunctionRegistry,
    stdout: &mut CappedStdout,
) -> Result<RunSegment, ExecutionError> {
    let restored = Dump::load(progress_bytes)
        .map_err(|err| internal("failed to deserialize paused run progress", err))?;
    let script_name = restored.script_name;
    let Session::Running(progress) = restored.state else {
        return Err(ExecutionError::InternalError(
            "paused run dump does not contain run progress".to_string(),
        ));
    };
    let progress = *progress;
    let Some(call) = progress.into_function_call() else {
        return Err(ExecutionError::InternalError(
            "paused run progress is not a function call".to_string(),
        ));
    };
    match call.resume(host_result(outcome), PrintWriter::Callback(stdout)) {
        Ok(progress) => drive_run(progress, &script_name, os, registry, stdout),
        Err(exc) => Ok(RunSegment::Finished(DriveEnd::raised(&exc))),
    }
}

/// Drive a one-shot run to the next segment boundary (pause) or the end.
fn drive_run(
    mut progress: RunProgress,
    script_name: &str,
    os: &OsAccess,
    registry: &FunctionRegistry,
    stdout: &mut CappedStdout,
) -> Result<RunSegment, ExecutionError> {
    let mut mounts = os.build_mount_table()?;
    loop {
        match progress {
            RunProgress::Complete(value) => {
                return Ok(RunSegment::Finished(DriveEnd::complete(&value)));
            }
            RunProgress::FunctionCall(call) => {
                if !call.method_call && registry.contains(&call.function_name) {
                    let name = call.function_name.clone();
                    let args = call.args.iter().map(monty_to_json).collect();
                    let kwargs = kwargs_to_json(&call.kwargs);
                    let paused = RunProgress::FunctionCall(call);
                    let progress_bytes = dump(script_name, None, SessionRef::Running(&paused))
                        .map_err(|err| internal("failed to serialize paused run progress", err))?;
                    debug!(
                        host_fn.name = %name,
                        progress.bytes = progress_bytes.len(),
                        "pausing run at host function call"
                    );
                    return Ok(RunSegment::Paused(PausedCall {
                        name,
                        args,
                        kwargs,
                        progress_bytes,
                    }));
                }
                let message = registry.call_failure_message(&call.function_name, call.method_call);
                progress = match call.resume(
                    ExtFunctionResult::Error(runtime_error(&message)),
                    PrintWriter::Callback(stdout),
                ) {
                    Ok(next) => next,
                    Err(exc) => return Ok(RunSegment::Finished(DriveEnd::raised(&exc))),
                };
            }
            RunProgress::OsCall(call) => {
                progress = match call.resume_with(PrintWriter::Callback(stdout), |call| {
                    os.resolve(call, &mut mounts)
                }) {
                    Ok(next) => next,
                    Err(exc) => return Ok(RunSegment::Finished(DriveEnd::raised(&exc))),
                };
            }
            RunProgress::NameLookup(lookup) => {
                let result = lookup_result(registry, &lookup.name);
                progress = match lookup.resume(result, PrintWriter::Callback(stdout)) {
                    Ok(next) => next,
                    Err(exc) => return Ok(RunSegment::Finished(DriveEnd::raised(&exc))),
                };
            }
            RunProgress::ResolveFutures(futures) => {
                let denied: Vec<(u32, ExtFunctionResult)> = futures
                    .pending_call_ids()
                    .iter()
                    .map(|id| (*id, ExtFunctionResult::Error(runtime_error(AWAIT_DENIED))))
                    .collect();
                progress = match futures.resume(denied, PrintWriter::Callback(stdout)) {
                    Ok(next) => next,
                    Err(exc) => return Ok(RunSegment::Finished(DriveEnd::raised(&exc))),
                };
            }
        }
    }
}

// ---------------------------------------------------------------------------
// REPL segments
// ---------------------------------------------------------------------------

/// Serialize a fresh, empty REPL session (for explicit `start()`).
pub(crate) fn fresh_repl_bytes(
    script_name: &str,
    tracker: Tracker,
) -> Result<Vec<u8>, ExecutionError> {
    let repl = MontyRepl::new(script_name, tracker, CompileOptions::default());
    dump(script_name, None, SessionRef::Idle(&repl))
        .map_err(|err| internal("failed to serialize fresh REPL state", err))
}

/// First REPL segment for one call: load the serialized session (or create a
/// fresh one), install the per-call time budget, feed the snippet, and drive
/// to the first pause or the end.
///
/// `set_max_duration` resets the accumulated interpreter time, so each call
/// gets a fresh time budget. Memory accounting rides along inside the
/// serialized tracker, so `ResourceLimits::max_memory` bounds the
/// *cumulative* session heap, not per-call allocation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn feed_repl(
    repl_bytes: Option<&[u8]>,
    script_name: &str,
    tracker: Tracker,
    timeout: std::time::Duration,
    code: &str,
    input: Option<Value>,
    os: &OsAccess,
    registry: &FunctionRegistry,
    stdout: &mut CappedStdout,
) -> Result<ReplSegment, ExecutionError> {
    let (mut repl, active_script_name) = match repl_bytes {
        Some(bytes) => {
            let restored = Dump::load(bytes)
                .map_err(|err| internal("failed to deserialize REPL session state", err))?;
            let Session::Idle(repl) = restored.state else {
                return Err(ExecutionError::InternalError(
                    "REPL session dump is not idle".to_string(),
                ));
            };
            (*repl, restored.script_name)
        }
        None => (
            MontyRepl::new(script_name, tracker, CompileOptions::default()),
            script_name.to_string(),
        ),
    };
    repl.tracker_mut().set_max_duration(timeout);
    match repl.feed_start(code, input_bindings(input), PrintWriter::Callback(stdout)) {
        Ok(progress) => drive_repl(progress, &active_script_name, os, registry, stdout),
        Err(err) => repl_raised(*err, &active_script_name),
    }
}

/// Resume a paused REPL drive with an awaited host-function outcome.
pub(crate) fn resume_repl(
    progress_bytes: &[u8],
    outcome: Result<Value, String>,
    os: &OsAccess,
    registry: &FunctionRegistry,
    stdout: &mut CappedStdout,
) -> Result<ReplSegment, ExecutionError> {
    let restored = Dump::load(progress_bytes)
        .map_err(|err| internal("failed to deserialize paused REPL progress", err))?;
    let script_name = restored.script_name;
    let Session::Suspended(progress) = restored.state else {
        return Err(ExecutionError::InternalError(
            "paused REPL dump does not contain suspended progress".to_string(),
        ));
    };
    let progress = *progress;
    let Some(call) = progress.into_function_call() else {
        return Err(ExecutionError::InternalError(
            "paused REPL progress is not a function call".to_string(),
        ));
    };
    match call.resume(host_result(outcome), PrintWriter::Callback(stdout)) {
        Ok(progress) => drive_repl(progress, &script_name, os, registry, stdout),
        Err(err) => repl_raised(*err, &script_name),
    }
}

/// A Python-level raise preserves the REPL session (`ReplStartError` returns
/// it), so serialize the surviving state alongside the rendered traceback.
fn repl_raised(err: ReplStartError, script_name: &str) -> Result<ReplSegment, ExecutionError> {
    let repl_bytes = dump(script_name, None, SessionRef::Idle(&err.repl))
        .map_err(|err| internal("failed to serialize REPL session state", err))?;
    Ok(ReplSegment::Finished { end: DriveEnd::raised(&err.error), repl_bytes })
}

/// Drive a REPL feed to the next segment boundary (pause) or the end.
fn drive_repl(
    mut progress: ReplProgress,
    script_name: &str,
    os: &OsAccess,
    registry: &FunctionRegistry,
    stdout: &mut CappedStdout,
) -> Result<ReplSegment, ExecutionError> {
    let mut mounts = os.build_mount_table()?;
    loop {
        match progress {
            ReplProgress::Complete { repl, value } => {
                let repl_bytes = dump(script_name, None, SessionRef::Idle(&repl))
                    .map_err(|err| internal("failed to serialize REPL session state", err))?;
                debug!(repl.state_bytes = repl_bytes.len(), "repl snippet complete");
                return Ok(ReplSegment::Finished { end: DriveEnd::complete(&value), repl_bytes });
            }
            ReplProgress::FunctionCall(call) => {
                if !call.method_call && registry.contains(&call.function_name) {
                    let name = call.function_name.clone();
                    let args = call.args.iter().map(monty_to_json).collect();
                    let kwargs = kwargs_to_json(&call.kwargs);
                    let paused = ReplProgress::FunctionCall(call);
                    let progress_bytes = dump(script_name, None, SessionRef::Suspended(&paused))
                        .map_err(|err| internal("failed to serialize paused REPL progress", err))?;
                    debug!(
                        host_fn.name = %name,
                        progress.bytes = progress_bytes.len(),
                        "pausing repl at host function call"
                    );
                    return Ok(ReplSegment::Paused(PausedCall {
                        name,
                        args,
                        kwargs,
                        progress_bytes,
                    }));
                }
                let message = registry.call_failure_message(&call.function_name, call.method_call);
                progress = match call.resume(
                    ExtFunctionResult::Error(runtime_error(&message)),
                    PrintWriter::Callback(stdout),
                ) {
                    Ok(next) => next,
                    Err(err) => return repl_raised(*err, script_name),
                };
            }
            ReplProgress::OsCall(call) => {
                progress = match call.resume_with(PrintWriter::Callback(stdout), |call| {
                    os.resolve(call, &mut mounts)
                }) {
                    Ok(next) => next,
                    Err(err) => return repl_raised(*err, script_name),
                };
            }
            ReplProgress::NameLookup(lookup) => {
                let result = lookup_result(registry, &lookup.name);
                progress = match lookup.resume(result, PrintWriter::Callback(stdout)) {
                    Ok(next) => next,
                    Err(err) => return repl_raised(*err, script_name),
                };
            }
            ReplProgress::ResolveFutures(futures) => {
                let denied: Vec<(u32, ExtFunctionResult)> = futures
                    .pending_call_ids()
                    .iter()
                    .map(|id| (*id, ExtFunctionResult::Error(runtime_error(AWAIT_DENIED))))
                    .collect();
                progress = match futures.resume(denied, PrintWriter::Callback(stdout)) {
                    Ok(next) => next,
                    Err(err) => return repl_raised(*err, script_name),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use monty_types::ResourceLimits;
    use serde_json::json;

    use super::super::host_fn::{ClosureHostFunction, HostFunction};
    use super::*;

    fn tracker() -> Tracker {
        Tracker::new(ResourceLimits::default())
    }

    fn registry_with(name: &str) -> FunctionRegistry {
        let function: Arc<dyn HostFunction> =
            Arc::new(ClosureHostFunction::new(name, "test function", |_args, _kwargs| async {
                Ok(json!(null))
            }));
        FunctionRegistry::build(vec![function]).unwrap()
    }

    // -----------------------------------------------------------------------
    // CappedStdout
    // -----------------------------------------------------------------------

    #[test]
    fn capped_stdout_collects_within_the_cap() {
        let mut stdout = CappedStdout::new(16);
        stdout.stdout_write("hello".into()).unwrap();
        stdout.stdout_push('\n').unwrap();
        assert_eq!(stdout.into_parts(), ("hello\n".to_string(), false));
    }

    #[test]
    fn capped_stdout_cuts_on_a_char_boundary_and_discards_the_rest() {
        // "ab€" is 5 bytes (€ is 3); a 4-byte cap must cut before the €.
        let mut stdout = CappedStdout::new(4);
        stdout.stdout_write("ab€".into()).unwrap();
        // Everything after truncation is discarded, including pushes.
        stdout.stdout_write("more".into()).unwrap();
        stdout.stdout_push('x').unwrap();
        assert_eq!(stdout.into_parts(), ("ab".to_string(), true));
    }

    #[test]
    fn capped_stdout_fills_to_exactly_the_cap_without_truncation() {
        let mut stdout = CappedStdout::new(5);
        stdout.stdout_write("ab€".into()).unwrap();
        assert_eq!(stdout.into_parts(), ("ab€".to_string(), false));
    }

    #[test]
    fn capped_stdout_push_beyond_the_cap_truncates() {
        let mut stdout = CappedStdout::new(1);
        stdout.stdout_push('a').unwrap();
        stdout.stdout_push('b').unwrap();
        assert_eq!(stdout.into_parts(), ("a".to_string(), true));
    }

    // -----------------------------------------------------------------------
    // Helper projections
    // -----------------------------------------------------------------------

    #[test]
    fn kwargs_with_a_non_string_key_degrade_to_repr() {
        let kwargs = vec![(MontyObject::Int(1), MontyObject::Bool(true))];
        let map = kwargs_to_json(&kwargs);
        assert_eq!(serde_json::Value::Object(map), json!({"1": true}));
    }

    #[test]
    fn lookup_resolves_registered_names_and_leaves_the_rest_undefined() {
        let registry = registry_with("fetch");
        match lookup_result(&registry, "fetch") {
            NameLookupResult::Value(MontyObject::Function { name, docstring }) => {
                assert_eq!(name, "fetch");
                assert_eq!(docstring.as_deref(), Some("test function"));
            }
            other => panic!("expected a Function value, got {other:?}"),
        }
        assert!(matches!(lookup_result(&registry, "missing"), NameLookupResult::Undefined));
    }

    #[test]
    fn a_timeout_exception_classifies_the_drive_end_as_timed_out() {
        let end = DriveEnd::raised(&MontyException::new(
            ExcType::TimeoutError,
            Some("time budget exceeded".to_string()),
        ));
        assert!(end.timed_out);
        assert!(end.error.unwrap().contains("time budget exceeded"));

        let end = DriveEnd::raised(&MontyException::new(ExcType::ValueError, None));
        assert!(!end.timed_out);
    }

    // -----------------------------------------------------------------------
    // Resume error paths
    // -----------------------------------------------------------------------

    #[test]
    fn corrupted_run_progress_bytes_are_an_internal_error() {
        let registry = FunctionRegistry::default();
        let os = OsAccess::default();
        let mut stdout = CappedStdout::new(1024);
        let err =
            resume_run(b"not postcard", Ok(json!(1)), &os, &registry, &mut stdout).unwrap_err();
        match err {
            ExecutionError::InternalError(msg) => assert!(msg.contains("deserialize")),
            other => panic!("expected InternalError, got {other:?}"),
        }
    }

    #[test]
    fn corrupted_repl_progress_bytes_are_an_internal_error() {
        let registry = FunctionRegistry::default();
        let os = OsAccess::default();
        let mut stdout = CappedStdout::new(1024);
        let err =
            resume_repl(b"not postcard", Ok(json!(1)), &os, &registry, &mut stdout).unwrap_err();
        match err {
            ExecutionError::InternalError(msg) => assert!(msg.contains("deserialize")),
            other => panic!("expected InternalError, got {other:?}"),
        }
    }

    #[test]
    fn resuming_a_non_function_call_run_progress_is_an_internal_error() {
        let registry = FunctionRegistry::default();
        let os = OsAccess::default();
        let mut stdout = CappedStdout::new(1024);
        // A trivial script completes immediately; its dumped progress is a
        // `Complete`, not a paused function call.
        let run = MontyRun::new("1 + 1".to_string(), "test", Vec::new(), CompileOptions::default())
            .unwrap();
        let progress =
            run.start(Vec::new(), tracker(), PrintWriter::Callback(&mut stdout)).unwrap();
        let bytes = dump("test", None, SessionRef::Running(&progress)).unwrap();
        let err = resume_run(&bytes, Ok(json!(1)), &os, &registry, &mut stdout)
            .expect_err("a completed progress must not resume");
        match err {
            ExecutionError::InternalError(msg) => assert!(msg.contains("not a function call")),
            other => panic!("expected InternalError, got {other:?}"),
        }
    }

    #[test]
    fn resuming_a_non_function_call_repl_progress_is_an_internal_error() {
        let registry = FunctionRegistry::default();
        let os = OsAccess::default();
        let mut stdout = CappedStdout::new(1024);
        let repl = MontyRepl::new("test", tracker(), CompileOptions::default());
        let progress =
            repl.feed_start("1 + 1", Vec::new(), PrintWriter::Callback(&mut stdout)).unwrap();
        let bytes = dump("test", None, SessionRef::Suspended(&progress)).unwrap();
        let err = resume_repl(&bytes, Ok(json!(1)), &os, &registry, &mut stdout)
            .expect_err("a completed progress must not resume");
        match err {
            ExecutionError::InternalError(msg) => assert!(msg.contains("not a function call")),
            other => panic!("expected InternalError, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // ResolveFutures denial
    // -----------------------------------------------------------------------

    /// The drive loop always resumes host-function calls with concrete
    /// values, so `ResolveFutures` cannot arise through `execute()` — the
    /// denial arm is defense in depth. Exercise it by steering a run into the
    /// pending-future state manually (`resume_pending`) and handing the
    /// paused progress to `drive_run`.
    #[test]
    fn pending_futures_are_denied_with_the_await_message() {
        let registry = registry_with("fetch");
        let os = OsAccess::default();
        let mut stdout = CappedStdout::new(1024);

        let run = MontyRun::new(
            "await fetch()".to_string(),
            "test",
            Vec::new(),
            CompileOptions::default(),
        )
        .unwrap();
        let mut progress =
            run.start(Vec::new(), tracker(), PrintWriter::Callback(&mut stdout)).unwrap();
        // Resolve the name, then answer the call with a pending future.
        while !matches!(progress, RunProgress::ResolveFutures(_)) {
            progress = match progress {
                RunProgress::NameLookup(lookup) => {
                    let result = lookup_result(&registry, &lookup.name);
                    lookup.resume(result, PrintWriter::Callback(&mut stdout)).unwrap()
                }
                RunProgress::FunctionCall(call) => {
                    call.resume_pending(PrintWriter::Callback(&mut stdout)).unwrap()
                }
                other => panic!("unexpected progress while steering: {other:?}"),
            };
        }

        let segment = drive_run(progress, "test", &os, &registry, &mut stdout).unwrap();
        match segment {
            RunSegment::Finished(end) => {
                let error = end.error.expect("the denied await raises");
                assert!(
                    error.contains("asynchronous external calls are not supported"),
                    "error: {error}"
                );
            }
            RunSegment::Paused(_) => panic!("expected the denied await to finish the run"),
        }
    }
}
