//! What `ProcessBackend` actually enforces.
//!
//! Three gaps motivated these tests:
//!
//! 1. `ProcessBackend::default()` has no enforcer, so execution is subprocess isolation
//!    only — but nothing said so, and the crate is named `adk-sandbox`.
//! 2. Rust source was compiled by a `Command` built inline and run with `output()`,
//!    bypassing `run_command`. That skipped the enforcer wrapper, the request timeout, and
//!    the process group. Rust compilation is not inert: `include_str!` reads files and
//!    procedural macros run arbitrary code, all before the produced binary ever enters the
//!    runtime boundary.
//! 3. `SandboxPolicy::env` was never applied. A policy that set variables silently
//!    supplied none.

use adk_sandbox::{ExecRequest, IsolationClass, Language, ProcessBackend, SandboxBackend};
use std::time::Duration;

fn rust_request(code: &str, timeout: Duration) -> ExecRequest {
    ExecRequest {
        language: Language::Rust,
        code: code.to_string(),
        timeout,
        memory_limit_mb: None,
        env: Default::default(),
        stdin: None,
    }
}

// ── The isolation class is explicit ───────────────────────────────────

#[test]
fn the_default_backend_reports_subprocess_isolation_only() {
    assert_eq!(
        ProcessBackend::default().isolation(),
        IsolationClass::SubprocessOnly,
        "a caller must be able to tell that the default applies no OS restriction"
    );
}

// ── Compilation is bounded ────────────────────────────────────────────

#[tokio::test]
async fn a_compile_that_exceeds_the_timeout_is_stopped() {
    // The timeout previously applied only to running the compiled binary, so a compile
    // that blocked ran unbounded. A `const` evaluation loop keeps rustc busy without
    // needing network or unusual features.
    let code = r#"
const fn spin(mut n: u64) -> u64 {
    let mut acc = 0u64;
    while n > 0 {
        acc = acc.wrapping_add(n);
        n -= 1;
    }
    acc
}
const HEAVY: u64 = spin(50_000_000);
fn main() { println!("{}", HEAVY); }
"#;

    let backend = ProcessBackend::default();
    let started = std::time::Instant::now();
    let result = backend.execute(rust_request(code, Duration::from_millis(500))).await;
    let elapsed = started.elapsed();

    // Either the compile is killed by the timeout (an error or a non-zero exit) or it
    // finished; what must not happen is running far past the requested budget.
    assert!(
        elapsed < Duration::from_secs(20),
        "compilation ran {elapsed:?}, far beyond the 500ms request timeout"
    );
    // A successful, fast compile is acceptable on a very fast machine; a hang is not.
    if let Ok(exec) = &result {
        assert!(
            exec.duration < Duration::from_secs(20),
            "the reported duration ignores the request timeout: {:?}",
            exec.duration
        );
    }
}

#[tokio::test]
async fn a_compile_error_is_still_reported_as_a_failed_execution() {
    // Routing compilation through the enforced path must not change how a compile error
    // surfaces.
    let backend = ProcessBackend::default();
    let result = backend
        .execute(rust_request("fn main() { this is not rust }", Duration::from_secs(60)))
        .await
        .expect("a compile error is a result, not an infrastructure error");

    assert_ne!(result.exit_code, 0, "a compile error must be a non-zero exit");
    assert!(
        !result.stderr.is_empty(),
        "the compiler's diagnostics must reach the caller: {result:?}"
    );
}

#[tokio::test]
async fn a_working_program_still_runs() {
    // Guards against the compile rerouting breaking ordinary execution. The
    // compiler receives a small toolchain allowlist, while the produced binary
    // starts again from the cleared runtime environment.
    let backend = ProcessBackend::default();
    let result = backend
        .execute(rust_request(
            r#"
fn main() {
    println!("hello from rust");
    println!("compile_lib={}", option_env!("LIB").is_some());
    println!("runtime_lib={}", std::env::var_os("LIB").is_some());
}
"#,
            Duration::from_secs(120),
        ))
        .await
        .expect("a valid program must execute");

    assert_eq!(result.exit_code, 0, "stderr was: {}", result.stderr);
    assert!(result.stdout.contains("hello from rust"));
    assert!(
        result.stdout.contains("runtime_lib=false"),
        "the MSVC library path leaked into the produced program: {}",
        result.stdout
    );
    #[cfg(windows)]
    assert!(
        result.stdout.contains("compile_lib=true"),
        "rustc did not receive the MSVC library path: {}",
        result.stdout
    );
}
