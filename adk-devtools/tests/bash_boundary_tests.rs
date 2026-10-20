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
use adk_devtools::{BashTool, DevToolset, Workspace};
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
    let workspace = Workspace::new(dir.path()).allow_bash(true);
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
    let workspace = Workspace::new(dir.path()).allow_bash(true).inherit_env(true);
    let result = run_bash(workspace, "echo \"v=$ADK_TEST_OPT_IN\"", None).await.expect("must run");

    assert!(
        result["stdout"].as_str().unwrap_or_default().contains("v=visible"),
        "opting in must actually pass the environment through"
    );

    unsafe {
        std::env::remove_var("ADK_TEST_OPT_IN");
    }
}

// ── bash is opt-in, and the operator's timeout is a ceiling ───────────

#[tokio::test]
async fn a_new_workspace_does_not_run_bash() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(dir.path());
    assert!(!workspace.bash_allowed());

    let toolset = DevToolset::new(workspace.clone());
    let readonly_ctx: Arc<dyn ReadonlyContext> = Arc::new(TestCtx);
    let tools = adk_core::Toolset::tools(&toolset, readonly_ctx).await.unwrap();
    assert!(!tools.iter().any(|tool| tool.name() == "bash"), "bash must not be offered");

    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx);
    let refused = BashTool::new(workspace).execute(ctx, json!({ "command": "echo ran" })).await;
    assert!(refused.is_err(), "a workspace that never enabled bash must refuse it");
}

#[tokio::test]
async fn a_model_cannot_raise_the_workspace_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        Workspace::new(dir.path()).allow_bash(true).bash_timeout(Duration::from_secs(1));

    let started = std::time::Instant::now();
    let result = run_bash(workspace, "sleep 10", Some(3600)).await;

    assert!(result.is_err(), "the command must hit the workspace limit");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the requested hour replaced the one-second limit: {:?}",
        started.elapsed()
    );
}

// ── A timeout takes descendants with it ───────────────────────────────

#[tokio::test]
async fn a_timeout_kills_processes_the_command_started() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("grandchild.pid");
    let workspace = Workspace::new(dir.path()).allow_bash(true);

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
    let workspace = Workspace::new(dir.path()).allow_bash(true).max_output_bytes(4);

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
    let workspace = Workspace::new(dir.path()).allow_bash(true);

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

// ── Nothing the command starts outlives the call (spec 3.1 §10 D3, I22, I23) ──

/// A `sleep` argument no other process on the host is using, so `pgrep -f` finds
/// only the process this test started.
fn unique_sleep() -> String {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
    format!("300.{}{:06}", std::process::id(), nanos % 1_000_000)
}

/// Whether a process whose command line contains `pattern` is running.
fn running(pattern: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .expect("pgrep must be available")
        .status
        .success()
}

#[tokio::test]
async fn a_background_process_does_not_outlive_the_call() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        Workspace::new(dir.path()).allow_bash(true).bash_timeout(Duration::from_secs(20));
    let sleep = unique_sleep();

    // The background `sleep` holds the output pipes open, so the call returns
    // promptly only if the group is killed when the shell exits.
    let started = std::time::Instant::now();
    let result = run_bash(workspace, &format!("sleep {sleep} & echo started"), None)
        .await
        .expect("the call must return when the shell exits, not at the timeout");

    assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
    assert_eq!(result["stdout"], json!("started\n"));
    assert_eq!(result["exit_code"], json!(0));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!running(&format!("sleep {sleep}")), "the background sleep survived the call");
}

#[tokio::test]
async fn a_detached_background_process_does_not_outlive_the_call() {
    let (_dir, workspace) = temp_workspace();
    let sleep = unique_sleep();

    // With its output redirected the sleep holds no pipe, so the call returned at once
    // and left it running.
    let result =
        run_bash(workspace, &format!("sleep {sleep} >/dev/null 2>&1 & echo started"), None)
            .await
            .expect("must run");

    assert_eq!(result["stdout"], json!("started\n"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!running(&format!("sleep {sleep}")), "the background sleep survived the call");
}

#[tokio::test]
async fn background_processes_survive_when_the_workspace_opts_out() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(dir.path()).allow_bash(true).bash_kill_group_on_exit(false);
    let sleep = unique_sleep();

    run_bash(workspace, &format!("sleep {sleep} >/dev/null 2>&1 & echo started"), None)
        .await
        .expect("must run");

    tokio::time::sleep(Duration::from_millis(300)).await;
    let survived = running(&format!("sleep {sleep}"));
    let _ = std::process::Command::new("pkill").args(["-f", &format!("sleep {sleep}")]).status();
    assert!(survived, "opting out must leave the background process running");
}

#[tokio::test]
async fn standard_input_is_empty() {
    // Give the test process a standard input with data in it; an inherited stdin
    // would hand that data to the command.
    let mut fds = [0; 2];
    // SAFETY: `fds` is a writable two-element array, as `pipe` requires.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let input = b"secret-input\n";
    // SAFETY: `fds[1]` is the pipe's open write end and `input` is a valid buffer.
    let written = unsafe { libc::write(fds[1], input.as_ptr().cast(), input.len()) };
    assert_eq!(written, input.len() as isize);
    // SAFETY: duplicating and replacing descriptor 0 cannot violate memory safety; the
    // saved copy restores it below.
    let saved = unsafe { libc::dup(0) };
    unsafe {
        libc::dup2(fds[0], 0);
        libc::close(fds[0]);
    }

    let (_dir, workspace) = temp_workspace();
    let result = run_bash(workspace, "read line; echo \"status=$? line=$line\"", Some(5)).await;

    // SAFETY: as above.
    unsafe {
        libc::dup2(saved, 0);
        libc::close(saved);
        libc::close(fds[1]);
    }
    let result = result.expect("read must see end of file, not wait for input");
    assert_eq!(result["stdout"], json!("status=1 line=\n"));
}

#[tokio::test]
async fn the_command_leads_a_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("shell.pid");
    let workspace = Workspace::new(dir.path()).allow_bash(true);
    let command = format!("echo $$ > {}; sleep 5", pid_file.to_string_lossy());

    let call = tokio::spawn(async move { run_bash(workspace, &command, None).await });
    let pid = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<libc::pid_t>().ok())
        {
            break pid;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    // SAFETY: `getsid` only queries the kernel and cannot violate memory safety.
    let session = unsafe { libc::getsid(pid) };
    call.abort();
    assert_eq!(session, pid, "the shell must lead its own session, with no controlling terminal");
}

// ── A launcher builds the process; the tool keeps its guarantees (spec 3.1 §10 D1) ──

/// Runs every script after a fixed prefix, through `sh -c`.
struct PrefixLauncher;

impl adk_devtools::CommandLauncher for PrefixLauncher {
    fn command(
        &self,
        ws: &Workspace,
        script: &str,
        ctx: &dyn ToolContext,
    ) -> Result<tokio::process::Command, adk_devtools::DevToolError> {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(format!("echo launched-for-{}; {script}", ctx.session_id()))
            .current_dir(ws.root())
            .env_clear()
            .envs(ws.bash_env());
        Ok(cmd)
    }

    fn isolation(&self) -> &str {
        "prefix"
    }
}

/// Refuses every script, as a launcher whose sandbox is unavailable would.
struct RefusingLauncher;

impl adk_devtools::CommandLauncher for RefusingLauncher {
    fn command(
        &self,
        _ws: &Workspace,
        _script: &str,
        _ctx: &dyn ToolContext,
    ) -> Result<tokio::process::Command, adk_devtools::DevToolError> {
        Err(adk_devtools::DevToolError::Other("sandbox unavailable".into()))
    }

    fn isolation(&self) -> &str {
        "refusing"
    }
}

#[tokio::test]
async fn a_launcher_builds_the_command_and_the_tool_keeps_its_guarantees() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(dir.path()).allow_bash(true);
    let bash = BashTool::new(workspace).with_launcher(Arc::new(PrefixLauncher));
    let sleep = unique_sleep();
    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx);

    let result = bash
        .execute(ctx, json!({ "command": format!("sleep {sleep} & read line; echo status=$?") }))
        .await
        .expect("must run");

    assert_eq!(result["stdout"], json!("launched-for-session-1\nstatus=1\n"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!running(&format!("sleep {sleep}")), "the group kill must apply to launched commands");
}

#[tokio::test]
async fn a_launcher_error_fails_the_call_without_running_anything() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let bash = BashTool::new(Workspace::new(dir.path()).allow_bash(true))
        .with_launcher(Arc::new(RefusingLauncher));
    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx);

    let result =
        bash.execute(ctx, json!({ "command": format!("touch {}", marker.display()) })).await;

    assert!(result.unwrap_err().to_string().contains("sandbox unavailable"));
    assert!(!marker.exists());
}

#[test]
fn the_host_launcher_runs_sh_in_the_workspace_root_with_the_workspace_environment() {
    use adk_devtools::{CommandLauncher, HostLauncher};

    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(dir.path()).env_allowlist(["PATH"]);
    let command = HostLauncher.command(&workspace, "echo hi", &TestCtx).unwrap();
    let command = command.as_std();

    assert_eq!(HostLauncher.isolation(), "host");
    assert_eq!(command.get_program(), "sh");
    assert_eq!(command.get_args().collect::<Vec<_>>(), ["-c", "echo hi"]);
    assert_eq!(command.get_current_dir(), Some(workspace.root()));
    let envs: Vec<_> = command.get_envs().map(|(key, _)| key.to_owned()).collect();
    assert_eq!(envs, ["PATH"]);
}
