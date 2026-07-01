//! A tiny, self-contained [`CodeRuntime`] for the CodeAct example.
//!
//! Production CodeAct uses a real interpreter (the intended adapter wraps
//! Pydantic's Monty, a Rust-native Python; see the `adk-codeact-monty` crate).
//! To keep this example runnable with no native dependencies, `LineScriptRuntime`
//! interprets a deliberately minimal *line script* language while still
//! exercising the full [`CodeRuntime`] seam, including suspend/resume at a call
//! boundary.
//!
//! # Language
//!
//! One instruction per line; blank lines and `#` comments are ignored:
//!
//! - `CALL <tool> <json-args>` — call a tool; its result becomes `$last`.
//! - `OBSERVE <json>` — return an observation to the model (continues the loop).
//! - `FINAL <json|$last>` — return the final result and end the loop.
//!
//! The "continuation" is just the remaining lines plus the last tool result, so
//! it serializes trivially — which is exactly what makes suspend/resume work.

use adk_agent::codeact::{
    CodeRuntime, PendingCall, ResumeWith, RunStep, RuntimeCapabilities, RuntimeError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The serializable interpreter state: the lines still to run and the most
/// recent tool result (bound to `$last`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Program {
    lines: Vec<String>,
    last: Value,
    next_call_id: u64,
}

impl Program {
    fn parse(script: &str) -> Self {
        let lines = script
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect();
        Self { lines, last: Value::Null, next_call_id: 1 }
    }
}

/// Advance the program by one instruction to its next host-relevant stop.
///
/// Each instruction maps to exactly one [`RunStep`]: a `CALL` yields a pending
/// call (with the remaining program as its continuation), while `OBSERVE`/`FINAL`
/// complete the script. Blank and comment lines were already stripped in
/// [`Program::parse`], so there is nothing to skip here.
///
/// Malformed instructions are the model's mistake, so they surface as
/// [`RunStep::Raised`] (fed back to the model) — never a [`RuntimeError`], which
/// is reserved for genuine host failures.
fn step(mut program: Program) -> Result<RunStep, RuntimeError> {
    if program.lines.is_empty() {
        // No FINAL was reached: report it as a script error the model can react to.
        return Ok(RunStep::raised("script ended without a FINAL result"));
    }
    let line = program.lines.remove(0);
    let (op, rest) = match line.split_once(char::is_whitespace) {
        Some((op, rest)) => (op, rest.trim()),
        None => (line.as_str(), ""),
    };
    match op {
        "CALL" => {
            let (name, args_str) = rest
                .split_once(char::is_whitespace)
                .map(|(n, a)| (n.trim(), a.trim()))
                .unwrap_or((rest, "{}"));
            let args: Value = match serde_json::from_str(args_str) {
                Ok(args) => args,
                Err(e) => return Ok(RunStep::raised(format!("bad CALL args: {e}"))),
            };
            let call_id = program.next_call_id;
            program.next_call_id += 1;
            let (positional, keyword) = split_args(args);
            Ok(RunStep::call(Box::new(LinePendingCall {
                name: name.to_string(),
                positional,
                keyword,
                call_id,
                remaining: program,
            })))
        }
        "OBSERVE" => match resolve(rest, &program.last) {
            Ok(value) => Ok(RunStep::complete(json!({"type": "observation", "value": value}))),
            Err(message) => Ok(RunStep::raised(message)),
        },
        "FINAL" => match resolve(rest, &program.last) {
            Ok(value) => Ok(RunStep::complete(json!({"type": "final_result", "value": value}))),
            Err(message) => Ok(RunStep::raised(message)),
        },
        other => Ok(RunStep::raised(format!("unknown instruction: {other}"))),
    }
}

/// Resolve a literal JSON argument, or the special `$last` token, to a value.
fn resolve(text: &str, last: &Value) -> Result<Value, String> {
    if text == "$last" {
        return Ok(last.clone());
    }
    serde_json::from_str(text).map_err(|e| format!("bad JSON value: {e}"))
}

/// Split JSON args into the positional/keyword shape the seam expects: an object
/// becomes keyword args (the common case here), an array becomes positional.
fn split_args(args: Value) -> (Vec<Value>, Vec<(String, Value)>) {
    match args {
        Value::Object(map) => (Vec::new(), map.into_iter().collect()),
        Value::Array(items) => (items, Vec::new()),
        Value::Null => (Vec::new(), Vec::new()),
        other => (vec![other], Vec::new()),
    }
}

/// A self-contained [`CodeRuntime`] over the line-script language.
pub struct LineScriptRuntime;

impl CodeRuntime for LineScriptRuntime {
    fn start(&self, script: &str, _script_name: &str) -> Result<RunStep, RuntimeError> {
        step(Program::parse(script))
    }

    fn resume(&self, snapshot: &[u8], with: ResumeWith) -> Result<RunStep, RuntimeError> {
        let mut program: Program =
            serde_json::from_slice(snapshot).map_err(|e| RuntimeError::Snapshot(e.to_string()))?;
        match with {
            ResumeWith::Value(value) => {
                program.last = value;
                step(program)
            }
            // A raised error in this toy language simply ends the script; a real
            // runtime would inject it at the call site so the script could catch it.
            ResumeWith::Raise(message) => Ok(RunStep::raised(message)),
        }
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities::new(
            true,
            "You are writing a minimal LINE SCRIPT. One instruction per line:\n\
             - CALL <tool> <json-args>   call a tool; its result is bound to $last\n\
             - OBSERVE <json|$last>      surface info to yourself and continue\n\
             - FINAL <json|$last>        return the final result and stop\n\
             Emit exactly one fenced code block containing the script.",
        )
    }
}

/// A paused tool call: its continuation is just the remaining [`Program`].
struct LinePendingCall {
    name: String,
    positional: Vec<Value>,
    keyword: Vec<(String, Value)>,
    call_id: u64,
    remaining: Program,
}

impl PendingCall for LinePendingCall {
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
        let mut remaining = self.remaining;
        match with {
            ResumeWith::Value(value) => {
                remaining.last = value;
                step(remaining)
            }
            ResumeWith::Raise(message) => Ok(RunStep::raised(message)),
        }
    }
}
