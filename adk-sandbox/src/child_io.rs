//! Child-process plumbing shared by [`ProcessBackend`](crate::ProcessBackend) and the local
//! workspace session.
//!
//! Both run untrusted commands as a child process and need the same guarantees: output is
//! read while the child runs, under a byte cap, so a chatty child can neither deadlock on a
//! full pipe nor exhaust memory; stdin is written concurrently with those reads and inside
//! the timeout; and the child's whole process group is killed both on timeout and on normal
//! exit, so background descendants do not outlive the execution.

use std::process::ExitStatus;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::process::{Child, ChildStdin};
use tokio::task::JoinHandle;

/// Output of a child that exited before its timeout.
#[derive(Debug)]
pub(crate) struct CapturedOutput {
    /// The child's exit status.
    pub(crate) status: ExitStatus,
    /// Retained stdout bytes, at most the configured cap.
    pub(crate) stdout: Vec<u8>,
    /// Whether stdout bytes past the cap were discarded.
    pub(crate) stdout_truncated: bool,
    /// Retained stderr bytes, at most the configured cap.
    pub(crate) stderr: Vec<u8>,
    /// Whether stderr bytes past the cap were discarded.
    pub(crate) stderr_truncated: bool,
}

/// Drives a spawned child to completion: feeds `stdin`, reads both output pipes under `cap`,
/// and enforces `timeout`.
///
/// The child must have been spawned with piped stdout and stderr, a piped stdin when `stdin`
/// is `Some`, and — on Unix — in its own process group (`process_group(0)`), which this
/// function kills on timeout and again once the child exits.
///
/// Returns `Ok(None)` when the timeout elapsed.
///
/// # Errors
///
/// Returns an I/O error when waiting on the child or reading its output fails.
pub(crate) async fn collect_output(
    mut child: Child,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    cap: usize,
) -> std::io::Result<Option<CapturedOutput>> {
    // Captured before `wait`, which clears the id once the child is reaped.
    #[cfg(unix)]
    let process_group = child.id().and_then(|id| i32::try_from(id).ok());

    let mut stdout_reader = spawn_capped_reader(child.stdout.take(), cap);
    let mut stderr_reader = spawn_capped_reader(child.stderr.take(), cap);
    // Written concurrently with the reads: a child that fills its output pipe before it drains
    // stdin would otherwise deadlock against a blocking write that no timeout covers.
    let stdin_writer = tokio::spawn(write_stdin(child.stdin.take(), stdin));

    let outcome = tokio::time::timeout(timeout, async {
        let status = child.wait().await?;
        // Background descendants (`cmd &`, `nohup cmd &`) stay in the group after the leader
        // exits. Killing the group ends them with the execution and closes the pipe ends they
        // inherited, which would otherwise keep the reads below open until the timeout.
        #[cfg(unix)]
        kill_process_group(process_group);
        let (stdout, stdout_truncated) =
            (&mut stdout_reader).await.map_err(std::io::Error::other)??;
        let (stderr, stderr_truncated) =
            (&mut stderr_reader).await.map_err(std::io::Error::other)??;
        Ok::<_, std::io::Error>(CapturedOutput {
            status,
            stdout,
            stdout_truncated,
            stderr,
            stderr_truncated,
        })
    })
    .await;

    // Any stdin still unwritten has no reader once the child is gone.
    stdin_writer.abort();

    match outcome {
        Ok(captured) => captured.map(Some),
        Err(_elapsed) => {
            #[cfg(unix)]
            kill_process_group(process_group);
            // Covers platforms without process groups; `kill_on_drop` reaps it afterwards.
            if let Err(error) = child.start_kill() {
                tracing::debug!(error = %error, "child already exited at timeout");
            }
            stdout_reader.abort();
            stderr_reader.abort();
            Ok(None)
        }
    }
}

/// Sends `SIGKILL` to every process in `group`.
///
/// Called after the group leader may already have been reaped. The group id cannot be reused
/// while any member survives, so the signal reaches either the execution's own descendants or
/// no process at all (`ESRCH`, ignored).
#[cfg(unix)]
pub(crate) fn kill_process_group(group: Option<i32>) {
    if let Some(group) = group.filter(|group| *group > 0) {
        // SAFETY: kill(2) has no memory-safety preconditions. A negative pid addresses the
        // process group whose id is `group`, which the caller created for this execution.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
}

/// Reads `pipe` on a background task under `cap`; a missing pipe yields no output.
fn spawn_capped_reader<R>(
    pipe: Option<R>,
    cap: usize,
) -> JoinHandle<std::io::Result<(Vec<u8>, bool)>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        match pipe {
            Some(pipe) => read_capped(pipe, cap).await,
            None => Ok((Vec::new(), false)),
        }
    })
}

/// Writes `input` to the child's stdin and closes it.
///
/// A child that exits or closes stdin without consuming it all makes the write fail with
/// `BrokenPipe`; that is the child's choice, not an execution failure.
async fn write_stdin(pipe: Option<ChildStdin>, input: Option<Vec<u8>>) {
    let (Some(mut pipe), Some(input)) = (pipe, input) else {
        return;
    };
    match pipe.write_all(&input).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
            tracing::debug!("child closed stdin before consuming all input");
        }
        Err(error) => tracing::warn!(error = %error, "failed to write child stdin"),
    }
}

/// Reads `reader` to EOF, accumulating at most `cap` bytes.
///
/// Bytes past `cap` are read and discarded rather than left in the pipe. Stopping the read
/// would block the child on a full pipe buffer and stall it until the execution timeout, so
/// the drain continues even though the data is thrown away.
///
/// Returns the retained bytes and whether anything was discarded.
pub(crate) async fn read_capped<R>(mut reader: R, cap: usize) -> std::io::Result<(Vec<u8>, bool)>
where
    R: AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut retained = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut discarded = false;

    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let room = cap.saturating_sub(retained.len());
        if room == 0 {
            discarded = true;
            continue;
        }
        let take = room.min(read);
        retained.extend_from_slice(&chunk[..take]);
        if take < read {
            discarded = true;
        }
    }

    Ok((retained, discarded))
}

/// Truncates a byte buffer to at most `max_bytes`, ensuring the result is
/// valid UTF-8 by backing off to the nearest char boundary.
pub(crate) fn truncate_utf8(bytes: Vec<u8>, max_bytes: usize) -> String {
    if bytes.len() <= max_bytes {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let truncated = &bytes[..max_bytes];
    // Walk backwards to find a valid UTF-8 boundary.
    let mut end = max_bytes;
    while end > 0 && std::str::from_utf8(&truncated[..end]).is_err() {
        end -= 1;
    }
    std::str::from_utf8(&bytes[..end]).unwrap_or("").to_string()
}

/// Appends a truncation notice when output was discarded.
///
/// A model that receives silently-cut output has no way to know it is incomplete, so the notice
/// travels with the data rather than only appearing in a log. Mirrors the convention in
/// adk-python's `tools/environment` toolset.
pub(crate) fn note_truncation(mut text: String, discarded: bool) -> String {
    if discarded {
        text.push_str("\n... (truncated: output exceeded the configured limit)");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `read_capped` must retain at most `cap` bytes regardless of how much arrives.
    ///
    /// This is the property the streaming read exists for, and it is not observable from
    /// `ExecResult`: `truncate_utf8` caps the *reported* string either way, so an end-to-end
    /// test passes even when the whole stream was buffered first. Asserting on the retained
    /// buffer is what distinguishes bounded memory from a bounded report.
    #[tokio::test]
    async fn read_capped_retains_at_most_the_cap() {
        let cap = 4_096;
        // 256x the cap, so a buffering implementation would allocate 1 MiB here.
        let source = vec![b'x'; cap * 256];

        let (retained, discarded) = read_capped(&source[..], cap).await.expect("reads");

        assert_eq!(retained.len(), cap, "retained buffer must stop at the cap");
        assert!(discarded, "the overflow must be reported as discarded");
    }

    /// Everything is retained when the stream is smaller than the cap, and nothing is flagged.
    #[tokio::test]
    async fn read_capped_retains_everything_under_the_cap() {
        let source = vec![b'y'; 100];

        let (retained, discarded) = read_capped(&source[..], 4_096).await.expect("reads");

        assert_eq!(retained, source);
        assert!(!discarded);
    }

    /// A stream landing exactly on the cap is not reported as truncated.
    #[tokio::test]
    async fn read_capped_handles_the_exact_boundary() {
        let cap = 8_192;
        let source = vec![b'z'; cap];

        let (retained, discarded) = read_capped(&source[..], cap).await.expect("reads");

        assert_eq!(retained.len(), cap);
        assert!(!discarded, "reaching the cap exactly discards nothing");
    }

    #[test]
    fn test_truncate_utf8_within_limit() {
        let data = "hello world".as_bytes().to_vec();
        let result = truncate_utf8(data, 1024);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_truncate_utf8_at_boundary() {
        // Multi-byte UTF-8: "é" is 2 bytes (0xC3 0xA9)
        let data = "café".as_bytes().to_vec(); // 5 bytes: c a f 0xC3 0xA9
        // Truncate at 4 bytes — would split the "é"
        let result = truncate_utf8(data, 4);
        assert_eq!(result, "caf");
    }

    #[cfg(unix)]
    fn piped_shell(script: &str, stdin: bool) -> Child {
        use std::os::unix::process::CommandExt;

        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(script)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(if stdin { std::process::Stdio::piped() } else { std::process::Stdio::null() })
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        command.spawn().expect("sh spawns")
    }

    /// A child that fills its stdout pipe before reading stdin must not deadlock the writer.
    ///
    /// Writing stdin to completion before starting the readers blocked on a full stdin pipe
    /// while the child blocked on a full stdout pipe, and neither wait was under the timeout.
    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_is_written_concurrently_with_the_reads() {
        let input = vec![b'i'; 512 * 1_024];
        let child = piped_shell("head -c 524288 /dev/zero; wc -c", true);

        let captured = tokio::time::timeout(
            Duration::from_secs(30),
            collect_output(child, Some(input), Duration::from_secs(20), 1_024 * 1_024),
        )
        .await
        .expect("must not deadlock")
        .expect("collects")
        .expect("finishes before the timeout");

        assert!(captured.status.success());
        let text = String::from_utf8_lossy(&captured.stdout);
        assert!(text.trim_end().ends_with("524288"), "stdin was not fully delivered: {text:?}");
    }

    /// A child that never reads stdin is still bounded by the timeout.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unread_stdin_does_not_escape_the_timeout() {
        let input = vec![b'i'; 4 * 1_024 * 1_024];
        let child = piped_shell("sleep 30", true);

        let started = std::time::Instant::now();
        let captured = collect_output(child, Some(input), Duration::from_millis(300), 1_024)
            .await
            .expect("collects");

        assert!(captured.is_none(), "the sleeping child must time out");
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
    }

    /// Background descendants are killed when the group leader exits normally.
    #[cfg(unix)]
    #[tokio::test]
    async fn background_descendants_end_with_the_execution() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("survivor");
        let quoted = marker.to_string_lossy().replace('\'', "'\\''");
        let child = piped_shell(
            &format!(
                "nohup sh -c 'sleep 1; touch \"$0\"' '{quoted}' >/dev/null 2>&1 & echo started"
            ),
            false,
        );

        let captured = collect_output(child, None, Duration::from_secs(10), 1_024)
            .await
            .expect("collects")
            .expect("the leader exits immediately");

        assert_eq!(String::from_utf8_lossy(&captured.stdout).trim(), "started");
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert!(!marker.exists(), "a background descendant outlived the execution");
    }
}
