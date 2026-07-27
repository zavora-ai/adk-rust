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
    let (_dir, workspace) = temp_workspace();
    let result = run_bash(workspace, "echo \"path=$PATH\"", None).await.expect("must run");
    let stdout = result["stdout"].as_str().unwrap_or_default();

    assert!(stdout.contains("path=/"), "PATH must be available or nothing can run: {stdout}");
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
