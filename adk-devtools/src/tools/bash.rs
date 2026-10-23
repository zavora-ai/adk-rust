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

        let mut cmd = self.launcher.command(&self.workspace, &command, ctx.as_ref())?;
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
        cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);

        let kill_group_on_exit = self.workspace.kills_bash_group_on_exit();
        let mut child = cmd.spawn().map_err(DevToolError::from)?;
        #[cfg(unix)]
        let mut group = ProcessGroupGuard { pid: child.id() };
        // On failure the suspended child is dropped, and `kill_on_drop` ends it.
        #[cfg(windows)]
        let mut group = ProcessGroupGuard {
            job: Some(job::Job::contain(&child).map_err(|error| {
                DevToolError::Other(format!(
                    "the command could not be placed in a Job Object, which stops the \
                     processes it starts, so it was not run: {error}"
                ))
            })?),
            keep_on_exit: !kill_group_on_exit,
        };
        #[cfg(not(any(unix, windows)))]
        let mut group = ProcessGroupGuard {};
        tracing::debug!(
            bash.isolation = self.launcher.isolation(),
            process.pid = child.id(),
            "started bash command"
        );

        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
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
                        group.kill();
                    }
                }
            };
            let (stdout, stderr, ()) = tokio::join!(
                capture(stdout_pipe, cap, ctx.as_ref(), "stdout"),
                capture(stderr_pipe, cap, ctx.as_ref(), "stderr"),
                exited,
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
                // Also signal the child directly, which is all that is available elsewhere.
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
    job: Option<job::Job>,
    /// Whether the job's processes keep running once the call ends normally.
    #[cfg(windows)]
    keep_on_exit: bool,
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
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
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
        self.kill();
    }
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

/// The Job Object that holds a `bash` command's process tree on Windows (spec 3.2 §10 W1).
#[cfg(windows)]
mod job {
    use std::io;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    /// A Job Object holding a command and every process it starts.
    pub(super) struct Job(OwnedHandle);

    impl Job {
        /// Places the suspended `child` in a new job that kills its processes when its last
        /// handle closes, then lets the child run.
        pub(super) fn contain(child: &tokio::process::Child) -> io::Result<Self> {
            let (Some(process), Some(pid)) = (child.raw_handle(), child.id()) else {
                return Err(io::Error::other("the command exited before it was started"));
            };
            // SAFETY: both arguments may be null: no security attributes and no name.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `handle` was just created and nothing else owns it.
            let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
            job.kill_on_close(true)?;
            // SAFETY: both handles are open for the call.
            if unsafe { AssignProcessToJobObject(job.0.as_raw_handle(), process) } == 0 {
                return Err(io::Error::last_os_error());
            }
            resume(pid)?;
            Ok(job)
        }

        /// Kills every process in the job.
        pub(super) fn terminate(&self) {
            // SAFETY: the handle is open. A failure means the job has no process left.
            unsafe {
                TerminateJobObject(self.0.as_raw_handle(), 1);
            }
        }

        /// Sets whether closing the job's last handle kills its processes.
        pub(super) fn kill_on_close(&self, kill: bool) -> io::Result<()> {
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            if kill {
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            }
            // SAFETY: `limits` is readable and the size passed is its own; the handle is open.
            let set = unsafe {
                SetInformationJobObject(
                    self.0.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if set == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
        }
    }

    /// Resumes the threads of the suspended process `pid`, which has only the primary
    /// thread it was created with.
    fn resume(pid: u32) -> io::Result<()> {
        // SAFETY: a thread snapshot takes no process id; the result is checked below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `snapshot` was just opened and nothing else owns it.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
        let mut entry =
            THREADENTRY32 { dwSize: size_of::<THREADENTRY32>() as u32, ..Default::default() };
        let mut resumed = false;
        // SAFETY: `entry` is writable and carries its own size; the snapshot is open.
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) } != 0;
        while found {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: the thread id comes from the snapshot; the result is checked.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: `thread` was just opened and nothing else owns it.
                let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
                // SAFETY: the handle is open with `THREAD_SUSPEND_RESUME`.
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                resumed = true;
            }
            // SAFETY: as for `Thread32First`.
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } != 0;
        }
        if resumed {
            Ok(())
        } else {
            Err(io::Error::other("the command's thread was not found to start it"))
        }
    }
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
