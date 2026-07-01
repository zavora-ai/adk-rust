//! Drives the Monty runtime directly through the `CodeRuntime` seam, exercising
//! a multi-tool script with suspend/resume — independent of the agent loop.

use std::time::Duration;

use adk_agent::codeact::{CodeRuntime, ResumeWith, RunStep};
use adk_codeact_monty::{MontyRuntime, PathAccess};
use serde_json::{Value, json};

/// Collapse a call's keyword arguments into a JSON object for assertions.
fn kwargs_object(kwargs: &[(String, Value)]) -> Value {
    Value::Object(kwargs.iter().cloned().collect())
}

/// Run `script` to completion, answering each tool call from `answer`.
fn run(rt: &MontyRuntime, script: &str, answer: impl Fn(&str, &Value) -> Value) -> RunStep {
    let mut step = rt.start(script, "test").expect("start");
    loop {
        match step {
            RunStep::Call { call, .. } => {
                let args = kwargs_object(call.keyword_args());
                let value = answer(call.function_name(), &args);
                step = call.resume(ResumeWith::Value(value)).expect("resume");
            }
            other => return other,
        }
    }
}

#[test]
fn runs_real_python_with_two_tool_calls() {
    let rt = MontyRuntime::new();
    let script = "\
cart = call_tool(\"fetch_cart\", {\"user_id\": \"u-42\"})
subtotal = 0.0
for item in cart[\"items\"]:
    subtotal = subtotal + item[\"price\"] * item[\"qty\"]
rate = call_tool(\"tax_rate\", {\"region\": \"CA\"})
total = subtotal * (1 + rate)
{\"type\": \"final_result\", \"value\": {\"subtotal\": subtotal, \"total\": total}}
";

    let step = run(&rt, script, |name, args| match name {
        "fetch_cart" => {
            assert_eq!(args, &json!({"user_id": "u-42"}));
            json!({"items": [
                {"name": "kb", "price": 80.0, "qty": 1},
                {"name": "cable", "price": 12.0, "qty": 2},
            ]})
        }
        "tax_rate" => {
            assert_eq!(args, &json!({"region": "CA"}));
            json!(0.1)
        }
        other => panic!("unexpected tool {other}"),
    });

    match step {
        RunStep::Complete { value, .. } => {
            assert_eq!(value["type"], "final_result");
            // subtotal = 80 + 12*2 = 104 ; total = 104 * 1.1 = 114.4
            assert_eq!(value["value"]["subtotal"], json!(104.0));
            let total = value["value"]["total"].as_f64().expect("total is a number");
            assert!((total - 114.4).abs() < 1e-9, "unexpected total {total}");
        }
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn arguments_are_surfaced_as_exact_keyword_pairs() {
    // Arguments arrive as a single named dict, so the runtime reports them as
    // exact name->value keyword pairs (never positionally) and the driver binds
    // them without any positional-order inference.
    let rt = MontyRuntime::new();
    let script = "x = call_tool(\"add\", {\"a\": 1, \"b\": 2})\n{\"type\": \"final_result\", \"value\": x}\n";
    let RunStep::Call { call, .. } = rt.start(script, "test").expect("start") else {
        panic!("expected a tool call");
    };
    assert_eq!(call.function_name(), "add");
    assert!(call.positional_args().is_empty(), "args come as a dict, not positionally");
    assert_eq!(kwargs_object(call.keyword_args()), json!({"a": 1, "b": 2}));
}

#[test]
fn captures_stdout_and_surfaces_it_on_the_step() {
    let rt = MontyRuntime::new();
    let script = "print(\"hello from python\")\n{\"type\": \"final_result\", \"value\": 1}\n";
    match rt.start(script, "test").expect("start") {
        RunStep::Complete { value, stdout } => {
            assert_eq!(value, json!({"type": "final_result", "value": 1}));
            assert!(stdout.contains("hello from python"), "missing stdout: {stdout:?}");
        }
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn dump_and_resume_round_trips_across_a_fresh_runtime() {
    let rt = MontyRuntime::new();
    let script =
        "x = call_tool(\"double\", {\"n\": 21})\n{\"type\": \"final_result\", \"value\": x}\n";

    // Start, reach the tool call, snapshot it, then resume on a *fresh* runtime
    // instance — mimicking a process restart with a checkpoint from session state.
    let RunStep::Call { call, .. } = rt.start(script, "test").expect("start") else {
        panic!("expected a tool call");
    };
    assert_eq!(call.function_name(), "double");
    let snapshot = call.dump().expect("dump");

    let rt2 = MontyRuntime::new();
    let step = rt2.resume(&snapshot, ResumeWith::Value(json!(42))).expect("resume");
    match step {
        RunStep::Complete { value, .. } => {
            assert_eq!(value, json!({"type": "final_result", "value": 42}));
        }
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn tool_error_is_raised_into_the_script_and_catchable() {
    let rt = MontyRuntime::new();
    // The script catches the raised tool error and reports a clean final result.
    let script = "\
try:
    call_tool(\"risky\")
    answer = \"ok\"
except Exception as e:
    answer = \"caught\"
{\"type\": \"final_result\", \"value\": answer}
";
    let mut step = rt.start(script, "test").expect("start");
    if let RunStep::Call { call, .. } = step {
        step = call.resume(ResumeWith::Raise("tool exploded".to_string())).expect("resume");
    } else {
        panic!("expected a tool call");
    }
    match step {
        RunStep::Complete { value, .. } => {
            assert_eq!(value, json!({"type": "final_result", "value": "caught"}));
        }
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn call_tool_dispatches_to_the_named_tool() {
    // Every tool is invoked via `call_tool("name", ...)`; the runtime strips the
    // leading name string so the driver sees the real name and arguments. This
    // also covers names that are not valid Python identifiers ("fetch-cart").
    let rt = MontyRuntime::new();
    let script = "\
cart = call_tool(\"fetch-cart\", {\"user_id\": \"u-7\"})
{\"type\": \"final_result\", \"value\": cart}
";
    let RunStep::Call { call, .. } = rt.start(script, "test").expect("start") else {
        panic!("expected a tool call");
    };
    assert_eq!(call.function_name(), "fetch-cart");
    assert!(call.positional_args().is_empty(), "the tool name must be stripped from positionals");
    assert_eq!(kwargs_object(call.keyword_args()), json!({"user_id": "u-7"}));

    let step = call.resume(ResumeWith::Value(json!({"ok": true}))).expect("resume");
    match step {
        RunStep::Complete { value, .. } => {
            assert_eq!(value, json!({"type": "final_result", "value": {"ok": true}}));
        }
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn a_bare_tool_call_is_rejected_there_is_only_call_tool() {
    // Tools are never in scope as bare callables. Calling one by name (instead
    // of through `call_tool`) is the model's mistake and surfaces as a corrective
    // `RunStep::Raised`, never a silent dispatch.
    let rt = MontyRuntime::new();
    let script = "fetch_cart(user_id=\"u-42\")\n{\"type\": \"final_result\", \"value\": 1}\n";
    let step = rt.start(script, "test").expect("start");
    match step {
        RunStep::Raised { message, .. } => {
            assert!(
                message.contains("call_tool"),
                "expected guidance to use call_tool, got: {message}"
            );
        }
        other => panic!("expected Raised, got {other:?}"),
    }
}

#[test]
fn call_tool_without_a_tool_name_is_rejected() {
    // `call_tool` requires the tool name as a leading string literal; omitting it
    // is a malformed dispatch and comes back as a corrective error.
    let rt = MontyRuntime::new();
    let script = "call_tool()\n{\"type\": \"final_result\", \"value\": 1}\n";
    let step = rt.start(script, "test").expect("start");
    assert!(matches!(step, RunStep::Raised { .. }), "expected Raised, got {step:?}");
}

#[test]
fn call_tool_with_keyword_arguments_is_rejected() {
    // Arguments go inside the dict, never as keyword arguments to `call_tool`
    // itself — there is exactly one form.
    let rt = MontyRuntime::new();
    let script =
        "call_tool(\"fetch_cart\", user_id=\"u-42\")\n{\"type\": \"final_result\", \"value\": 1}\n";
    let step = rt.start(script, "test").expect("start");
    assert!(matches!(step, RunStep::Raised { .. }), "expected Raised, got {step:?}");
}

#[test]
fn call_tool_with_a_non_dict_argument_is_rejected() {
    // The second argument must be a dict; a bare value is a malformed dispatch.
    let rt = MontyRuntime::new();
    let script =
        "call_tool(\"fetch_cart\", \"u-42\")\n{\"type\": \"final_result\", \"value\": 1}\n";
    let step = rt.start(script, "test").expect("start");
    assert!(matches!(step, RunStep::Raised { .. }), "expected Raised, got {step:?}");
}

#[test]
fn call_tool_with_a_non_string_argument_key_is_rejected() {
    // Argument keys must be strings; a non-string key is a hard error, never
    // silently dropped.
    let rt = MontyRuntime::new();
    let script =
        "call_tool(\"fetch_cart\", {1: \"u-42\"})\n{\"type\": \"final_result\", \"value\": 1}\n";
    let step = rt.start(script, "test").expect("start");
    assert!(matches!(step, RunStep::Raised { .. }), "expected Raised, got {step:?}");
}

#[test]
fn syntax_error_is_a_raised_step_not_a_host_error() {
    let rt = MontyRuntime::new();
    // A parse failure must come back as Ok(Raised), never Err(RuntimeError).
    let step = rt.start("this is not valid python !!!", "test").expect("start must not host-error");
    assert!(matches!(step, RunStep::Raised { .. }), "expected Raised, got {step:?}");
}

#[test]
fn a_runaway_loop_is_cancelled_by_the_time_limit_as_a_raised_step() {
    // A tight per-advance time cap turns an accidental long-running loop into a
    // script error the model can react to — not a hang and not a host error.
    // (Monty's subset has no `while`, so a large `range` loop stands in for one.)
    let rt = MontyRuntime::builder().max_duration(Duration::from_millis(20)).build();
    let script = "\
x = 0
for i in range(100000000):
    x = x + 1
{\"type\": \"final_result\", \"value\": x}
";
    let step = rt
        .start(script, "test")
        .expect("resource cancellation is a script error, not a host error");
    match step {
        RunStep::Raised { message, .. } => {
            assert!(
                message.contains("time limit exceeded"),
                "expected a time-limit traceback, got: {message}"
            );
        }
        other => panic!("expected Raised, got {other:?}"),
    }
}

#[test]
fn unmounted_filesystem_read_raises_permission_error() {
    // The default runtime grants no filesystem access, so a read outside every
    // mount raises `PermissionError`, which propagates to a `RunStep::Raised`.
    // The OS call is serviced in-place — it never surfaces as a tool call.
    let rt = MontyRuntime::new();
    let script = "\
from pathlib import Path
data = Path(\"/etc/passwd\").read_text()
{\"type\": \"final_result\", \"value\": data}
";
    let step =
        rt.start(script, "test").expect("a refused OS call is a script error, not a host error");
    match step {
        RunStep::Raised { message, .. } => {
            assert!(
                message.contains("PermissionError"),
                "expected a permission error, got: {message}"
            );
        }
        other => panic!("expected Raised, got {other:?}"),
    }
}

#[test]
fn empty_environment_returns_getenv_default_without_pausing() {
    // The default environment is empty, so `os.getenv` returns its default
    // (`None`) and the script completes — the OS call is serviced in-place, not
    // turned into a tool call.
    let rt = MontyRuntime::new();
    let script = "\
import os
home = os.getenv(\"HOME\", \"unset\")
{\"type\": \"final_result\", \"value\": home}
";
    let step = run(&rt, script, |_, _| panic!("os.getenv must not become a tool call"));
    match step {
        RunStep::Complete { value, .. } => {
            assert_eq!(value, json!({"type": "final_result", "value": "unset"}));
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[test]
fn granted_environment_is_readable_in_place() {
    // An explicit environment map is exposed via `os.getenv` / `os.environ`.
    let rt = MontyRuntime::builder().environ_var("PROJECT", "acme").build();
    let script = "\
import os
{\"type\": \"final_result\", \"value\": os.getenv(\"PROJECT\")}
";
    let step = run(&rt, script, |_, _| panic!("os.getenv must not become a tool call"));
    match step {
        RunStep::Complete { value, .. } => {
            assert_eq!(value, json!({"type": "final_result", "value": "acme"}));
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[test]
fn granted_read_path_is_readable_in_place() {
    // A mounted directory is reachable through `pathlib.Path` against its
    // virtual path. The filesystem OS call is serviced in-place — it never
    // surfaces as a tool call.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("note.txt"), "hello from host").expect("write fixture");

    let rt = MontyRuntime::builder().allow_path("/data", dir.path(), PathAccess::ReadOnly).build();
    let script = "\
from pathlib import Path
{\"type\": \"final_result\", \"value\": Path(\"/data/note.txt\").read_text()}
";
    let step = run(&rt, script, |_, _| panic!("a filesystem read must not become a tool call"));
    match step {
        RunStep::Complete { value, .. } => {
            assert_eq!(value, json!({"type": "final_result", "value": "hello from host"}));
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[test]
fn read_only_mount_refuses_writes() {
    // A read-only mount lets reads through but raises `PermissionError` on a
    // write attempt.
    let dir = tempfile::tempdir().expect("temp dir");
    let rt = MontyRuntime::builder().allow_path("/data", dir.path(), PathAccess::ReadOnly).build();
    let script = "\
from pathlib import Path
Path(\"/data/new.txt\").write_text(\"nope\")
{\"type\": \"final_result\", \"value\": \"unreachable\"}
";
    let step = rt.start(script, "test").expect("a refused write is a script error");
    match step {
        RunStep::Raised { message, .. } => {
            assert!(
                message.contains("PermissionError") || message.contains("Read-only"),
                "expected a read-only refusal, got: {message}"
            );
        }
        other => panic!("expected Raised, got {other:?}"),
    }
    // The host file was never created.
    assert!(!dir.path().join("new.txt").exists(), "read-only mount must not write");
}
