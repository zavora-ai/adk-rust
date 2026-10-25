//! Process groups that hold a process and every process it starts (spec 3.2 §10 W1, K-I5).
//!
//! A Unix process tree is held in a session and process group of its own: the process is
//! started as the leader of a new session ([`ProcessGroup::new_session`]), every process it
//! starts belongs to the group, and a signal sent to the group reaches all of them, the
//! descendants of a leader that has already exited included. [`ProcessGroup`] is the Unix
//! counterpart of the Windows `JobObject` this module holds there: the stdio servers of
//! `adk-tool`'s `McpServerManager`, the kernel's drivers among them, end this way, and the
//! end returns only once every process of the group has exited.
//!
//! A process group is not a boundary the way a Job Object is. A process that starts a
//! session or group of its own (`setsid`, `setpgid`) leaves the group, and nothing ends the
//! group when its owner is killed outright, since the owner then runs no cleanup.

use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How often a terminating group is asked whether its processes have exited.
const TERMINATION_POLL: Duration = Duration::from_millis(5);

/// A process group whose leader the group holds, which ends as a whole (spec 3.2 §10 W1,
/// K-I5).
///
/// Start the process with [`new_session`](Self::new_session) and hand the spawned child to
/// [`contain`](Self::contain). The group keeps the leader unreaped until it ends the group,
/// so the leader's process id, which is the group's id, cannot be reused while the group is
/// signalled. [`terminate_and_wait`](Self::terminate_and_wait) sends the group `SIGTERM`,
/// then `SIGKILL`, and returns once every process in it has exited;
/// [`terminate`](Self::terminate) sends `SIGKILL` at once. Dropping the group sends
/// `SIGKILL` to whatever is left in it.
///
/// # Example
///
/// ```rust,no_run
/// use adk_sandbox::process_tree::ProcessGroup;
///
/// # async fn run() -> std::io::Result<()> {
/// let mut command = tokio::process::Command::new("sh");
/// command.args(["-c", "sleep 100 & sleep 100"]);
/// ProcessGroup::new_session(&mut command);
/// let group = ProcessGroup::contain(command.spawn()?)?;
/// // The `sleep` the shell left in the background ends here too.
/// assert!(group.terminate_and_wait_async(ProcessGroup::TERMINATION_WAIT).await?);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct ProcessGroup {
    /// The group's id: its leader's process id.
    id: libc::pid_t,
    /// The leader until the group reaps it; while it is held, its id names this group only.
    leader: Mutex<Option<tokio::process::Child>>,
}

impl ProcessGroup {
    /// How long [`terminate_and_wait`](Self::terminate_and_wait) callers wait by default for
    /// the group's processes to exit: a process that handles `SIGTERM` gets up to half of
    /// it, and `SIGKILL` ends the rest in milliseconds.
    pub const TERMINATION_WAIT: Duration = Duration::from_secs(2);

    /// Makes `command` start its process as the leader of a new session, and so of a new
    /// process group whose id is the process's own, with no controlling terminal.
    ///
    /// Without a controlling terminal the process can neither read from nor inject input
    /// into the owner's terminal, and the terminal's signals, such as Ctrl-C's `SIGINT`, do
    /// not reach it: its owner ends it. A command that already makes its process a group
    /// leader, with `process_group(0)`, keeps that group and its session.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_sandbox::process_tree::ProcessGroup;
    ///
    /// let mut command = tokio::process::Command::new("my-mcp-server");
    /// ProcessGroup::new_session(&mut command).kill_on_drop(true);
    /// ```
    pub fn new_session(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
        // SAFETY: the closure runs in the forked child before `exec` and calls only `setsid`,
        // `getpgrp` and `getpid`, which are async-signal-safe, and reads `errno`; it touches
        // no memory of the parent.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    let error = io::Error::last_os_error();
                    // `setsid` refuses a process that already leads its group, which
                    // `process_group(0)` makes it before this runs.
                    if libc::getpgrp() != libc::getpid() {
                        return Err(error);
                    }
                }
                Ok(())
            })
        }
    }

    /// Holds `child`, which [`new_session`](Self::new_session) started, as the leader of its
    /// group.
    ///
    /// # Errors
    ///
    /// Returns an error when the child has already been reaped or leads no process group of
    /// its own, as a command spawned without [`new_session`](Self::new_session) does. The
    /// child is then sent `SIGKILL`, since nothing could end what it starts.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_sandbox::process_tree::ProcessGroup;
    ///
    /// # fn run() -> std::io::Result<()> {
    /// let mut command = tokio::process::Command::new("my-mcp-server");
    /// ProcessGroup::new_session(&mut command);
    /// let group = ProcessGroup::contain(command.spawn()?)?;
    /// assert!(group.id() > 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn contain(mut child: tokio::process::Child) -> io::Result<Self> {
        let id = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok()).ok_or_else(|| {
            io::Error::other("the process was reaped before its process group could be held")
        })?;
        // SAFETY: `getpgid` takes a process id and has no memory-safety preconditions.
        let group = unsafe { libc::getpgid(id) };
        // A leader that has already exited is unreaped, and some systems report no group for
        // it; its id stays reserved all the same.
        let exited = group == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if id <= 1 || (group != id && !exited) {
            let _ = child.start_kill();
            return Err(io::Error::other(format!(
                "process {id} leads no process group of its own, so the processes it starts \
                 could not be ended with it; start it with ProcessGroup::new_session"
            )));
        }
        Ok(Self { id, leader: Mutex::new(Some(child)) })
    }

    /// The group's id, which is its leader's process id.
    pub fn id(&self) -> u32 {
        self.id.unsigned_abs()
    }

    /// Sends `SIGKILL` to every process in the group, the leader included.
    ///
    /// Termination is asynchronous: a process can still complete I/O it had in flight, such
    /// as a write to a file, after this returns.
    /// [`terminate_and_wait`](Self::terminate_and_wait) returns only once every process has
    /// exited. A group whose leader it has already reaped is not signalled, since its id may
    /// then name another group.
    pub fn terminate(&self) {
        let leader = self.leader();
        if leader.is_some() {
            signal(self.id, libc::SIGKILL);
        }
    }

    /// Waits, for at most `limit`, until the group's leader has exited, without reaping it,
    /// and returns whether it has. The rest of the group may still run.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the leader's state cannot be queried.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::time::Duration;
    /// use adk_sandbox::process_tree::ProcessGroup;
    ///
    /// # async fn run(group: ProcessGroup) -> std::io::Result<()> {
    /// if !group.wait_for_leader(Duration::from_secs(3)).await? {
    ///     group.terminate_and_wait_async(ProcessGroup::TERMINATION_WAIT).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_for_leader(&self, limit: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + limit;
        loop {
            let exited = match self.leader().as_ref() {
                Some(_) => leader_exited(self.id)?,
                None => true,
            };
            if exited {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(TERMINATION_POLL).await;
        }
    }

    /// Ends the group and waits, for at most `limit`, until each of its processes has exited,
    /// so nothing a process had in flight lands after the call returns. Returns whether every
    /// process has exited. The wait blocks the thread;
    /// [`terminate_and_wait_async`](Self::terminate_and_wait_async) is its async form.
    ///
    /// The group is sent `SIGTERM`, and `SIGKILL` once its leader has exited or half of
    /// `limit` has passed. The leader is then reaped, and the call returns once no process of
    /// the group is left, including descendants the leader left behind, which the system
    /// reaps once they exit. A second call only waits.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the leader cannot be waited for.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_sandbox::process_tree::ProcessGroup;
    ///
    /// # fn run(group: ProcessGroup) -> std::io::Result<()> {
    /// assert!(group.terminate_and_wait(ProcessGroup::TERMINATION_WAIT)?);
    /// # Ok(())
    /// # }
    /// ```
    pub fn terminate_and_wait(&self, limit: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + limit;
        let mut termination = Termination::begin(self, limit / 2);
        loop {
            if termination.finished()? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(TERMINATION_POLL);
        }
    }

    /// [`terminate_and_wait`](Self::terminate_and_wait) that sleeps on the Tokio timer
    /// instead of blocking the thread.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the leader cannot be waited for.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_sandbox::process_tree::ProcessGroup;
    ///
    /// # async fn run(group: ProcessGroup) -> std::io::Result<()> {
    /// assert!(group.terminate_and_wait_async(ProcessGroup::TERMINATION_WAIT).await?);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn terminate_and_wait_async(&self, limit: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + limit;
        let mut termination = Termination::begin(self, limit / 2);
        loop {
            if termination.finished()? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(TERMINATION_POLL).await;
        }
    }

    /// The leader, `None` once the group has reaped it. Signals are sent under this lock, so
    /// the leader cannot be reaped while one is on its way.
    fn leader(&self) -> MutexGuard<'_, Option<tokio::process::Child>> {
        self.leader.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // The leader is then dropped unreaped, and Tokio reaps it once it exits.
        if self.leader.get_mut().unwrap_or_else(PoisonError::into_inner).is_some() {
            signal(self.id, libc::SIGKILL);
        }
    }
}

/// A group being ended: `SIGTERM` sent, `SIGKILL` once the leader has exited or the grace
/// has passed, then the leader reaped and the rest of the group waited for.
struct Termination<'a> {
    group: &'a ProcessGroup,
    /// When `SIGKILL` follows a `SIGTERM` the leader has not exited on.
    kill_at: Instant,
    /// Whether `SIGKILL` has been sent.
    killed: bool,
}

impl<'a> Termination<'a> {
    fn begin(group: &'a ProcessGroup, grace: Duration) -> Self {
        let leader = group.leader();
        if leader.is_some() {
            signal(group.id, libc::SIGTERM);
        }
        Self { group, kill_at: Instant::now() + grace, killed: leader.is_none() }
    }

    /// Whether every process of the group has exited, advancing the termination.
    fn finished(&mut self) -> io::Result<bool> {
        let mut leader = self.group.leader();
        if let Some(child) = leader.as_mut() {
            if !self.killed {
                if !leader_exited(self.group.id)? && Instant::now() < self.kill_at {
                    return Ok(false);
                }
                signal(self.group.id, libc::SIGKILL);
                self.killed = true;
            }
            if child.try_wait()?.is_none() {
                return Ok(false);
            }
            *leader = None;
        }
        drop(leader);
        // A member the system has yet to reap still counts: it may not have finished exiting.
        // SAFETY: `kill` has no memory-safety preconditions; signal 0 only checks that a
        // process of the group exists.
        let probed = unsafe { libc::kill(-self.group.id, 0) };
        Ok(probed == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH))
    }
}

/// Sends `signal` to the group `id`. The caller holds the group's unreaped leader, so `id`
/// names no other group.
fn signal(id: libc::pid_t, signal: libc::c_int) {
    // SAFETY: `killpg` takes a group id and a signal and cannot violate memory safety; `id` is
    // greater than 1, so it never names the caller's own group or every process. A failure
    // means no process of the group is left to signal.
    unsafe {
        libc::killpg(id, signal);
    }
}

/// Whether the unreaped child `id` has exited. `waitid` with `WNOWAIT` leaves it unreaped,
/// so its id stays reserved.
fn leader_exited(id: libc::pid_t) -> io::Result<bool> {
    loop {
        // SAFETY: all zeroes is a valid `siginfo_t`, which `waitid` writes only into; a zero
        // `si_pid` afterwards means the child has not exited.
        let (waited, pid) = unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let waited = libc::waitid(
                libc::P_PID,
                id.unsigned_abs(),
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            );
            (waited, info.si_pid())
        };
        if waited == 0 {
            return Ok(pid != 0);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => {}
            // Not a child of this process any more: it has exited and been reaped.
            Some(libc::ECHILD) => return Ok(true),
            _ => return Err(error),
        }
    }
}
