//! `bash` — run a shell command inside the workspace.
//!
//! Executes host-local (`sh -c`) with the working directory pinned to the
//! workspace root and a timeout. Streams stdout/stderr incrementally via
//! [`ToolContext::emit_progress`] for UI implementations that display live
//! terminal output.
//!
//! **Not** strongly isolated; production deployments should run behind a
//! containerized `CodeExecutor` (see the coding-agent design, §9).
//! Mutating use requires [`Workspace::bash_allowed`].

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::error::DevToolError;
use crate::tools::read::require_str;
use crate::workspace::Workspace;

/// Runs a shell command in the workspace root with a timeout.
///
/// Streams stdout and stderr line-by-line via [`ToolContext::emit_progress`]
/// so UI layers can display live terminal output. The final result still
/// contains the complete stdout/stderr for the model to consume.
pub struct BashTool {
    workspace: Workspace,
}

impl BashTool {
    /// Create a `bash` tool bound to `workspace`.
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command in the workspace root and return stdout, stderr, and the \
         exit code. Streams output incrementally for live UI display. Has a timeout."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to run." },
                "timeout_secs": { "type": "integer", "description": "Optional timeout in seconds (default: workspace setting)." }
            },
            "required": ["command"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if !self.workspace.bash_allowed() {
            return Err(DevToolError::BashDisabled.into());
        }
        let command = require_str(&args, "command")?;
        let timeout = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .map(Duration::from_secs)
            .unwrap_or_else(|| self.workspace.bash_timeout_value());

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(&command)
            .current_dir(self.workspace.root())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // The parent environment of an agent process routinely holds provider API keys,
        // and `env` would print them. Pass through only what tools need, unless the
        // workspace opts into inheriting.
        if !self.workspace.inherits_env() {
            cmd.env_clear();
            for (key, value) in self.workspace.bash_env() {
                cmd.env(key, value);
            }
        }

        // Run in its own process group so a timeout can terminate descendants. Killing
        // only the direct child left `sh`'s children running after the tool returned.
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(DevToolError::from)?;
        let child_pid = child.id();

        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let cap = self.workspace.max_output();

        // Stream stdout and stderr concurrently, emitting lines as progress events
        let ctx_out = ctx.clone();
        let stdout_task = tokio::spawn(async move {
            let mut output = String::new();
            if let Some(pipe) = stdout_pipe {
                let mut reader = BufReader::new(pipe).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    ctx_out.emit_progress("stdout", &format!("{line}\n")).await;
                    output.push_str(&line);
                    output.push('\n');
                }
            }
            output
        });

        let ctx_err = ctx.clone();
        let stderr_task = tokio::spawn(async move {
            let mut output = String::new();
            if let Some(pipe) = stderr_pipe {
                let mut reader = BufReader::new(pipe).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    ctx_err.emit_progress("stderr", &format!("{line}\n")).await;
                    output.push_str(&line);
                    output.push('\n');
                }
            }
            output
        });

        // Wait for completion with timeout
        let result = tokio::time::timeout(timeout, async {
            let status = child.wait().await?;
            let stdout = stdout_task.await.unwrap_or_default();
            let stderr = stderr_task.await.unwrap_or_default();
            Ok::<_, std::io::Error>((status, stdout, stderr))
        })
        .await;

        match result {
            Ok(Ok((status, stdout, stderr))) => {
                let (stdout, out_trunc) = truncate(stdout, cap);
                let (stderr, err_trunc) = truncate(stderr, cap);
                Ok(json!({
                    "command": command,
                    "exit_code": status.code(),
                    "stdout": stdout,
                    "stderr": stderr,
                    "truncated": out_trunc || err_trunc,
                }))
            }
            Ok(Err(e)) => Err(DevToolError::from(e).into()),
            Err(_) => {
                terminate_process_group(&mut child, child_pid);
                ctx.emit_progress("stderr", &format!("\n[timeout after {}s]\n", timeout.as_secs()))
                    .await;
                Err(DevToolError::Timeout(timeout).into())
            }
        }
    }
}

/// Terminate a timed-out command and everything it started.
///
/// The child leads its own process group, so signalling the negated pid reaches
/// grandchildren too. Killing only the direct child left descendants — a spawned server,
/// a background build — running after the tool returned.
fn terminate_process_group(child: &mut tokio::process::Child, pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // SAFETY: `killpg` takes a process-group id and a signal, and cannot violate
        // memory safety. A failure means the group already exited.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;

    // Also signal the child directly, which is all that is available off Unix.
    let _ = child.start_kill();
}

fn truncate(mut s: String, cap: usize) -> (String, bool) {
    if s.len() <= cap {
        return (s, false);
    }
    s.truncate(cap);
    s.push_str("\n…[truncated]");
    (s, true)
}
