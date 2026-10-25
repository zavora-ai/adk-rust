//! A Job Object holds a process and everything it starts (spec 3.2 §10 W1).
//!
//! Each test starts `cmd.exe` suspended, contains it, and lets it start `ping` in the
//! background, which on its own would outlive the shell. They run on the PR tier's `windows`
//! job.
#![cfg(all(windows, feature = "process-tree"))]

use std::os::windows::io::{AsHandle as _, AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::time::{Duration, Instant};

use adk_sandbox::process_tree::{CREATE_SUSPENDED, JobObject};
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::System::JobObjects::{
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JobObjectBasicProcessIdList, QueryInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

/// Starts a shell that leaves `ping` running in the background and then waits on its own
/// `ping`, both for five minutes; returns the shell and its job.
fn contained_shell() -> (tokio::process::Child, JobObject) {
    let mut command = tokio::process::Command::new("cmd.exe");
    command
        .args(["/c", "start /b ping -n 300 127.0.0.1 >nul & ping -n 300 127.0.0.1 >nul"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_SUSPENDED)
        .kill_on_drop(true);
    let child = command.spawn().expect("cmd.exe starts");
    let job = JobObject::contain_suspended(child.id().expect("the shell runs"))
        .expect("the suspended shell is contained");
    (child, job)
}

/// The processes in `job`, once it holds at least `count`, for at most twenty seconds.
fn processes(job: &JobObject, count: usize) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        // Room for 64 process ids after the header.
        let mut buffer = [0usize; 65];
        // SAFETY: `buffer` is writable for its own size and holds the list's header; the
        // job's handle is open.
        let queried = unsafe {
            QueryInformationJobObject(
                job.as_handle().as_raw_handle(),
                JobObjectBasicProcessIdList,
                buffer.as_mut_ptr().cast(),
                size_of_val(&buffer) as u32,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(queried, 0, "{}", std::io::Error::last_os_error());
        // SAFETY: the call filled the header and as many ids as it reports.
        let list = unsafe { &*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
        // The header is two `u32` counts, so the ids start at the list's own offset, not
        // after two `usize`s; reading from the wrong slot yields a stray 0 as a process id.
        let first = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList)
            / size_of::<usize>();
        let ids: Vec<u32> = buffer[first..first + list.NumberOfProcessIdsInList as usize]
            .iter()
            .map(|id| *id as u32)
            .collect();
        if ids.len() >= count || Instant::now() > deadline {
            return ids;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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

/// Ends the process `pid`, should a test leave it running.
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
async fn terminate_ends_every_process_the_shell_started() {
    let (_shell, job) = contained_shell();
    // The shell and its two `ping`s.
    let ids = processes(&job, 3);
    assert!(ids.len() >= 3, "the job holds {ids:?}");

    job.terminate();

    let left: Vec<u32> =
        ids.iter().copied().filter(|pid| !ends_within(*pid, Duration::from_secs(10))).collect();
    ids.iter().copied().for_each(terminate);
    assert!(left.is_empty(), "{left:?} outlived the terminated job");
}

#[tokio::test]
async fn closing_the_job_ends_the_whole_tree() {
    let (mut shell, job) = contained_shell();
    let ids = processes(&job, 3);
    assert!(ids.len() >= 3, "the job holds {ids:?}");

    drop(job);

    let left: Vec<u32> =
        ids.iter().copied().filter(|pid| !ends_within(*pid, Duration::from_secs(10))).collect();
    ids.iter().copied().for_each(terminate);
    assert!(left.is_empty(), "{left:?} outlived the job");
    let status = tokio::time::timeout(Duration::from_secs(10), shell.wait()).await;
    assert!(matches!(status, Ok(Ok(_))), "the shell ended: {status:?}");
}

#[tokio::test]
async fn a_released_job_leaves_its_processes_running() {
    let (_shell, job) = contained_shell();
    let ids = processes(&job, 3);
    assert!(ids.len() >= 3, "the job holds {ids:?}");
    job.kill_on_close(false).expect("the limit is cleared");
    let handle = job.as_handle().try_clone_to_owned().expect("the handle is duplicated");

    drop(job);

    let ended: Vec<u32> =
        ids.iter().copied().filter(|pid| ends_within(*pid, Duration::from_secs(2))).collect();
    assert!(ended.is_empty(), "{ended:?} ended with a released job");

    // Clean up through a second handle to the same job.
    // SAFETY: the duplicated handle is open and owned here.
    unsafe {
        windows_sys::Win32::System::JobObjects::TerminateJobObject(handle.as_raw_handle(), 1);
    }
    ids.iter().copied().for_each(terminate);
}

/// Terminating the job and waiting returns only once every process in it has exited, not
/// only left the job's count, so a write the background process had in flight lands before
/// the call returns, not after.
#[tokio::test]
async fn terminate_and_wait_returns_once_no_process_is_left() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tick = dir.path().join("tick.txt");
    let mut command = tokio::process::Command::new("cmd.exe");
    command
        .current_dir(dir.path())
        .args([
            "/d",
            "/c",
            "start /b cmd /d /c for /l %i in (1,1,2000000000) do @echo x^>^> tick.txt& \
             ping -n 300 127.0.0.1 >nul",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_SUSPENDED)
        .kill_on_drop(true);
    let shell = command.spawn().expect("cmd.exe starts");
    let job = JobObject::contain_suspended(shell.id().expect("the shell runs"))
        .expect("the suspended shell is contained");
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::fs::metadata(&tick).map_or(0, |meta| meta.len()) == 0 {
        assert!(Instant::now() < deadline, "the background writer never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(job.active_processes().expect("the job is queried") >= 2);
    // The shell and the writer, held open so each handle shows whether its process has exited.
    let held: Vec<OwnedHandle> = processes(&job, 2)
        .into_iter()
        .filter_map(|pid| {
            // SAFETY: plain arguments; the result is checked.
            let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            // SAFETY: an open handle is owned here alone.
            (!process.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(process) })
        })
        .collect();
    assert!(held.len() >= 2, "{} of the job's processes could be opened", held.len());

    assert!(job.terminate_and_wait(JobObject::TERMINATION_WAIT).expect("the job is queried"));

    assert_eq!(job.active_processes().expect("the job is queried"), 0);
    let running = held
        .iter()
        // SAFETY: each handle is open while `held` lives.
        .filter(
            |process| unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } != WAIT_OBJECT_0,
        )
        .count();
    assert_eq!(running, 0, "a process had not finished exiting when the wait returned");
    let size = std::fs::metadata(&tick).unwrap().len();
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(std::fs::metadata(&tick).unwrap().len(), size, "a write landed after the wait");
}
