//! A Unix process group holds a process and everything it starts, and ends as a whole
//! (spec 3.2 §7.7, §10 W1, K-I5).
//!
//! Each test starts `sh` in a session of its own and lets it start `sleep` in the background,
//! which on its own would outlive the shell, and prints that process's id.
#![cfg(all(unix, feature = "process-tree"))]

use std::process::Stdio;
use std::time::{Duration, Instant};

use adk_sandbox::process_tree::ProcessGroup;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{ChildStdout, Command};

/// Long enough that only a group that does not end reaches it.
const LIMIT: Duration = Duration::from_secs(10);

/// Whether a process with id `pid` exists, a zombie included.
fn exists(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let probed = unsafe { libc::kill(pid as libc::pid_t, 0) };
    probed == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Starts `sh -c script` in a new session, holds it in its group, and returns the group, the
/// shell's standard output, and the id of the process the script printed first.
async fn shell(script: &str) -> (ProcessGroup, BufReader<ChildStdout>, u32) {
    let mut command = Command::new("sh");
    command.args(["-c", script]).stdin(Stdio::null()).stdout(Stdio::piped());
    ProcessGroup::new_session(&mut command);
    let mut child = command.spawn().expect("sh starts");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped"));
    let group = ProcessGroup::contain(child).expect("the shell leads its own group");
    let mut line = String::new();
    stdout.read_line(&mut line).await.expect("the script prints a process id");
    let pid = line.trim().parse().expect("a process id");
    (group, stdout, pid)
}

#[tokio::test]
async fn ending_a_group_ends_the_process_its_leader_started() {
    let (group, _stdout, background) = shell("sleep 300 & echo $!; exec sleep 300").await;
    let leader = group.id();
    assert!(exists(leader) && exists(background), "both processes run");

    assert!(group.terminate_and_wait(LIMIT).unwrap(), "the group ended within the limit");

    assert!(!exists(leader), "the leader has exited and been reaped");
    assert!(!exists(background), "the background process has exited and been reaped");
    // A second termination signals nothing and only confirms the group is gone.
    assert!(group.terminate_and_wait_async(LIMIT).await.unwrap());
}

#[tokio::test]
async fn a_leader_that_exited_on_its_own_leaves_nothing_behind() {
    let (group, _stdout, background) = shell("sleep 300 & echo $!").await;

    assert!(group.wait_for_leader(LIMIT).await.unwrap(), "the shell exits after printing");
    assert!(exists(background), "the background process outlives the shell");

    assert!(group.terminate_and_wait_async(LIMIT).await.unwrap());
    assert!(!exists(group.id()));
    assert!(!exists(background), "the process the exited leader left ended with the group");
}

#[tokio::test]
async fn sigterm_comes_first_and_a_handler_runs_before_the_group_ends() {
    let (group, mut stdout, background) =
        shell("trap 'echo stopped; exit 0' TERM; sleep 300 & echo $!; wait").await;

    assert!(group.terminate_and_wait_async(LIMIT).await.unwrap());

    let mut rest = String::new();
    stdout.read_to_string(&mut rest).await.unwrap();
    assert_eq!(rest.trim(), "stopped", "the shell handled SIGTERM before it was killed");
    assert!(!exists(background));
}

#[tokio::test]
async fn processes_that_ignore_sigterm_are_killed_after_the_grace() {
    // An ignored signal stays ignored across `exec`, so `sleep` ignores SIGTERM too.
    let (group, _stdout, background) = shell("trap '' TERM; sleep 300 & echo $!; wait").await;
    let limit = Duration::from_secs(4);

    let started = Instant::now();
    assert!(group.terminate_and_wait_async(limit).await.unwrap());

    assert!(started.elapsed() >= limit / 2, "SIGKILL waited for the grace");
    assert!(!exists(group.id()));
    assert!(!exists(background));
}

#[tokio::test]
async fn dropping_the_group_kills_what_is_left_in_it() {
    let (group, _stdout, background) = shell("sleep 300 & echo $!; exec sleep 300").await;

    drop(group);

    let deadline = Instant::now() + LIMIT;
    while exists(background) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!exists(background), "the background process outlived the dropped group");
}

#[tokio::test]
async fn a_process_without_a_session_of_its_own_is_refused() {
    let child = Command::new("sleep").arg("300").kill_on_drop(true).spawn().unwrap();

    let error = ProcessGroup::contain(child).unwrap_err();

    assert!(error.to_string().contains("ProcessGroup::new_session"), "{error}");
}

#[tokio::test]
async fn a_command_that_already_leads_its_group_keeps_it() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 300 & echo $!; exec sleep 300"]).stdout(Stdio::piped());
    command.process_group(0);
    ProcessGroup::new_session(&mut command);
    let mut child = command.spawn().expect("a group leader starts despite `setsid` refusing it");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped"));
    let group = ProcessGroup::contain(child).expect("the shell leads its own group");
    let mut line = String::new();
    stdout.read_line(&mut line).await.unwrap();
    let background: u32 = line.trim().parse().unwrap();

    assert!(group.terminate_and_wait_async(LIMIT).await.unwrap());
    assert!(!exists(background));
}
