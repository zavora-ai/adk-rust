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
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::DevToolError;
use crate::tools::read::require_str;
use crate::workspace::Workspace;

/// Runs a shell command in the workspace root with a timeout.
///
/// Streams stdout and stderr line-by-line via [`ToolContext::emit_progress`]
/// so UI layers can display live terminal output. The final result contains
/// stdout and stderr, each capped at [`Workspace::max_output`] bytes; output
/// past the cap is read and discarded, so memory stays bounded.
///
/// Cancelling the call (dropping its future) kills the command and every
/// process it started, as a timeout does.
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
                "timeout_secs": { "type": "integer", "description": "Optional timeout in seconds. Defaults to, and cannot exceed, the workspace limit." }
            },
            "required": ["command"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if !self.workspace.bash_allowed() {
            return Err(DevToolError::BashDisabled.into());
        }
        let command = require_str(&args, "command")?;
        // The operator's timeout is a ceiling: a model may ask for less time, never more.
        let limit = self.workspace.bash_timeout_value();
        let timeout = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .map_or(limit, |requested| Duration::from_secs(requested).min(limit));

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(&command)
            .current_dir(self.workspace.root())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // The parent environment of an agent process routinely holds provider API keys,
        // and `env` would print them. Pass through only what tools need, unless the
        // workspace opts into inheriting.
        if !self.workspace.inherits_env() {
            cmd.env_clear();
            for (key, value) in self.workspace.bash_env() {
                cmd.env(key, value);
            }
        }

        // Run in its own process group so a timeout or a cancelled call can terminate
        // descendants. Killing only the direct child left `sh`'s children running.
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(DevToolError::from)?;
        let mut group = ProcessGroupGuard { pid: child.id() };

        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let cap = self.workspace.max_output();

        let result = tokio::time::timeout(timeout, async {
            let (stdout, stderr) = tokio::join!(
                capture(stdout_pipe, cap, ctx.as_ref(), "stdout"),
                capture(stderr_pipe, cap, ctx.as_ref(), "stderr"),
            );
            let status = child.wait().await?;
            // The child is reaped, so its pid may be reused and must not be signalled.
            group.disarm();
            Ok::<_, std::io::Error>((status, stdout, stderr))
        })
        .await;

        match result {
            Ok(Ok((status, (stdout, out_exceeded), (stderr, err_exceeded)))) => {
                let (stdout, out_trunc) = bound_output(&stdout, cap, out_exceeded);
                let (stderr, err_trunc) = bound_output(&stderr, cap, err_exceeded);
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
                group.kill();
                // Also signal the child directly, which is all that is available off Unix.
                let _ = child.start_kill();
                ctx.emit_progress("stderr", &format!("\n[timeout after {}s]\n", timeout.as_secs()))
                    .await;
                Err(DevToolError::Timeout(timeout).into())
            }
        }
    }
}

/// Kills the command's whole process group, including when dropped.
///
/// The child leads its own process group, so signalling the negated pid reaches
/// grandchildren too. `kill_on_drop` reaches only the direct child, which left
/// descendants — a spawned server, a background build — running after a timeout or a
/// cancelled call. The guard is disarmed once the child is reaped, because its pid may
/// then be reused.
struct ProcessGroupGuard {
    pid: Option<u32>,
}

impl ProcessGroupGuard {
    fn kill(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // SAFETY: `killpg` takes a process-group id and a signal, and cannot violate
            // memory safety. A failure means the group already exited.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = self.pid;
    }

    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Reads one output stream to EOF, keeping at most `cap` bytes.
///
/// Returns the kept bytes and whether the stream exceeded the cap. Output past the
/// cap is read and discarded rather than left in the pipe, because a writer blocked
/// on a full pipe would hang until the timeout. Complete lines are forwarded as
/// progress up to the cap.
async fn capture(
    pipe: Option<impl AsyncRead + Unpin>,
    cap: usize,
    ctx: &dyn ToolContext,
    stream: &str,
) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut exceeded = false;
    let Some(mut pipe) = pipe else {
        return (kept, exceeded);
    };
    let mut chunk = [0u8; 8192];
    // Bytes of `kept` already forwarded as progress.
    let mut emitted = 0;
    loop {
        let read = match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let room = cap - kept.len();
        if read > room && !exceeded {
            exceeded = true;
            tracing::debug!(
                output.stream = stream,
                output.cap = cap,
                "discarding output past the cap"
            );
        }
        kept.extend_from_slice(&chunk[..read.min(room)]);
        if let Some(newline) = kept[emitted..].iter().rposition(|&byte| byte == b'\n') {
            let end = emitted + newline + 1;
            ctx.emit_progress(stream, &String::from_utf8_lossy(&kept[emitted..end])).await;
            emitted = end;
        }
    }
    if emitted < kept.len() {
        ctx.emit_progress(stream, &String::from_utf8_lossy(&kept[emitted..])).await;
    }
    (kept, exceeded)
}

/// Decodes captured output and bounds it to `cap` bytes on a character boundary.
///
/// The byte cap can split a multi-byte character, and lossy decoding can lengthen
/// the text, so the cut is taken on the nearest boundary at or below `cap`.
fn bound_output(bytes: &[u8], cap: usize, exceeded: bool) -> (String, bool) {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    let cut = text.len() > cap;
    if cut {
        text.truncate(text.floor_char_boundary(cap));
    }
    let truncated = exceeded || cut;
    if truncated {
        text.push_str("\n…[truncated]");
    }
    (text, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cap_inside_a_multi_byte_character_does_not_panic() {
        // `€` is three bytes, so a cap of 4 falls inside the second one.
        let (text, truncated) = bound_output("€€€".as_bytes(), 4, false);
        assert_eq!((text.as_str(), truncated), ("€\n…[truncated]", true));
    }

    #[test]
    fn a_character_split_by_the_capture_cap_is_dropped() {
        // The capture kept `a` and the first byte of `é` before the cap was hit.
        let (text, truncated) = bound_output(&"aé".as_bytes()[..2], 2, true);
        assert_eq!((text.as_str(), truncated), ("a\n…[truncated]", true));
    }

    #[test]
    fn output_within_the_cap_is_unchanged() {
        let (text, truncated) = bound_output("héllo\n".as_bytes(), 1024, false);
        assert_eq!((text.as_str(), truncated), ("héllo\n", false));
    }
}
