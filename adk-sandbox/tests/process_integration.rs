//! Integration tests for `ProcessBackend`.
//!
//! These tests require external tools (rustc, python3, node) and are marked
//! `#[ignore]` so they only run when explicitly requested:
//!
//! ```bash
//! cargo test -p adk-sandbox --test process_integration -- --ignored
//! ```

use adk_sandbox::{ExecRequest, Language, ProcessBackend, SandboxBackend, SandboxError};
use std::collections::HashMap;
use std::time::Duration;

fn backend() -> ProcessBackend {
    ProcessBackend::default()
}

fn env_with_path() -> HashMap<String, String> {
    let mut env = HashMap::new();
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }
    env
}

// ---------------------------------------------------------------------------
// Rust
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn rust_valid_snippet_compiles_and_runs() {
    let backend = backend();
    let request = ExecRequest {
        language: Language::Rust,
        code: r#"fn main() { println!("hello"); }"#.to_string(),
        stdin: None,
        timeout: Duration::from_secs(30),
        memory_limit_mb: None,
        env: env_with_path(),
    };

    let result = backend.execute(request).await.expect("should succeed");
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("hello"));
}

#[tokio::test]
#[ignore]
async fn rust_invalid_snippet_returns_nonzero_exit_code() {
    let backend = backend();
    let request = ExecRequest {
        language: Language::Rust,
        code: "fn main() { let x: i32 = \"not a number\"; }".to_string(),
        stdin: None,
        timeout: Duration::from_secs(30),
        memory_limit_mb: None,
        env: env_with_path(),
    };

    // Non-zero exit code is a valid result, NOT a SandboxError.
    let result = backend.execute(request).await.expect("should return ExecResult, not error");
    assert_ne!(result.exit_code, 0, "compile error should produce non-zero exit code");
    assert!(!result.stderr.is_empty(), "stderr should contain compiler diagnostics");
}

// ---------------------------------------------------------------------------
// Timeout
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn timeout_enforcement_returns_sandbox_error() {
    let backend = backend();
    let request = ExecRequest {
        language: Language::Command,
        code: "sleep 60".to_string(),
        stdin: None,
        timeout: Duration::from_secs(1),
        memory_limit_mb: None,
        env: env_with_path(),
    };

    let err = backend.execute(request).await.expect_err("should timeout");
    assert!(
        matches!(err, SandboxError::Timeout { .. }),
        "expected SandboxError::Timeout, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn python_snippet_executes() {
    let backend = backend();
    let request = ExecRequest {
        language: Language::Python,
        code: "print('hello from python')".to_string(),
        stdin: None,
        timeout: Duration::from_secs(30),
        memory_limit_mb: None,
        env: env_with_path(),
    };

    let result = backend.execute(request).await.expect("python3 should be available");
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("hello from python"));
}

// ---------------------------------------------------------------------------
// JavaScript
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn javascript_snippet_executes() {
    let backend = backend();
    let request = ExecRequest {
        language: Language::JavaScript,
        code: "console.log('hello from js')".to_string(),
        stdin: None,
        timeout: Duration::from_secs(30),
        memory_limit_mb: None,
        env: env_with_path(),
    };

    let result = backend.execute(request).await.expect("node should be available");
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("hello from js"));
}
