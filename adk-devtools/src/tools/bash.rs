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
//! A result carries at most [`INLINE_OUTPUT_CHARS`](crate::INLINE_OUTPUT_CHARS) of output; an [`OutputStore`] keeps
//! the whole of a longer one (spec 3.1 §7.33 fix 8). [`BashTool::spawn`] starts a command
//! and returns a [`RunningCommand`] at once (spec 3.1 §10 D10).
//!
//! **Not** strongly isolated with the default [`HostLauncher`]; production
//! deployments should run behind a sandboxing launcher or a containerized
//! `CodeExecutor` (see the coding-agent design, §9).
//! Mutating use requires [`Workspace::bash_allowed`].

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::{Result, Tool, ToolContext};
#[cfg(windows)]
use adk_sandbox::process_tree::{ContainedChild, ContainedProcess, JobObject};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::error::DevToolError;
use crate::launcher::{CommandLauncher, HostLauncher};
use crate::tools::output::{Collector, OutputStore, Stream};
use crate::tools::read::require_str;
use crate::workspace::Workspace;

/// How long a killed command's output is still read: a process that left the group can
/// hold a pipe open, and its output is not waited for.
const KILL_GRACE: Duration = Duration::from_secs(1);

/// The longest partial line held back from progress before it is forwarded anyway.
const PROGRESS_LINE_MAX: usize = 8192;

/// Runs a shell command in the workspace root with a timeout.
///
/// Streams stdout and stderr line-by-line via [`ToolContext::emit_progress`]
/// so UI layers can display live terminal output. The result carries at most
/// [`INLINE_OUTPUT_CHARS`](crate::INLINE_OUTPUT_CHARS) of output, stdout and stderr together: a longer stream keeps
/// its first third and last two thirds of its share around a marker naming the bytes left
/// out (spec 3.1 §7.33 fix 8). With an [`OutputStore`]
/// ([`with_output_store`](Self::with_output_store)) the whole output, up to
/// [`Workspace::max_output`] bytes, goes to a file the marker and the result's `output_id`
/// name. Memory stays bounded either way.
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
    output_store: Option<Arc<dyn OutputStore>>,
}

impl BashTool {
    /// Create a `bash` tool bound to `workspace`, running commands with [`HostLauncher`].
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace, launcher: Arc::new(HostLauncher), output_store: None }
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

    /// Keep the whole output of a command whose result is cut in a file from `store`,
    /// named by the result's `output_id` (spec 3.1 §7.33 fix 8). The file holds at most
    /// [`Workspace::max_output`] bytes.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// use adk_core::ToolContext;
    /// use adk_devtools::{BashTool, OutputFile, OutputStore, Workspace};
    ///
    /// struct Temp;
    ///
    /// impl OutputStore for Temp {
    ///     fn create(&self, ctx: &dyn ToolContext, _command: &str) -> std::io::Result<OutputFile> {
    ///         let id = ctx.function_call_id().to_string();
    ///         let path = std::env::temp_dir().join(format!("{id}.log"));
    ///         let file = std::fs::File::create(&path)?;
    ///         Ok(OutputFile { id, path, file })
    ///     }
    /// }
    ///
    /// let bash = BashTool::new(Workspace::new(".").allow_bash(true))
    ///     .with_output_store(Arc::new(Temp));
    /// # let _ = bash;
    /// ```
    #[must_use]
    pub fn with_output_store(mut self, store: Arc<dyn OutputStore>) -> Self {
        self.output_store = Some(store);
        self
    }

    /// Starts `command` and returns at once (spec 3.1 §10 D10).
    ///
    /// The command runs as [`execute`](Tool::execute) runs it — the same launcher,
    /// environment, standard input, session and process group — with no timeout: the
    /// caller decides when to [`kill`](RunningCommand::kill) it. Its output streams
    /// through `ctx` while `ctx` listens and is kept as `execute` keeps it;
    /// [`RunningCommand::keep_output`] sends all of it to a file. Dropping the
    /// [`RunningCommand`] kills the command's processes.
    ///
    /// # Errors
    ///
    /// Returns `BashDisabled` when the workspace does not allow `bash`, or the error that
    /// kept the command from starting.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut running = bash.spawn(ctx, "cargo build").await?;
    /// let exit_code = running.wait().await?;
    /// let result = running.result(exit_code);
    /// ```
    pub async fn spawn(&self, ctx: Arc<dyn ToolContext>, command: &str) -> Result<RunningCommand> {
        if !self.workspace.bash_allowed() {
            return Err(DevToolError::BashDisabled.into());
        }
        let kill_group_on_exit = self.workspace.kills_bash_group_on_exit();
        let (child, group, stdout, stderr) = self.start(&ctx, command, kill_group_on_exit).await?;
        let pid = child.id();
        tracing::debug!(
            bash.isolation = self.launcher.isolation(),
            process.pid = pid,
            "started bash command"
        );
        let output = Arc::new(Collector::new(
            self.output_store.clone(),
            ctx.clone(),
            command,
            self.workspace.max_output(),
        ));
        let pumps = [(stdout, Stream::Stdout), (stderr, Stream::Stderr)]
            .into_iter()
            .filter_map(|(pipe, stream)| {
                pipe.map(|pipe| tokio::spawn(pump(pipe, stream, output.clone(), ctx.clone())))
            })
            .collect();
        let kill = Arc::new(Notify::new());
        #[cfg(windows)]
        let job = group.job.clone();
        let done = tokio::spawn(supervise(child, group, kill_group_on_exit, pumps, kill.clone()));
        Ok(RunningCommand {
            command: command.to_string(),
            pid,
            output,
            kill,
            done,
            exit: None,
            started: Instant::now(),
            #[cfg(windows)]
            job,
        })
    }

    /// Starts the process for `script`: the launcher's own on Windows when it contains one
    /// (spec 3.2 §10 W5), else the one [`spawn_process`](Self::spawn_process) builds.
    async fn start(
        &self,
        ctx: &Arc<dyn ToolContext>,
        script: &str,
        kill_group_on_exit: bool,
    ) -> Result<(Running, ProcessGroupGuard, Option<Pipe>, Option<Pipe>)> {
        // A launcher may start a confined process itself (spec 3.2 §10 W5); starting one
        // can take long, so it runs on the blocking pool.
        #[cfg(windows)]
        {
            let (launcher, ws, script, call) =
                (self.launcher.clone(), self.workspace.clone(), script.to_string(), ctx.clone());
            let contained = tokio::task::spawn_blocking(move || {
                launcher.spawn_contained(&ws, &script, &[], call.as_ref())
            })
            .await
            .map_err(|error| {
                DevToolError::Other(format!("the command could not be started: {error}"))
            })?;
            if let Some(contained) = contained {
                let ContainedChild { job, process, stdin, stdout, stderr, .. } = contained?;
                // Standard input reads end of file, as `Stdio::null()` gives a spawned one.
                drop(stdin);
                let pipe =
                    |file: std::fs::File| -> Pipe { Box::new(tokio::fs::File::from_std(file)) };
                return Ok((
                    Running::Contained(process),
                    ProcessGroupGuard {
                        job: Some(Arc::new(job)),
                        keep_on_exit: !kill_group_on_exit,
                    },
                    stdout.map(pipe),
                    stderr.map(pipe),
                ));
            }
        }
        self.spawn_process(script, ctx.as_ref(), kill_group_on_exit)
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

        let mut running = self.spawn(ctx.clone(), &command).await?;
        match tokio::time::timeout(timeout, running.wait()).await {
            Ok(Ok(exit_code)) => Ok(running.result(exit_code)),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                running.kill_and_wait().await;
                ctx.emit_progress("stderr", &format!("\n[timeout after {}s]\n", timeout.as_secs()))
                    .await;
                Err(DevToolError::Timeout(timeout).into())
            }
        }
    }
}

/// A command [`BashTool::spawn`] started (spec 3.1 §10 D10).
///
/// [`wait`](Self::wait) for it to exit, then read its [`result`](Self::result); or
/// [`kill`](Self::kill) it. Dropping it kills the command's processes, so a caller that
/// keeps a command running — a background shell — keeps this value.
pub struct RunningCommand {
    command: String,
    pid: Option<u32>,
    output: Arc<Collector>,
    kill: Arc<Notify>,
    done: JoinHandle<std::io::Result<Option<i32>>>,
    /// The exit code once [`wait`](Self::wait) has seen it.
    exit: Option<Option<i32>>,
    started: Instant,
    /// The command's Job Object, shared with its supervising task.
    #[cfg(windows)]
    job: Option<Arc<JobObject>>,
}

impl std::fmt::Debug for RunningCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningCommand")
            .field("command", &self.command)
            .field("pid", &self.pid)
            .field("exit", &self.exit)
            .finish_non_exhaustive()
    }
}

impl RunningCommand {
    /// The command as it was started.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// The process id of the shell, which leads the command's process group on Unix; `None`
    /// when the platform gave none.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// How long the command has run.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Waits for the command to exit and its output to end; its exit code, `None` when a
    /// signal ended it. Cancel-safe: a `wait` dropped before it returns can be called again.
    ///
    /// # Errors
    ///
    /// Returns the error from waiting on the process.
    pub async fn wait(&mut self) -> Result<Option<i32>> {
        if let Some(exit) = self.exit {
            return Ok(exit);
        }
        let exit = match (&mut self.done).await {
            Ok(Ok(exit)) => exit,
            Ok(Err(error)) => return Err(DevToolError::from(error).into()),
            Err(error) => {
                return Err(DevToolError::Other(format!(
                    "the command's supervision ended unexpectedly: {error}"
                ))
                .into());
            }
        };
        self.exit = Some(exit);
        Ok(exit)
    }

    /// Whether the command has exited and been waited for.
    pub fn is_finished(&self) -> bool {
        self.exit.is_some() || self.done.is_finished()
    }

    /// Kills the command's processes; [`wait`](Self::wait) returns soon after.
    pub fn kill(&self) {
        self.kill.notify_one();
    }

    /// Kills the command's processes and waits, briefly, for them to be reaped.
    pub async fn kill_and_wait(&mut self) {
        self.kill();
        let _ = tokio::time::timeout(KILL_GRACE * 2, self.wait()).await;
    }

    /// Bytes the command has written to stdout and stderr so far.
    pub fn output_bytes(&self) -> u64 {
        self.output.total()
    }

    /// The last line the command has written, at most 200 characters.
    pub fn last_line(&self) -> Option<String> {
        self.output.last_line()
    }

    /// Sends the command's output to a file from the output store, from its first byte and
    /// whatever the result needs, and returns the file's id.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool has no output store or the store cannot create the
    /// file.
    pub fn keep_output(&self) -> std::io::Result<String> {
        self.output.keep()
    }

    /// The result of the call: `{ command, exit_code, stdout, stderr, truncated }`, plus
    /// `output_id` when the output was cut and an output store keeps the whole of it.
    pub fn result(&self, exit_code: Option<i32>) -> Value {
        let rendered = self.output.render();
        let mut result = json!({
            "command": self.command,
            "exit_code": exit_code,
            "stdout": rendered.stdout,
            "stderr": rendered.stderr,
            "truncated": rendered.truncated,
        });
        if let Some(id) = rendered.output_id {
            result["output_id"] = Value::String(id);
        }
        result
    }
}

impl Drop for RunningCommand {
    fn drop(&mut self) {
        if self.exit.is_none() {
            self.kill.notify_one();
            // The supervising task may not run again before the caller blocks or the
            // runtime stops, so a dropped call ends its job here, waiting as the guard does.
            #[cfg(windows)]
            if !self.done.is_finished()
                && let Some(job) = &self.job
            {
                note_termination(job.terminate_and_wait(JobObject::TERMINATION_WAIT));
            }
        }
    }
}

/// Supervises one command: kills the rest of its group once the shell exits, waits for
/// its output to end, and reaps it; or kills it on request. Returns the exit code.
async fn supervise(
    mut child: Running,
    mut group: ProcessGroupGuard,
    kill_group_on_exit: bool,
    pumps: Vec<JoinHandle<()>>,
    kill: Arc<Notify>,
) -> std::io::Result<Option<i32>> {
    let mut killed = false;
    if kill_group_on_exit {
        // The shell has exited but is not reaped, so its pid, which is the group id,
        // cannot be reused while the rest of the group is killed. That also closes the
        // output pipes a background process holds open.
        #[cfg(unix)]
        let exited = wait_for_exit(child.id());
        // The job, not the pid, names the processes, so the shell can be reaped.
        #[cfg(windows)]
        let exited = async { child.wait().await.is_ok() };
        #[cfg(not(any(unix, windows)))]
        let exited = async { false };
        tokio::select! {
            shell_exited = exited => {
                if shell_exited {
                    group.kill().await;
                }
            }
            () = kill.notified() => killed = true,
        }
    }
    let aborts: Vec<_> = pumps.iter().map(JoinHandle::abort_handle).collect();
    let output_ended = async {
        for pump in pumps {
            let _ = pump.await;
        }
    };
    tokio::pin!(output_ended);
    if !killed {
        tokio::select! {
            () = &mut output_ended => {}
            () = kill.notified() => killed = true,
        }
    }
    if killed {
        group.kill().await;
        // Also signal the child directly, which is all that is available elsewhere.
        child.start_kill();
        if tokio::time::timeout(KILL_GRACE, &mut output_ended).await.is_err() {
            for abort in aborts {
                abort.abort();
            }
        }
    }
    let exit_code = child.wait().await?;
    // The child is reaped, so its pid may be reused and must not be signalled.
    group.disarm();
    Ok(exit_code)
}

impl BashTool {
    /// Spawns the launcher's command for `script`: standard input at end of file, output
    /// piped, in a new session on Unix and in a Job Object on Windows.
    fn spawn_process(
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
            job: Some(Arc::new(contain(&child).map_err(|error| {
                DevToolError::Other(format!(
                    "the command could not be placed in a Job Object, which stops the \
                     processes it starts, so it was not run: {error}"
                ))
            })?)),
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
    job: Option<Arc<JobObject>>,
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
        // The guard's handle closes here; the job's last one kills what is left in it
        // unless the workspace keeps background processes.
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

/// Reads one output stream to its end into `output`, forwarding complete lines as
/// progress through `ctx` (spec 3.1 §10 D10).
///
/// Every byte is read, so a writer never blocks on a full pipe; `output` keeps the start
/// and end in memory and the rest in its file, if any.
async fn pump(mut pipe: Pipe, stream: Stream, output: Arc<Collector>, ctx: Arc<dyn ToolContext>) {
    let mut chunk = [0u8; 8192];
    // Output read but not yet forwarded: the partial line after the last newline.
    let mut line = Vec::new();
    loop {
        let read = match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        output.push(stream, &chunk[..read]);
        line.extend_from_slice(&chunk[..read]);
        let end = match line.iter().rposition(|&byte| byte == b'\n') {
            Some(newline) => newline + 1,
            None if line.len() > PROGRESS_LINE_MAX => line.len(),
            None => continue,
        };
        let rest = line.split_off(end);
        ctx.emit_progress(stream.name(), &String::from_utf8_lossy(&line)).await;
        line = rest;
    }
    if !line.is_empty() {
        ctx.emit_progress(stream.name(), &String::from_utf8_lossy(&line)).await;
    }
}
