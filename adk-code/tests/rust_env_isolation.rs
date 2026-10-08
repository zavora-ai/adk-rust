//! Negative tests: host environment variables must not reach user Rust code,
//! neither at compile time (`env!`, `option_env!`) nor at run time
//! (`std::env::var`).
//!
//! Each test sets a marker variable in this test process and checks that the
//! compiled program cannot observe it. The tests skip when `rustc` is not on
//! `PATH`.

use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use adk_code::{
    CodeExecutor, EnvironmentPolicy, ExecutionLanguage, ExecutionPayload, ExecutionRequest,
    ExecutionStatus, RustExecutor, RustExecutorConfig, RustSandboxExecutor, SandboxPolicy,
};
use adk_sandbox::ProcessBackend;
use serde_json::json;

const SECRET_NAME: &str = "ADK_TEST_SECRET";
const SECRET_VALUE: &str = "adk-marker-5c1e0f7a";

/// Prints the marker as seen at run time and as captured at compile time.
const PROBE: &str = r#"
fn run(_input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "runtime": std::env::var("ADK_TEST_SECRET").ok(),
        "compile_time": option_env!("ADK_TEST_SECRET"),
    })
}
"#;

fn set_marker() {
    static MARKER: Once = Once::new();
    MARKER.call_once(|| {
        // SAFETY: every test in this binary calls `set_marker` before it spawns a
        // process or reads the environment, and `Once` blocks the others until the
        // write completes, so no other thread accesses the environment concurrently.
        unsafe { std::env::set_var(SECRET_NAME, SECRET_VALUE) };
    });
}

fn rustc_available() -> bool {
    std::process::Command::new("rustc").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn rust_request(code: &str, environment: EnvironmentPolicy) -> ExecutionRequest {
    ExecutionRequest {
        language: ExecutionLanguage::Rust,
        payload: ExecutionPayload::Source { code: code.to_string() },
        argv: vec![],
        stdin: None,
        input: Some(json!({})),
        sandbox: SandboxPolicy {
            environment,
            timeout: Duration::from_secs(60),
            ..SandboxPolicy::host_local()
        },
        identity: None,
    }
}

#[tokio::test]
async fn rust_sandbox_hides_host_variables_from_compiler_and_binary() {
    set_marker();
    if !rustc_available() {
        eprintln!(
            "skipping rust_sandbox_hides_host_variables_from_compiler_and_binary: rustc not found"
        );
        return;
    }

    let result = RustSandboxExecutor::default()
        .execute(rust_request(PROBE, EnvironmentPolicy::None))
        .await
        .expect("execution should produce a result");

    assert_eq!(result.status, ExecutionStatus::Success, "stderr: {}", result.stderr);
    assert_eq!(result.output, Some(json!({ "runtime": null, "compile_time": null })));
    assert!(!result.stdout.contains(SECRET_VALUE));
    assert!(!result.stderr.contains(SECRET_VALUE));
}

#[tokio::test]
async fn rust_sandbox_env_macro_cannot_read_host_variables() {
    set_marker();
    if !rustc_available() {
        eprintln!("skipping rust_sandbox_env_macro_cannot_read_host_variables: rustc not found");
        return;
    }

    let code = r#"
const SECRET: &str = env!("ADK_TEST_SECRET");
fn run(_input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "secret": SECRET })
}
"#;
    let result = RustSandboxExecutor::default()
        .execute(rust_request(code, EnvironmentPolicy::None))
        .await
        .expect("execution should produce a result");

    assert_eq!(result.status, ExecutionStatus::CompileFailed, "{result:?}");
    assert!(result.stderr.contains(SECRET_NAME), "stderr: {}", result.stderr);
    assert!(!result.stderr.contains(SECRET_VALUE));
    assert_eq!(result.output, None);
}

/// Positive control: an allowlisted variable reaches the binary, which proves the
/// probe can observe the marker, but it still never reaches the compiler.
#[tokio::test]
async fn rust_sandbox_allowlist_reaches_binary_but_not_compiler() {
    set_marker();
    if !rustc_available() {
        eprintln!(
            "skipping rust_sandbox_allowlist_reaches_binary_but_not_compiler: rustc not found"
        );
        return;
    }

    let allowlist = EnvironmentPolicy::AllowList(vec![SECRET_NAME.to_string()]);
    let result = RustSandboxExecutor::default()
        .execute(rust_request(PROBE, allowlist))
        .await
        .expect("execution should produce a result");

    assert_eq!(result.status, ExecutionStatus::Success, "stderr: {}", result.stderr);
    assert_eq!(result.output, Some(json!({ "runtime": SECRET_VALUE, "compile_time": null })));
}

#[tokio::test]
async fn rust_executor_hides_host_variables_from_compiler_and_binary() {
    set_marker();
    if !rustc_available() {
        eprintln!(
            "skipping rust_executor_hides_host_variables_from_compiler_and_binary: rustc not found"
        );
        return;
    }

    let executor =
        RustExecutor::new(Arc::new(ProcessBackend::default()), RustExecutorConfig::default());
    let result = executor
        .execute(PROBE, Some(&json!({})), Duration::from_secs(60))
        .await
        .expect("probe should compile and run");

    assert_eq!(result.exec_result.exit_code, 0, "stderr: {}", result.exec_result.stderr);
    assert_eq!(result.output, Some(json!({ "runtime": null, "compile_time": null })));
    assert!(!result.exec_result.stdout.contains(SECRET_VALUE));
    assert!(!result.exec_result.stderr.contains(SECRET_VALUE));
}

/// A binary that stops reading stdin early must not stall the caller: stdin
/// delivery runs under the execution timeout, concurrently with output capture.
#[tokio::test]
async fn rust_sandbox_unread_stdin_does_not_outlive_the_timeout() {
    set_marker();
    if !rustc_available() {
        eprintln!(
            "skipping rust_sandbox_unread_stdin_does_not_outlive_the_timeout: rustc not found"
        );
        return;
    }

    // The harness's JSON reader stops at the first invalid byte, leaving the rest
    // of stdin unread, and `run` never returns.
    let code = r#"
fn run(_input: serde_json::Value) -> serde_json::Value {
    loop { std::thread::sleep(std::time::Duration::from_secs(1)); }
}
"#;
    let mut stdin = b"x".to_vec();
    stdin.resize(8 * 1024 * 1024, b'x');
    let mut request = rust_request(code, EnvironmentPolicy::None);
    request.input = None;
    request.stdin = Some(stdin);
    request.sandbox.timeout = Duration::from_secs(20);

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        RustSandboxExecutor::default().execute(request),
    )
    .await
    .expect("execution must not hang on undelivered stdin")
    .expect("execution should produce a result");

    assert_eq!(result.status, ExecutionStatus::Timeout, "{result:?}");
    assert_ne!(result.stderr, "compilation timed out", "the run phase must be reached");
    assert!(started.elapsed() < Duration::from_secs(60), "took {:?}", started.elapsed());
}
