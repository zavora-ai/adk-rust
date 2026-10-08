//! What a model-directed shell command can reach.
//!
//! `BashTool` ran `sh -c` with only `current_dir` set. It did not call `env_clear`, so
//! the command inherited the parent environment — including provider API keys an agent
//! process routinely holds — and a timeout called `start_kill` on the direct child only,
//! so anything `sh` had started kept running after the tool returned.
//!
//! These tests state what the boundary does and does not do. A working directory is not
//! an OS sandbox: the command can still reach absolute paths and the network. What is
//! asserted here is the part that is enforced.

#![cfg(unix)]

use adk_core::{ReadonlyContext, Tool, ToolContext};
use adk_devtools::{DevToolset, Workspace};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

mod common;
use common::TestCtx;

/// Runs `command` through the toolset's bash tool.
async fn run_bash(
    workspace: Workspace,
    command: &str,
    timeout_secs: Option<u64>,
) -> adk_core::Result<Value> {
    let toolset = DevToolset::new(workspace);
    let readonly_ctx: Arc<dyn ReadonlyContext> = Arc::new(TestCtx);
    let tools = adk_core::Toolset::tools(&toolset, readonly_ctx).await.unwrap();
    let bash: &Arc<dyn Tool> =
        tools.iter().find(|tool| tool.name() == "bash").expect("the toolset must expose bash");

    let mut args = json!({ "command": command });
    if let Some(secs) = timeout_secs {
        args["timeout_secs"] = json!(secs);
    }
    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx);
    bash.execute(ctx, args).await
}

fn temp_workspace() -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(dir.path());
    (dir, workspace)
}

// ── The environment is not inherited ──────────────────────────────────

#[tokio::test]
async fn a_command_cannot_read_an_inherited_secret() {
    // SAFETY: single-threaded setup before any command runs; mirrors how an agent
    // process would already hold a provider key in its environment.
    unsafe {
        std::env::set_var("ADK_TEST_PROVIDER_KEY", "super-secret-value");
    }

    let (_dir, workspace) = temp_workspace();
    let result = run_bash(workspace, "env", None).await.expect("env must run");
    let stdout = result["stdout"].as_str().unwrap_or_default();

    assert!(
        !stdout.contains("super-secret-value"),
        "a model-directed command read an inherited credential: {stdout}"
    );
    assert!(
        !stdout.contains("ADK_TEST_PROVIDER_KEY"),
        "the variable name leaked even without its value: {stdout}"
    );

    unsafe {
        std::env::remove_var("ADK_TEST_PROVIDER_KEY");
    }
}

#[tokio::test]
async fn allowlisted_variables_still_reach_the_command() {
    // Clearing everything would break the tools an agent is meant to run.
    let path = std::env::var("PATH").expect("the test environment must provide PATH");
    let (_dir, workspace) = temp_workspace();
    let result = run_bash(workspace, "printf '%s\\n' \"$PATH\"", None).await.expect("must run");

    assert_eq!(result["exit_code"], json!(0));
    assert_eq!(result["stdout"], json!(format!("{path}\n")));
}

#[tokio::test]
async fn inheriting_the_environment_is_available_but_opt_in() {
    unsafe {
        std::env::set_var("ADK_TEST_OPT_IN", "visible");
    }

    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(dir.path()).inherit_env(true);
    let result = run_bash(workspace, "echo \"v=$ADK_TEST_OPT_IN\"", None).await.expect("must run");

    assert!(
        result["stdout"].as_str().unwrap_or_default().contains("v=visible"),
        "opting in must actually pass the environment through"
    );

    unsafe {
        std::env::remove_var("ADK_TEST_OPT_IN");
    }
}

// ── A timeout takes descendants with it ───────────────────────────────

#[tokio::test]
async fn a_timeout_kills_processes_the_command_started() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("grandchild.pid");
    let workspace = Workspace::new(dir.path());

    // `sh` starts a background sleep that records its own pid, then blocks. Killing only
    // the direct child would leave that sleep running.
    let command = format!("(sleep 30 & echo $! > {}) ; sleep 30", marker.to_string_lossy());
    let result = run_bash(workspace, &command, Some(1)).await;
    assert!(result.is_err(), "the command must time out");

    // Give the signal a moment to land.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let pid: i32 = std::fs::read_to_string(&marker)
        .expect("the background process must have recorded its pid")
        .trim()
        .parse()
        .expect("a numeric pid");

    // SAFETY: signal 0 only probes for existence and cannot violate memory safety.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "a descendant (pid {pid}) survived the timeout");
}

#[tokio::test]
async fn a_command_that_finishes_is_unaffected() {
    // Guards against the process-group handling breaking ordinary execution.
    let (_dir, workspace) = temp_workspace();
    let result = run_bash(workspace, "echo hello", None).await.expect("must run");

    assert_eq!(result["exit_code"], json!(0));
    assert!(result["stdout"].as_str().unwrap_or_default().contains("hello"));
}

// ── Output is capped while it is read ─────────────────────────────────
//
// Output was collected in full and cut afterwards with `String::truncate`, which panics
// when the cap falls inside a multi-byte character, and which bounded only the report,
// not the memory spent collecting it.

const TRUNCATION_MARKER: &str = "\n…[truncated]";

#[tokio::test]
async fn a_cap_inside_a_multi_byte_character_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    // `€` is three bytes, so a 4-byte cap falls inside the second one.
    let workspace = Workspace::new(dir.path()).max_output_bytes(4);

    let result = run_bash(workspace, "printf '€€€'", None).await.expect("must run");

    assert_eq!(result["stdout"], json!(format!("€{TRUNCATION_MARKER}")));
    assert_eq!(result["truncated"], json!(true));
    assert_eq!(result["exit_code"], json!(0));
}

#[tokio::test]
async fn more_than_a_mebibyte_of_multi_byte_output_is_capped() {
    let (_dir, workspace) = temp_workspace();
    let cap = workspace.max_output();
    // Each line is 61 bytes, so the 1 MiB cap lands inside a character.
    let command = "yes '€€€€€€€€€€€€€€€€€€€€' | head -c 1500000";

    let result = run_bash(workspace, command, None).await.expect("must run");
    let stdout = result["stdout"].as_str().expect("stdout must be a string");
    let kept = stdout.strip_suffix(TRUNCATION_MARKER).expect("the output must be marked truncated");

    assert_eq!(result["truncated"], json!(true));
    assert!(kept.len() <= cap, "kept {} bytes over a {cap}-byte cap", kept.len());
    assert!(kept.len() > cap - 4, "the cap discarded more than a partial character");
    assert!(
        kept.chars().all(|c| c == '€' || c == '\n'),
        "the cut produced a replacement or partial character"
    );
    // Excess output is drained rather than left in the pipe, so the command completes.
    assert_eq!(result["exit_code"], json!(0));
}

// ── A cancelled call takes the command with it ────────────────────────

/// Whether the test process's direct child `pid` has exited, reaping it if so.
fn child_has_exited(pid: i32) -> bool {
    // SAFETY: a non-blocking `waitpid` on a pid with a null status pointer cannot
    // violate memory safety.
    let reaped = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    // `pid` means it exited now; -1 (ECHILD) means it was already reaped.
    reaped == pid || reaped == -1
}

#[tokio::test]
async fn dropping_the_call_kills_the_shell_and_its_children() {
    let dir = tempfile::tempdir().unwrap();
    let shell_marker = dir.path().join("shell.pid");
    let child_marker = dir.path().join("child.pid");
    let workspace = Workspace::new(dir.path());

    let command = format!(
        "echo $$ > {}; sleep 30 & echo $! > {}; wait",
        shell_marker.to_string_lossy(),
        child_marker.to_string_lossy()
    );
    let call = run_bash(workspace, &command, Some(60));
    // The call cannot finish within a second, so the timeout drops it mid-flight.
    let outcome = tokio::time::timeout(Duration::from_secs(1), call).await;
    assert!(outcome.is_err(), "the command must still be running when the call is dropped");

    tokio::time::sleep(Duration::from_millis(300)).await;

    let read_pid = |path: &std::path::Path| -> i32 {
        std::fs::read_to_string(path)
            .expect("the command must have recorded its pid")
            .trim()
            .parse()
            .expect("a numeric pid")
    };
    let shell = read_pid(&shell_marker);
    let child = read_pid(&child_marker);

    assert!(child_has_exited(shell), "the shell (pid {shell}) survived the dropped call");
    // SAFETY: signal 0 only probes for existence and cannot violate memory safety.
    let child_alive = unsafe { libc::kill(child, 0) } == 0;
    assert!(!child_alive, "a descendant (pid {child}) survived the dropped call");
}
