//! What a `bash` call leaves running on Windows (spec 3.2 §10 W1).
//!
//! The command runs in a Job Object, so every process it starts is killed when the shell
//! exits, on a timeout and when the call is dropped, unless the workspace keeps background
//! processes. The scripts run under Windows PowerShell through a launcher, so the tests need
//! nothing from Git for Windows; `ping -n 300` stands in for a long-running server.

#![cfg(windows)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::{Tool, ToolContext};
use adk_devtools::{BashTool, CommandLauncher, DevToolError, Workspace};
use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

mod common;
use common::TestCtx;

/// Runs each script with `powershell.exe -Command` in the workspace root.
struct PowerShell;

impl CommandLauncher for PowerShell {
    fn command(
        &self,
        ws: &Workspace,
        script: &str,
        _ctx: &dyn ToolContext,
    ) -> Result<tokio::process::Command, DevToolError> {
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
            .current_dir(ws.root());
        Ok(cmd)
    }

    fn isolation(&self) -> &str {
        "host-powershell"
    }
}

/// Starts `ping` in the background with `start_options`, records its pid in `pid.txt`,
/// then runs `then`.
fn background_ping(start_options: &str, then: &str) -> String {
    format!(
        "$p = Start-Process -FilePath ping.exe -ArgumentList '-n','300','127.0.0.1' \
         {start_options} -PassThru; Set-Content -Path pid.txt -Value $p.Id; {then}"
    )
}

async fn run(
    workspace: Workspace,
    script: &str,
    timeout_secs: Option<u64>,
) -> adk_core::Result<Value> {
    let bash = BashTool::new(workspace).with_launcher(Arc::new(PowerShell));
    let mut args = json!({ "command": script });
    if let Some(secs) = timeout_secs {
        args["timeout_secs"] = json!(secs);
    }
    bash.execute(Arc::new(TestCtx), args).await
}

fn workspace(dir: &Path) -> Workspace {
    Workspace::new(dir).allow_bash(true).inherit_env(true).bash_timeout(Duration::from_secs(60))
}

/// The pid the script recorded.
fn recorded_pid(dir: &Path) -> u32 {
    let text = std::fs::read_to_string(dir.join("pid.txt")).expect("the script records a pid");
    text.trim().parse().expect("pid.txt holds a pid")
}

/// Whether the process `pid` has ended, or ends within `wait`.
fn ends_within(pid: u32, wait: Duration) -> bool {
    // SAFETY: plain arguments; a null handle means no such process is left.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        return true;
    }
    let millis = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: the handle is open with `PROCESS_SYNCHRONIZE` and closed once.
    unsafe {
        let waited = WaitForSingleObject(process, millis);
        CloseHandle(process);
        waited == WAIT_OBJECT_0
    }
}

fn terminate(pid: u32) {
    // SAFETY: plain arguments; the handle is checked and closed once.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !process.is_null() {
            TerminateProcess(process, 1);
            CloseHandle(process);
        }
    }
}

#[tokio::test]
async fn a_background_process_does_not_outlive_the_call() {
    let dir = tempfile::tempdir().unwrap();
    // `-NoNewWindow` hands the background process the call's output pipes, so the call
    // returns before its timeout only if the job is killed when the shell exits.
    let script = background_ping("-NoNewWindow", "Write-Output started");

    let started = Instant::now();
    let result = run(workspace(dir.path()), &script, None)
        .await
        .expect("the call must return when the shell exits, not at the timeout");

    assert!(started.elapsed() < Duration::from_secs(45), "took {:?}", started.elapsed());
    assert!(result["stdout"].as_str().unwrap_or_default().contains("started"), "{result}");
    assert_eq!(result["exit_code"], json!(0));
    let pid = recorded_pid(dir.path());
    let ended = ends_within(pid, Duration::from_secs(5));
    terminate(pid);
    assert!(ended, "the background process outlived the call");
}

#[tokio::test]
async fn a_timeout_kills_every_process_the_command_started() {
    let dir = tempfile::tempdir().unwrap();
    let script = background_ping("-WindowStyle Hidden", "Start-Sleep -Seconds 300");

    let result = run(workspace(dir.path()), &script, Some(20)).await;

    assert!(result.is_err(), "the call must time out: {result:?}");
    let pid = recorded_pid(dir.path());
    let ended = ends_within(pid, Duration::from_secs(5));
    terminate(pid);
    assert!(ended, "the background process outlived the timeout");
}

#[tokio::test]
async fn dropping_the_call_kills_every_process_the_command_started() {
    let dir = tempfile::tempdir().unwrap();
    let script = background_ping("-WindowStyle Hidden", "Start-Sleep -Seconds 300");

    let call = run(workspace(dir.path()), &script, None);
    // The call cannot finish within the window, so the outer timeout drops it mid-flight.
    let dropped = tokio::time::timeout(Duration::from_secs(20), call).await;

    assert!(dropped.is_err(), "the call finished: {dropped:?}");
    let pid = recorded_pid(dir.path());
    let ended = ends_within(pid, Duration::from_secs(5));
    terminate(pid);
    assert!(ended, "the background process outlived the dropped call");
}

#[tokio::test]
async fn background_processes_survive_when_the_workspace_opts_out() {
    let dir = tempfile::tempdir().unwrap();
    let script = background_ping("-WindowStyle Hidden", "Write-Output started");

    run(workspace(dir.path()).bash_kill_group_on_exit(false), &script, None)
        .await
        .expect("must run");

    let pid = recorded_pid(dir.path());
    let ended = ends_within(pid, Duration::from_secs(2));
    terminate(pid);
    assert!(!ended, "opting out must leave the background process running");
}
