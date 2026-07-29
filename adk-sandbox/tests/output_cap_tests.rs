//! The output cap must bound memory *while reading*, not after.
//!
//! `wait_with_output()` buffered a process's entire stdout before `truncate_utf8` applied the
//! 1 MiB limit, so a process writing 10 GiB allocated 10 GiB and the cap limited only what was
//! reported. These tests drive far more output than the cap through a real process.

use adk_sandbox::{ExecRequest, Language, ProcessBackend, ProcessConfig, SandboxBackend};
use std::time::Duration;

const CAP: usize = 1_024 * 1_024;

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

/// 64 MiB of stdout is retained only up to the cap.
#[tokio::test]
async fn stdout_far_beyond_the_cap_is_capped() {
    let backend = ProcessBackend::new(ProcessConfig::default());
    // 64 MiB: large enough that buffering it all would be obvious, small enough to stay quick.
    let result = backend
        .execute(shell_request("yes 0123456789abcdef | head -c 67108864"))
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
    let backend = ProcessBackend::new(ProcessConfig::default());
    let result = backend
        .execute(shell_request("yes 0123456789abcdef | head -c 33554432 1>&2"))
        .await
        .expect("the process runs to completion");

    let payload = result.stderr.split("\n... (truncated").next().expect("payload");
    assert_eq!(payload.len(), CAP, "32 MiB in must retain exactly the cap");
}

/// A process producing more than the cap must still be reaped, not left blocked on a full pipe.
///
/// If the reader stopped at the cap instead of draining, the child would block writing and this
/// would hit the timeout rather than exiting cleanly.
#[tokio::test]
async fn a_process_exceeding_the_cap_still_exits_cleanly() {
    let backend = ProcessBackend::new(ProcessConfig::default());
    let result = backend
        .execute(shell_request("yes 0123456789abcdef | head -c 8388608; echo done 1>&2"))
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
    let backend = ProcessBackend::new(ProcessConfig::default());
    let result = backend
        .execute(shell_request("yes 0123456789abcdef | head -c 4194304"))
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
    let config = ProcessConfig { max_output_bytes: 4_096, ..ProcessConfig::default() };
    let backend = ProcessBackend::new(config);
    let result = backend
        .execute(shell_request("yes 0123456789abcdef | head -c 1048576"))
        .await
        .expect("the process runs to completion");

    // The notice is appended after the capped payload, so allow for its length.
    assert!(
        result.stdout.len() < 4_096 + 128,
        "a 4 KiB cap must bound the output, got {} bytes",
        result.stdout.len()
    );
}
