//! The output cap must bound memory *while reading*, not after.
//!
//! `wait_with_output()` buffered a process's entire stdout before `truncate_utf8` applied the
//! 1 MiB limit, so a process writing 10 GiB allocated 10 GiB and the cap limited only what was
//! reported. These tests drive far more output than the cap through a real process.

use adk_sandbox::{ExecRequest, Language, ProcessBackend, ProcessConfig, SandboxBackend};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

const CAP: usize = 1_024 * 1_024;
const OUTPUT_HELPER_ENV: &str = "ADK_SANDBOX_OUTPUT_CAP_HELPER";
static OUTPUT_CHUNK: [u8; 64 * 1_024] = [b'x'; 64 * 1_024];

fn shell_request(script: &str) -> ExecRequest {
    ExecRequest {
        code: script.to_string(),
        language: Language::Command,
        stdin: None,
        timeout: Duration::from_secs(60),
        memory_limit_mb: None,
        env: Default::default(),
    }
}

/// The executable as it must appear inside a shell command string.
///
/// On Windows the command reaches `cmd /C` through `Command::arg`, which escapes an
/// embedded `"` as `\"`. `cmd.exe` does not read that escape, so it strips the outer pair
/// and tries to run the remaining backslash-quoted text as a program name. The path is
/// therefore passed **unquoted**, which is only representable when it contains no space —
/// see [`helper_invocation_supported`].
#[cfg(windows)]
fn shell_executable(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(not(windows))]
fn shell_executable(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// Whether this platform can express an invocation of the helper.
///
/// Windows only: a path containing a space has no unquoted form, and a quoted one cannot
/// survive `Command::arg` (see [`shell_executable`]). Such a checkout is skipped rather
/// than reported as a cap failure it is not. Cargo target paths in CI contain no space.
fn helper_invocation_supported() -> bool {
    if !cfg!(windows) {
        return true;
    }
    let exe = std::env::current_exe().expect("the test executable path is available");
    !exe.display().to_string().contains(' ')
}

/// Emits the skip notice for a checkout whose path the shell cannot receive.
fn skip_unsupported_path() {
    eprintln!(
        "skipped: the test executable path contains a space, which cmd.exe cannot be given \
         unquoted; the output cap itself is unaffected"
    );
}

fn helper_request(stream: &str, bytes: usize) -> ExecRequest {
    let executable = std::env::current_exe().expect("the test executable path is available");
    let command =
        format!("{} --exact output_producer --nocapture", shell_executable(executable.as_path()));

    let mut env = HashMap::new();
    env.insert(OUTPUT_HELPER_ENV.to_string(), format!("{stream}:{bytes}"));

    ExecRequest {
        code: command,
        language: Language::Command,
        stdin: None,
        timeout: Duration::from_secs(60),
        memory_limit_mb: None,
        env,
    }
}

fn write_bytes(mut writer: impl Write, bytes: usize) {
    let full_chunks = bytes / OUTPUT_CHUNK.len();
    let remainder = bytes % OUTPUT_CHUNK.len();

    for _ in 0..full_chunks {
        writer.write_all(&OUTPUT_CHUNK).expect("the output helper writes a full chunk");
    }
    writer
        .write_all(&OUTPUT_CHUNK[..remainder])
        .expect("the output helper writes the final partial chunk");
    writer.flush().expect("the output helper flushes its stream");
}

/// Emits deterministic output when this test binary is spawned by an output-cap test.
///
/// Reusing the test executable avoids platform-specific utilities such as `yes` and `head`,
/// which are unavailable on Windows. A normal test-suite invocation leaves the helper inert.
#[test]
fn output_producer() {
    let Ok(spec) = std::env::var(OUTPUT_HELPER_ENV) else {
        return;
    };
    let (stream, bytes) = spec.split_once(':').expect("the helper specification has a separator");
    let bytes = bytes.parse::<usize>().expect("the helper byte count is valid");

    match stream {
        "stdout" => write_bytes(io::stdout().lock(), bytes),
        "stderr" => write_bytes(io::stderr().lock(), bytes),
        "stdout-then-done" => {
            write_bytes(io::stdout().lock(), bytes);
            writeln!(io::stderr().lock(), "done").expect("the helper writes its completion marker");
        }
        other => panic!("unsupported output helper stream: {other}"),
    }
}

/// 64 MiB of stdout is retained only up to the cap.
#[tokio::test]
async fn stdout_far_beyond_the_cap_is_capped() {
    if !helper_invocation_supported() {
        skip_unsupported_path();
        return;
    }
    let backend = ProcessBackend::new(ProcessConfig::default());
    // 64 MiB: large enough that buffering it all would be obvious, small enough to stay quick.
    let result = backend
        .execute(helper_request("stdout", 64 * 1_024 * 1_024))
        .await
        .expect("the process runs to completion");

    assert_eq!(result.exit_code, 0, "stderr: {}", result.stderr);
    // The payload is capped exactly; a truncation notice follows it.
    let payload = result.stdout.split("\n... (truncated").next().expect("payload");
    assert_eq!(
        payload.len(),
        CAP,
        "64 MiB in must retain exactly the cap; an empty result would also satisfy `<= CAP`"
    );
}

/// The same bound applies to stderr.
#[tokio::test]
async fn stderr_far_beyond_the_cap_is_capped() {
    if !helper_invocation_supported() {
        skip_unsupported_path();
        return;
    }
    let backend = ProcessBackend::new(ProcessConfig::default());
    let result = backend
        .execute(helper_request("stderr", 32 * 1_024 * 1_024))
        .await
        .expect("the process runs to completion");

    assert_eq!(result.exit_code, 0, "stdout: {}", result.stdout);
    let payload = result.stderr.split("\n... (truncated").next().expect("payload");
    assert_eq!(payload.len(), CAP, "32 MiB in must retain exactly the cap");
}

/// A process producing more than the cap must still be reaped, not left blocked on a full pipe.
///
/// If the reader stopped at the cap instead of draining, the child would block writing and this
/// would hit the timeout rather than exiting cleanly.
#[tokio::test]
async fn a_process_exceeding_the_cap_still_exits_cleanly() {
    if !helper_invocation_supported() {
        skip_unsupported_path();
        return;
    }
    let backend = ProcessBackend::new(ProcessConfig::default());
    let result = backend
        .execute(helper_request("stdout-then-done", 8 * 1_024 * 1_024))
        .await
        .expect("the process runs to completion");

    assert_eq!(result.exit_code, 0, "the child must not be stalled by a full pipe");
    assert!(result.stderr.contains("done"), "stderr after the flood must survive");
}

/// Output under the cap is unaffected.
#[tokio::test]
async fn small_output_is_returned_intact() {
    let backend = ProcessBackend::new(ProcessConfig::default());
    let result = backend.execute(shell_request("echo hello")).await.expect("runs");

    assert_eq!(result.stdout.trim(), "hello");
    assert_eq!(result.exit_code, 0);
}

/// Truncated output must say so, so a model is not handed a silent partial result.
#[tokio::test]
async fn truncated_output_carries_a_notice() {
    if !helper_invocation_supported() {
        skip_unsupported_path();
        return;
    }
    let backend = ProcessBackend::new(ProcessConfig::default());
    let result = backend
        .execute(helper_request("stdout", 4 * 1_024 * 1_024))
        .await
        .expect("the process runs to completion");

    assert!(
        result.stdout.contains("truncated"),
        "the output must state that it was cut; adk-python's environment toolset does the same"
    );
}

/// A smaller configured cap is honoured.
#[tokio::test]
async fn the_cap_is_configurable() {
    if !helper_invocation_supported() {
        skip_unsupported_path();
        return;
    }
    let config = ProcessConfig { max_output_bytes: 4_096, ..ProcessConfig::default() };
    let backend = ProcessBackend::new(config);
    let result = backend
        .execute(helper_request("stdout", 1_024 * 1_024))
        .await
        .expect("the process runs to completion");

    // The notice is appended after the capped payload, so allow for its length.
    assert!(
        result.stdout.len() < 4_096 + 128,
        "a 4 KiB cap must bound the output, got {} bytes",
        result.stdout.len()
    );
}
