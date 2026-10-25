//! Job Objects that hold a process and every process it starts (spec 3.2 §10 W1).
//!
//! A Windows process has no process group: ending one leaves the processes it started
//! running. A [Job Object](https://learn.microsoft.com/windows/win32/procthread/job-objects)
//! holds a process tree instead. A process created in a job, or added to it before it runs,
//! cannot start anything outside it, so terminating the job ends the whole tree. The jobs
//! here also end their processes when their last handle closes, so a tree never outlives
//! its owner — including when the owning process exits without cleaning up.
//!
//! The devtools `bash` tool, the stdio servers of `adk-tool`'s `McpServerManager` (the
//! kernel's drivers) and the restricted-token enforcer's `WindowsEnforcer::run_confined` and
//! `WindowsEnforcer::spawn_contained` hold their processes this way. A process the
//! enforcer starts itself, outside `std::process`, comes back as a [`ContainedChild`]
//! (spec 3.2 §10 W5).

use std::io;
use std::os::windows::io::{
    AsHandle, AsRawHandle as _, BorrowedHandle, FromRawHandle as _, OwnedHandle,
};
use std::sync::Arc;

use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, INFINITE, OpenProcess, OpenThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    ResumeThread, THREAD_SUSPEND_RESUME, WaitForSingleObject,
};

/// The process creation flag that starts a process suspended, so
/// [`JobObject::contain_suspended`] can place it in a job before it runs.
///
/// # Example
///
/// ```rust,no_run
/// use adk_sandbox::process_tree::{CREATE_SUSPENDED, JobObject};
///
/// # async fn run() -> std::io::Result<()> {
/// let mut command = tokio::process::Command::new("cmd.exe");
/// command.args(["/c", "echo hi"]).creation_flags(CREATE_SUSPENDED).kill_on_drop(true);
/// let mut child = command.spawn()?;
/// let job = JobObject::contain_suspended(child.id().expect("the child has not been reaped"))?;
/// child.wait().await?;
/// drop(job);
/// # Ok(())
/// # }
/// ```
pub const CREATE_SUSPENDED: u32 = windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

/// A Job Object that kills every process in it when its last handle closes (spec 3.2 §10 W1).
///
/// Create the process suspended ([`CREATE_SUSPENDED`]) and hand it to
/// [`contain_suspended`](Self::contain_suspended), which places it in a new job and only then
/// lets it run: every process it starts is created in the job and none can leave it.
/// [`terminate`](Self::terminate) kills the tree at once; dropping the job kills what is left
/// in it, unless [`kill_on_close(false)`](Self::kill_on_close) released it.
///
/// # Example
///
/// ```rust,no_run
/// use adk_sandbox::process_tree::{CREATE_SUSPENDED, JobObject};
///
/// # async fn run() -> std::io::Result<()> {
/// let mut command = tokio::process::Command::new("cmd.exe");
/// command.args(["/c", "start /b ping -n 100 127.0.0.1"]).creation_flags(CREATE_SUSPENDED);
/// let mut child = command.spawn()?;
/// let job = JobObject::contain_suspended(child.id().expect("the child has not been reaped"))?;
/// child.wait().await?;
/// // `ping`, which the shell left running, ends here with the job.
/// job.terminate();
/// # Ok(())
/// # }
/// ```
pub struct JobObject {
    handle: OwnedHandle,
    /// Dropped after `handle`, so they outlive every process the job's close ends.
    companions: Companions,
}

impl std::fmt::Debug for JobObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobObject")
            .field("handle", &self.handle)
            .field("companions", &self.companions.items.len())
            .finish()
    }
}

/// What must live as long as a job's processes, such as the restricted-token enforcer's
/// keepers of MSYS2 namespaces (spec 3.2 §10 W5).
#[derive(Default)]
struct Companions {
    items: Vec<Box<dyn std::any::Any + Send + Sync>>,
    /// Set by `kill_on_close(false)`: the processes may outlive the job's handle, so the
    /// companions are kept for the life of this process instead of dropped.
    released: std::sync::atomic::AtomicBool,
}

impl Drop for Companions {
    fn drop(&mut self) {
        if *self.released.get_mut() {
            for item in self.items.drain(..) {
                std::mem::forget(item);
            }
        }
    }
}

impl JobObject {
    /// Creates an empty job that kills its processes when its last handle closes.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the job cannot be created or configured.
    pub fn new() -> io::Result<Self> {
        // SAFETY: both arguments may be null: no security attributes and no name.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `handle` was just created and nothing else owns it.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let job = Self { handle, companions: Companions::default() };
        job.kill_on_close(true)?;
        Ok(job)
    }

    /// Places the suspended process `pid` in a new job, then resumes its threads.
    ///
    /// The process must have been created with [`CREATE_SUSPENDED`] and still be held by its
    /// creator, whose handle keeps `pid` from being reused. It runs only once it is in the
    /// job, so nothing it starts is outside it.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the process cannot be opened, the job cannot be created, the
    /// process cannot be added to it, or its threads cannot be resumed. The process is then
    /// left suspended, or ended with the job when it was already added; the caller ends it.
    pub fn contain_suspended(pid: u32) -> io::Result<Self> {
        // SAFETY: the result is checked; the rights are the two `AssignProcessToJobObject` needs.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `process` was just opened and nothing else owns it.
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        let job = Self::new()?;
        job.assign(process.as_handle())?;
        resume_threads(pid)?;
        Ok(job)
    }

    /// Adds `process` to the job; every process it starts from then on belongs to the job too.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the process cannot be added, for example when it has exited
    /// or the handle lacks `PROCESS_SET_QUOTA` and `PROCESS_TERMINATE`.
    pub fn assign(&self, process: BorrowedHandle<'_>) -> io::Result<()> {
        // SAFETY: both handles are open for the call.
        if unsafe { AssignProcessToJobObject(self.handle.as_raw_handle(), process.as_raw_handle()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Keeps `companion` until the job's processes end: it is dropped after the job's handle
    /// closes, or kept for the life of this process once `kill_on_close(false)` let them
    /// outlive the job.
    #[cfg(feature = "sandbox-windows")]
    pub(crate) fn attach(&mut self, companion: Box<dyn std::any::Any + Send + Sync>) {
        self.companions.items.push(companion);
    }

    /// Kills every process in the job. A job with no process left is unaffected.
    pub fn terminate(&self) {
        // SAFETY: the handle is open. A failure means the job has no process left.
        unsafe {
            TerminateJobObject(self.handle.as_raw_handle(), 1);
        }
    }

    /// Sets whether closing the job's last handle kills its processes; `true` on a new job.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the job's limits cannot be set.
    pub fn kill_on_close(&self, kill: bool) -> io::Result<()> {
        self.companions.released.store(!kill, std::sync::atomic::Ordering::SeqCst);
        // SAFETY: the structure is plain data for which all zeroes is valid: no limits.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        if kill {
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        // SAFETY: `limits` is readable and the size passed is its own; the handle is open.
        let set = unsafe {
            SetInformationJobObject(
                self.handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }
}

impl AsHandle for JobObject {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

/// A running process held in a Job Object of its own, with its standard streams on pipes,
/// as `WindowsEnforcer::spawn_contained` starts it (spec 3.2 §10 W1, W5).
///
/// Dropping the child closes the job, which ends the process and everything it started; so
/// does [`JobObject::terminate`] on `job`. Its standard input is the write end of a pipe the
/// process reads: drop it for end of file. Read `stdout` and `stderr` to their end of file,
/// which comes once every process holding them has exited.
///
/// # Example
///
/// ```rust,no_run
/// use adk_sandbox::process_tree::ContainedChild;
///
/// # async fn run(child: ContainedChild) -> std::io::Result<()> {
/// let ContainedChild { job, process, stdin, .. } = child;
/// drop(stdin);
/// let code = process.wait().await?;
/// job.terminate();
/// println!("exited with {code}");
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct ContainedChild {
    /// The job holding the process and every process it starts.
    pub job: JobObject,
    /// The process, to wait for.
    pub process: ContainedProcess,
    /// The write end of the process's standard input.
    pub stdin: Option<std::fs::File>,
    /// The read end of the process's standard output.
    pub stdout: Option<std::fs::File>,
    /// The read end of the process's standard error.
    pub stderr: Option<std::fs::File>,
}

/// A process started outside `std::process`, which can be waited for (spec 3.2 §10 W5).
///
/// Clones wait for the same process.
#[derive(Debug, Clone)]
pub struct ContainedProcess {
    handle: Arc<OwnedHandle>,
    pid: u32,
}

impl ContainedProcess {
    /// The process `handle`, which must allow `SYNCHRONIZE` and
    /// `PROCESS_QUERY_LIMITED_INFORMATION`, with id `pid`.
    #[cfg(feature = "sandbox-windows")]
    pub(crate) fn new(handle: OwnedHandle, pid: u32) -> Self {
        Self { handle: Arc::new(handle), pid }
    }

    /// The process id.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Waits for the process to exit and returns its exit code.
    ///
    /// The wait blocks a thread of Tokio's blocking pool, so it must be called inside a
    /// Tokio runtime. Dropping the future leaves that thread waiting until the process exits,
    /// which terminating its job makes immediate.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the wait or the exit-code query fails.
    pub async fn wait(&self) -> io::Result<i32> {
        let handle = Arc::clone(&self.handle);
        tokio::task::spawn_blocking(move || {
            // SAFETY: the handle is open while `handle` lives.
            if unsafe { WaitForSingleObject(handle.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
                return Err(io::Error::last_os_error());
            }
            exit_code(&handle)
        })
        .await
        .map_err(io::Error::other)?
    }

    /// The exit code when the process has exited, `None` while it runs.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the state or the exit code cannot be queried.
    pub fn try_wait(&self) -> io::Result<Option<i32>> {
        // SAFETY: the handle is open while `self` lives.
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => exit_code(&self.handle).map(Some),
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

/// The exit code of the exited process `handle`, with Windows' `u32` read as `i32` as
/// `std::process::ExitStatus::code` does.
fn exit_code(handle: &OwnedHandle) -> io::Result<i32> {
    let mut code: u32 = 0;
    // SAFETY: the handle is open and `code` is writable.
    if unsafe { GetExitCodeProcess(handle.as_raw_handle(), &raw mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(code as i32)
}

/// Resumes the threads of the suspended process `pid`, which has only the primary thread it
/// was created with.
fn resume_threads(pid: u32) -> io::Result<()> {
    // SAFETY: a thread snapshot takes no process id; the result is checked below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `snapshot` was just opened and nothing else owns it.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    // SAFETY: the structure is plain data for which all zeroes is valid; its size is set next.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
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
        Err(io::Error::other("the suspended process's thread was not found to start it"))
    }
}
