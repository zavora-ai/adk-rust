//! `bash` — run a shell command inside the workspace.
//!
//! Executes host-local (`sh -c`) by default, with the working directory pinned to
//! the workspace root and a timeout. A [`CommandLauncher`] can build the process
//! instead, for example to run the script under an OS sandbox. Streams
//! stdout/stderr incrementally via [`ToolContext::emit_progress`] for UI
//! implementations that display live terminal output.
//!
//! The command runs with standard input at end of file, in a new session with no
//! controlling terminal, and its process group is killed when the shell exits, on a
//! timeout and when the call is cancelled. On Windows the command runs in a Job Object,
//! and the job's processes are killed at the same points (spec 3.2 §10 W1).
//!
//! **Not** strongly isolated with the default [`HostLauncher`]; production
//! deployments should run behind a sandboxing launcher or a containerized
//! `CodeExecutor` (see the coding-agent design, §9).
//! Mutating use requires [`Workspace::bash_allowed`].

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{Result, Tool, ToolContext};
#[cfg(windows)]
use adk_sandbox::process_tree::{ContainedChild, ContainedProcess, JobObject};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::DevToolError;
use crate::launcher::{CommandLauncher, HostLauncher};
use crate::tools::read::require_str;
use crate::workspace::Workspace;

/// Runs a shell command in the workspace root with a timeout.
///
/// Streams stdout and stderr line-by-line via [`ToolContext::emit_progress`]
/// so UI layers can display live terminal output. The final result contains
/// stdout and stderr, each capped at [`Workspace::max_output`] bytes; output
/// past the cap is read and discarded, so memory stays bounded.
///
/// The command reads end of file from standard input and starts in a new session,
/// so it has no controlling terminal and leads its own process group. When the
/// shell exits, the rest of the group — anything it started in the background —
/// is killed, unless [`Workspace::bash_kill_group_on_exit`] turns that off. A
/// timeout or a cancelled call (dropping its future) kills the whole group too.
/// A process that starts its own session or group escapes the kill.
///
/// On Windows the command starts suspended and runs only once it is in a Job Object, so
/// every process it starts belongs to the job and none can leave it. The job's processes
/// are killed where Unix kills the group, and when the agent process exits during the
/// call (spec 3.2 §10 W1).
///
/// The process comes from a [`CommandLauncher`]: [`HostLauncher`] by default,
/// another one through [`with_launcher`](Self::with_launcher).
pub struct BashTool {
    workspace: Workspace,
    launcher: Arc<dyn CommandLauncher>,
}

impl BashTool {
    /// Create a `bash` tool bound to `workspace`, running commands with [`HostLauncher`].
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace, launcher: Arc::new(HostLauncher) }
    }

    /// Build each command with `launcher` instead of [`HostLauncher`].
    ///
    /// The timeout ceiling, output cap, streaming, standard input, session and
    /// process-group kill stay with the tool. Implements spec 3.1 §10 D1.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// use adk_devtools::{BashTool, HostLauncher, Workspace};
    ///
    /// let workspace = Workspace::new("./my-repo").allow_bash(true);
    /// let bash = BashTool::new(workspace).with_launcher(Arc::new(HostLauncher));
    /// # let _ = bash;
    /// ```
    #[must_use]
    pub fn with_launcher(mut self, launcher: Arc<dyn CommandLauncher>) -> Self {
        self.launcher = launcher;
        self
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        if self.workspace.kills_bash_group_on_exit() {
            "Run a shell command in the workspace root and return stdout, stderr, and the \
             exit code. Streams output incrementally for live UI display. Has a timeout. \
             Standard input is empty, and background processes the command starts are \
             stopped when it exits."
        } else {
            "Run a shell command in the workspace root and return stdout, stderr, and the \
             exit code. Streams output incrementally for live UI display. Has a timeout. \
             Standard input is empty."
        }
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

        let kill_group_on_exit = self.workspace.kills_bash_group_on_exit();
        // A launcher may start a confined process itself (spec 3.2 §10 W5); starting one
        // can take long, so it runs on the blocking pool.
        #[cfg(windows)]
        let contained = {
            let (launcher, ws, script, call) =
                (self.launcher.clone(), self.workspace.clone(), command.clone(), ctx.clone());
            tokio::task::spawn_blocking(move || {
                launcher.spawn_contained(&ws, &script, &[], call.as_ref())
            })
            .await
            .map_err(|error| {
                DevToolError::Other(format!("the command could not be started: {error}"))
            })?
        };
        #[cfg(windows)]
        let (mut child, mut group, stdout_pipe, stderr_pipe) = match contained {
            Some(contained) => {
                let ContainedChild { job, process, stdin, stdout, stderr, .. } = contained?;
                // Standard input reads end of file, as `Stdio::null()` gives a spawned one.
                drop(stdin);
                let pipe =
                    |file: std::fs::File| -> Pipe { Box::new(tokio::fs::File::from_std(file)) };
                (
                    Running::Contained(process),
                    ProcessGroupGuard { job: Some(job), keep_on_exit: !kill_group_on_exit },
                    stdout.map(pipe),
                    stderr.map(pipe),
                )
            }
            None => self.spawn(&command, ctx.as_ref(), kill_group_on_exit)?,
        };
        #[cfg(not(windows))]
        let (mut child, mut group, stdout_pipe, stderr_pipe) =
            self.spawn(&command, ctx.as_ref(), kill_group_on_exit)?;
        tracing::debug!(
            bash.isolation = self.launcher.isolation(),
            process.pid = child.id(),
            "started bash command"
        );

        let cap = self.workspace.max_output();

        let result = tokio::time::timeout(timeout, async {
            let exited = async {
                if kill_group_on_exit {
                    // The shell has exited but is not reaped, so its pid, which is the group
                    // id, cannot be reused while the rest of the group is killed. That also
                    // closes the output pipes a background process holds open.
                    #[cfg(unix)]
                    let shell_exited = wait_for_exit(child.id()).await;
                    // The job, not the pid, names the processes, so the shell can be reaped.
                    #[cfg(windows)]
                    let shell_exited = child.wait().await.is_ok();
                    #[cfg(not(any(unix, windows)))]
                    let shell_exited = false;
                    if shell_exited {
                        group.kill().await;
                    }
                }
            };
            let (stdout, stderr, ()) = tokio::join!(
                capture(stdout_pipe, cap, ctx.as_ref(), "stdout"),
                capture(stderr_pipe, cap, ctx.as_ref(), "stderr"),
                exited,
            );
            let exit_code = child.wait().await?;
            // The child is reaped, so its pid may be reused and must not be signalled.
            group.disarm();
            Ok::<_, std::io::Error>((exit_code, stdout, stderr))
        })
        .await;

        match result {
            Ok(Ok((exit_code, (stdout, out_exceeded), (stderr, err_exceeded)))) => {
                let (stdout, out_trunc) = bound_output(&stdout, cap, out_exceeded);
                let (stderr, err_trunc) = bound_output(&stderr, cap, err_exceeded);
                Ok(json!({
                    "command": command,
                    "exit_code": exit_code,
                    "stdout": stdout,
                    "stderr": stderr,
                    "truncated": out_trunc || err_trunc,
                }))
            }
            Ok(Err(e)) => Err(DevToolError::from(e).into()),
            Err(_) => {
                group.kill().await;
                // Also signal the child directly, which is all that is available elsewhere.
                child.start_kill();
                ctx.emit_progress("stderr", &format!("\n[timeout after {}s]\n", timeout.as_secs()))
                    .await;
                Err(DevToolError::Timeout(timeout).into())
            }
        }
    }
}

impl BashTool {
    /// Spawns the launcher's command for `script`: standard input at end of file, output
    /// piped, in a new session on Unix and in a Job Object on Windows.
    fn spawn(
        &self,
        script: &str,
        ctx: &dyn ToolContext,
        kill_group_on_exit: bool,
    ) -> Result<(Running, ProcessGroupGuard, Option<Pipe>, Option<Pipe>)> {
        let mut cmd = self.launcher.command(&self.workspace, script, ctx)?;
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);

        // A new session detaches the command from any controlling terminal, so it cannot
        // read from or inject input into the agent's terminal, and makes it the leader of
        // its own process group, so a timeout, a cancelled call or the shell's exit can
        // terminate its descendants.
        #[cfg(unix)]
        {
            // SAFETY: the closure runs in the forked child before `exec` and calls only
            // `setsid`, which is async-signal-safe and touches no memory of the parent.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        // The command runs only once it is in the job, so nothing it starts is outside it.
        #[cfg(windows)]
        cmd.creation_flags(adk_sandbox::process_tree::CREATE_SUSPENDED);

        let mut child = cmd.spawn().map_err(DevToolError::from)?;
        #[cfg(unix)]
        let group = ProcessGroupGuard { pid: child.id() };
        // On failure the suspended child is dropped, and `kill_on_drop` ends it.
        #[cfg(windows)]
        let group = ProcessGroupGuard {
            job: Some(contain(&child).map_err(|error| {
                DevToolError::Other(format!(
                    "the command could not be placed in a Job Object, which stops the \
                     processes it starts, so it was not run: {error}"
                ))
            })?),
            keep_on_exit: !kill_group_on_exit,
        };
        #[cfg(not(any(unix, windows)))]
        let group = ProcessGroupGuard {};
        // Only a Windows job is told whether to keep background processes.
        #[cfg(not(windows))]
        let _ = kill_group_on_exit;
        let stdout = child.stdout.take().map(|pipe| Box::new(pipe) as Pipe);
        let stderr = child.stderr.take().map(|pipe| Box::new(pipe) as Pipe);
        Ok((Running::Spawned(Box::new(child)), group, stdout, stderr))
    }
}

/// One end of a command's output.
type Pipe = Box<dyn AsyncRead + Send + Unpin>;

/// The process a call runs: the one `BashTool` spawned, or on Windows one its launcher
/// started in a Job Object (spec 3.2 §10 W5).
enum Running {
    Spawned(Box<tokio::process::Child>),
    #[cfg(windows)]
    Contained(ContainedProcess),
}

impl Running {
    fn id(&self) -> Option<u32> {
        match self {
            Self::Spawned(child) => child.id(),
            #[cfg(windows)]
            Self::Contained(process) => Some(process.id()),
        }
    }

    /// Waits for the process to exit; its exit code when it has one.
    async fn wait(&mut self) -> std::io::Result<Option<i32>> {
        match self {
            Self::Spawned(child) => child.wait().await.map(|status| status.code()),
            #[cfg(windows)]
            Self::Contained(process) => process.wait().await.map(Some),
        }
    }

    /// Signals a spawned process directly; a contained one ends with its job.
    fn start_kill(&mut self) {
        match self {
            Self::Spawned(child) => {
                let _ = child.start_kill();
            }
            #[cfg(windows)]
            Self::Contained(_) => {}
        }
    }
}

/// Kills the command's whole process group, including when dropped.
///
/// The child leads its own session and process group, so signalling the group
/// reaches grandchildren too. `kill_on_drop` reaches only the direct child, which
/// left descendants — a spawned server, a background build — running after a timeout
/// or a cancelled call. The guard is disarmed once the child is reaped, because its
/// pid may then be reused.
///
/// On Windows the guard holds the command's Job Object instead, which also kills its
/// processes when the agent process exits, since the job closes with it (spec 3.2 §10
/// W1).
struct ProcessGroupGuard {
    #[cfg(unix)]
    pid: Option<u32>,
    #[cfg(windows)]
    job: Option<JobObject>,
    /// Whether the job's processes keep running once the call ends normally.
    #[cfg(windows)]
    keep_on_exit: bool,
}

impl ProcessGroupGuard {
    /// Signals the Unix process group; a Windows job is ended by `kill` or the drop.
    fn signal(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // SAFETY: `killpg` takes a process-group id and a signal, and cannot violate
            // memory safety. A failure means the group already exited.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }

    /// Kills the command's processes. On Windows it returns once the job's processes have
    /// exited, for at most `JobObject::TERMINATION_WAIT`, since a process the job ended can
    /// still complete a write it had in flight.
    async fn kill(&self) {
        self.signal();
        #[cfg(windows)]
        if let Some(job) = &self.job {
            note_termination(job.terminate_and_wait_async(JobObject::TERMINATION_WAIT).await);
        }
    }

    fn disarm(&mut self) {
        #[cfg(unix)]
        {
            self.pid = None;
        }
        // The job's last handle closes here, which kills what is left in it unless the
        // workspace keeps background processes.
        #[cfg(windows)]
        if let Some(job) = self.job.take()
            && self.keep_on_exit
            && let Err(error) = job.kill_on_close(false)
        {
            tracing::warn!(
                error = %error,
                "background processes of the bash command were not kept"
            );
        }
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.signal();
        // A dropped call, such as a cancelled run's, waits briefly for its processes too.
        #[cfg(windows)]
        if let Some(job) = &self.job {
            note_termination(job.terminate_and_wait(JobObject::TERMINATION_WAIT));
        }
    }
}

/// Logs a job whose processes had not all exited when the wait ended.
#[cfg(windows)]
fn note_termination(result: std::io::Result<bool>) {
    match result {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            wait = ?JobObject::TERMINATION_WAIT,
            "a process of the bash command had not exited when the call ended"
        ),
        Err(error) => {
            tracing::warn!(error = %error, "could not wait for the bash command's processes to exit")
        }
    }
}

/// Places the suspended `child` in a Job Object, then lets it run (spec 3.2 §10 W1).
#[cfg(windows)]
fn contain(child: &tokio::process::Child) -> std::io::Result<JobObject> {
    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("the command exited before it was started"))?;
    JobObject::contain_suspended(pid)
}

/// Waits until the child `pid` has exited, without reaping it.
///
/// Returns `true` once the child is a zombie: `waitid` with `WNOWAIT` leaves it
/// unreaped, so its pid stays reserved until `Child::wait` reaps it. The wait blocks,
/// so it runs on the blocking pool.
#[cfg(unix)]
async fn wait_for_exit(pid: Option<u32>) -> bool {
    let Some(pid) = pid else { return false };
    let waited = tokio::task::spawn_blocking(move || {
        loop {
            // SAFETY: `info` is a zeroed, writable `siginfo_t` that outlives the call,
            // and `waitid` writes only into it.
            let status = unsafe {
                let mut info: libc::siginfo_t = std::mem::zeroed();
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if status == 0 {
                return true;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return false;
            }
        }
    })
    .await;
    waited.unwrap_or_else(|error| {
        tracing::debug!(error = %error, "waiting for the bash command to exit failed");
        false
    })
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
