//! Host-process plumbing shared by the executors that spawn local processes.
//!
//! Every process started here begins from a cleared environment. `rustc`
//! receives only the toolchain allowlist from [`ProcessBackend::toolchain_env`],
//! so compile-time `env!` and `option_env!` cannot bake host credentials into a
//! binary, and compiled binaries receive only the variables their
//! [`EnvironmentPolicy`] names.

use std::ffi::OsString;
use std::io;
use std::process::ExitStatus;
use std::time::Duration;

use adk_sandbox::ProcessBackend;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use crate::EnvironmentPolicy;

/// Returns the host variables `policy` exposes, skipping names the host does not set.
pub(crate) fn allowlisted_host_env(policy: &EnvironmentPolicy) -> Vec<(String, OsString)> {
    match policy {
        EnvironmentPolicy::None => Vec::new(),
        EnvironmentPolicy::AllowList(names) => names
            .iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (name.clone(), value)))
            .collect(),
    }
}

/// Returns the environment for a compiled user binary.
///
/// Windows binaries also receive `SystemRoot`, which system DLL initialisation
/// (Winsock, for example) reads; it names the OS directory and carries no secret.
pub(crate) fn binary_env(policy: &EnvironmentPolicy) -> Vec<(String, OsString)> {
    let environment = allowlisted_host_env(policy);

    #[cfg(windows)]
    let mut environment = environment;

    #[cfg(windows)]
    if !environment.iter().any(|(key, _)| key.eq_ignore_ascii_case("SystemRoot"))
        && let Some(value) = std::env::var_os("SystemRoot")
    {
        environment.push(("SystemRoot".to_string(), value));
    }

    environment
}

/// Builds a `rustc` command that sees only the toolchain environment.
///
/// `kill_on_drop` ties the compiler to the caller's deadline: dropping the
/// `output()` future on timeout kills `rustc`.
pub(crate) fn rustc_command(rustc_path: &str) -> Command {
    let mut command = Command::new(rustc_path);
    command.env_clear().envs(ProcessBackend::toolchain_env()).kill_on_drop(true);
    command
}

/// Bytes captured from one output stream.
#[derive(Debug, Default)]
pub(crate) struct CapturedStream {
    /// At most the caller's cap.
    pub(crate) bytes: Vec<u8>,
    /// Whether output past the cap was read and discarded.
    pub(crate) truncated: bool,
}

/// A child process that ran to completion.
#[derive(Debug)]
pub(crate) struct CompletedProcess {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: CapturedStream,
    pub(crate) stderr: CapturedStream,
}

/// Feeds `stdin` to `child` while draining stdout and stderr, all under one deadline.
///
/// Writing stdin before the readers start deadlocks once the child fills an
/// output pipe while the parent is still writing, and writing outside the
/// deadline lets a child that never reads stdin stall the caller indefinitely.
///
/// Returns `Ok(None)` when the deadline passes; the child is killed first.
pub(crate) async fn run_to_completion(
    mut child: Child,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
) -> io::Result<Option<CompletedProcess>> {
    let stdin_pipe = child.stdin.take();
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let feed_stdin = async move {
        if let (Some(mut pipe), Some(bytes)) = (stdin_pipe, stdin) {
            // A child may exit without reading all of its input; that is not an I/O failure.
            match pipe.write_all(&bytes).await {
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
                result => result?,
            }
        }
        // The pipe drops here, which closes the child's stdin.
        Ok::<(), io::Error>(())
    };

    let run = async {
        let (fed, stdout, stderr, status) = tokio::join!(
            feed_stdin,
            read_capped(stdout_pipe, stdout_cap),
            read_capped(stderr_pipe, stderr_cap),
            child.wait(),
        );
        fed?;
        Ok(CompletedProcess { status: status?, stdout: stdout?, stderr: stderr? })
    };

    let outcome = tokio::time::timeout(timeout, run).await;
    match outcome {
        Ok(result) => result.map(Some),
        Err(_) => {
            // `kill_on_drop` would also reap it; killing now ends it before the caller reports.
            let _ = child.start_kill();
            Ok(None)
        }
    }
}

/// Reads `pipe` to EOF, retaining at most `cap` bytes.
///
/// Reading continues past the cap because an undrained pipe blocks the child
/// until the deadline.
async fn read_capped<R>(pipe: Option<R>, cap: usize) -> io::Result<CapturedStream>
where
    R: AsyncRead + Unpin,
{
    let mut captured = CapturedStream::default();
    let Some(mut pipe) = pipe else {
        return Ok(captured);
    };

    let mut chunk = [0u8; 8192];
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            return Ok(captured);
        }
        let take = cap.saturating_sub(captured.bytes.len()).min(read);
        captured.bytes.extend_from_slice(&chunk[..take]);
        captured.truncated |= take < read;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::Instant;

    #[test]
    fn no_environment_policy_exposes_nothing() {
        assert!(allowlisted_host_env(&EnvironmentPolicy::None).is_empty());
    }

    #[test]
    fn allowlist_exposes_only_named_variables_the_host_sets() {
        let policy = EnvironmentPolicy::AllowList(vec![
            "PATH".to_string(),
            "ADK_CODE_VARIABLE_THE_HOST_NEVER_SETS".to_string(),
        ]);
        let expected: Vec<(String, OsString)> =
            std::env::var_os("PATH").map(|path| ("PATH".to_string(), path)).into_iter().collect();
        assert_eq!(allowlisted_host_env(&policy), expected);
    }

    #[test]
    fn rustc_command_starts_from_a_cleared_environment() {
        let command = rustc_command("rustc");
        // std renders a cleared environment as an `env -i` prefix on Unix.
        #[cfg(unix)]
        assert!(format!("{:?}", command.as_std()).starts_with("env -i "), "{command:?}");

        let explicit: Vec<String> = command
            .as_std()
            .get_envs()
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        let toolchain: Vec<String> =
            ProcessBackend::toolchain_env().into_iter().map(|(key, _)| key).collect();
        assert_eq!(explicit.len(), toolchain.len());
        assert!(explicit.iter().all(|key| toolchain.contains(key)), "{explicit:?}");
    }

    /// A child that never reads stdin must not stall the caller past the deadline.
    #[cfg(unix)]
    #[tokio::test]
    async fn unread_stdin_does_not_outlive_the_deadline() {
        let child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep");

        let started = Instant::now();
        let outcome = run_to_completion(
            child,
            Some(vec![b'x'; 8 * 1024 * 1024]),
            Duration::from_millis(300),
            1024,
            1024,
        )
        .await
        .expect("run");

        assert!(outcome.is_none(), "expected the deadline to pass");
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    /// A child that fills stdout before reading stdin completes instead of deadlocking.
    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_and_output_are_pumped_concurrently() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("head -c 1000000 /dev/zero; cat > /dev/null; echo done >&2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sh");

        let completed = run_to_completion(
            child,
            Some(vec![b'x'; 1_000_000]),
            Duration::from_secs(20),
            4096,
            4096,
        )
        .await
        .expect("run")
        .expect("must complete before the deadline");

        assert!(completed.status.success());
        assert_eq!(completed.stdout.bytes.len(), 4096);
        assert!(completed.stdout.truncated);
        assert_eq!(completed.stderr.bytes, b"done\n");
        assert!(!completed.stderr.truncated);
    }
}
