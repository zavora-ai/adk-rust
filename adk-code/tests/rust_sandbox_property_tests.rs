//! Property-based and unit tests for the Rust sandbox executor.
//!
//! **Feature: code-execution, Property 3: Unsupported Sandbox Controls Fail Closed**
//! *For any* `SandboxPolicy` that requests network, filesystem, or environment
//! restrictions, `validate_policy()` SHALL reject the policy when the backend
//! capabilities report that those controls cannot be enforced.
//! **Validates: Requirements 2.3, 10.2**
//!
//! **Feature: code-execution, Property 4: Timeout Produces Timeout Failure**
//! *For any* execution that exceeds the configured timeout, the result status
//! SHALL be `ExecutionStatus::Timeout`, never `ExecutionStatus::Success`.
//! **Validates: Requirements 3.3, 10.1**
//!
//! **Feature: code-execution, Property 5: Rust Compilation Failure Is Explicit**
//! *For any* invalid Rust source code, the result status SHALL be
//! `ExecutionStatus::CompileFailed`, not `ExecutionStatus::Failed` or
//! `ExecutionStatus::Success`.
//! **Validates: Requirements 3.4, 7.5, 10.3, 12.1**

use adk_code::{
    BackendCapabilities, CodeExecutor, EnvironmentPolicy, ExecutionError, ExecutionLanguage,
    ExecutionPayload, ExecutionRequest, ExecutionStatus, FilesystemPolicy, NetworkPolicy,
    RustSandboxExecutor, SandboxPolicy, validate_policy,
};
use proptest::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

// ── Helpers ────────────────────────────────────────────────────────────

fn rust_sandbox_caps() -> BackendCapabilities {
    RustSandboxExecutor::default().capabilities()
}

fn rust_request(code: &str, sandbox: SandboxPolicy) -> ExecutionRequest {
    ExecutionRequest {
        language: ExecutionLanguage::Rust,
        payload: ExecutionPayload::Source { code: code.to_string() },
        argv: vec![],
        stdin: None,
        input: Some(serde_json::json!({})),
        sandbox,
        identity: None,
    }
}

/// A permissive policy that the Rust sandbox CAN validate (no restrictions).
fn permissive_policy() -> SandboxPolicy {
    SandboxPolicy {
        network: NetworkPolicy::Enabled,
        filesystem: FilesystemPolicy::None,
        environment: EnvironmentPolicy::None,
        timeout: Duration::from_secs(30),
        max_stdout_bytes: 1_048_576,
        max_stderr_bytes: 1_048_576,
        working_directory: None,
    }
}

// ── Generators ─────────────────────────────────────────────────────────

fn arb_network_policy() -> impl Strategy<Value = NetworkPolicy> {
    prop_oneof![Just(NetworkPolicy::Disabled), Just(NetworkPolicy::Enabled)]
}

fn arb_filesystem_policy() -> impl Strategy<Value = FilesystemPolicy> {
    prop_oneof![
        Just(FilesystemPolicy::None),
        "[a-z/]{1,20}".prop_map(|p| FilesystemPolicy::WorkspaceReadOnly { root: PathBuf::from(p) }),
        "[a-z/]{1,20}"
            .prop_map(|p| FilesystemPolicy::WorkspaceReadWrite { root: PathBuf::from(p) }),
        (
            proptest::collection::vec("[a-z/]{1,10}".prop_map(PathBuf::from), 0..3),
            proptest::collection::vec("[a-z/]{1,10}".prop_map(PathBuf::from), 0..3),
        )
            .prop_map(|(ro, rw)| FilesystemPolicy::Paths { read_only: ro, read_write: rw }),
    ]
}

fn arb_environment_policy() -> impl Strategy<Value = EnvironmentPolicy> {
    prop_oneof![
        Just(EnvironmentPolicy::None),
        proptest::collection::vec("[A-Z_]{1,10}", 1..5).prop_map(EnvironmentPolicy::AllowList),
    ]
}

fn arb_sandbox_policy() -> impl Strategy<Value = SandboxPolicy> {
    (arb_network_policy(), arb_filesystem_policy(), arb_environment_policy()).prop_map(
        |(network, filesystem, environment)| SandboxPolicy {
            network,
            filesystem,
            environment,
            timeout: Duration::from_secs(30),
            max_stdout_bytes: 1_048_576,
            max_stderr_bytes: 1_048_576,
            working_directory: None,
        },
    )
}

// ════════════════════════════════════════════════════════════════════════
// Property 3: Unsupported Sandbox Controls Fail Closed
// ════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: code-execution, Property 3: Unsupported Sandbox Controls Fail Closed**
    /// *For any* `SandboxPolicy` that requests a restriction the Rust sandbox
    /// backend cannot enforce (network, filesystem, or environment),
    /// `validate_policy()` SHALL return `Err(ExecutionError::UnsupportedPolicy(...))`.
    /// **Validates: Requirements 2.3, 10.2**
    #[test]
    fn prop_unsupported_sandbox_controls_fail_closed(policy in arb_sandbox_policy()) {
        let caps = rust_sandbox_caps();

        let requests_network_restriction = matches!(policy.network, NetworkPolicy::Disabled);
        let requests_filesystem_access = !matches!(policy.filesystem, FilesystemPolicy::None);
        let requests_env_access = !matches!(policy.environment, EnvironmentPolicy::None);

        let should_fail =
            requests_network_restriction || requests_filesystem_access || requests_env_access;

        let result = validate_policy(&caps, &policy);

        if should_fail {
            let err = result.expect_err("must reject policies with unenforced controls");
            prop_assert!(
                matches!(err, ExecutionError::UnsupportedPolicy(_)),
                "expected UnsupportedPolicy, got: {err:?}"
            );
        } else {
            prop_assert!(result.is_ok(), "should accept policies without unenforced controls");
        }
    }
}

// ════════════════════════════════════════════════════════════════════════
// Property 4: Timeout Produces Timeout Failure
// ════════════════════════════════════════════════════════════════════════

/// **Feature: code-execution, Property 4: Timeout Produces Timeout Failure**
/// When execution exceeds the configured timeout, the result status SHALL be
/// `ExecutionStatus::Timeout`, not `ExecutionStatus::Success`.
/// **Validates: Requirements 3.3, 10.1**
#[tokio::test]
async fn test_timeout_produces_timeout_failure_short() {
    let executor = RustSandboxExecutor::default();

    // Code that sleeps — will exceed a very short timeout.
    let code = r#"
fn run(input: serde_json::Value) -> serde_json::Value {
    std::thread::sleep(std::time::Duration::from_secs(10));
    input
}
"#;

    let mut policy = permissive_policy();
    // 100ms is likely too short for compilation + execution.
    policy.timeout = Duration::from_millis(100);

    let request = rust_request(code, policy);
    let result = executor.execute(request).await.expect("should return result, not infra error");

    assert_eq!(
        result.status,
        ExecutionStatus::Timeout,
        "timed-out execution must report Timeout, got: {:?}",
        result.status
    );
    assert_ne!(result.status, ExecutionStatus::Success);
}

/// Timeout during the execution phase specifically (compilation succeeds).
/// **Validates: Requirements 3.3, 10.1**
#[tokio::test]
async fn test_timeout_during_execution_phase() {
    let executor = RustSandboxExecutor::default();

    // Infinite loop — compilation succeeds but execution never finishes.
    let code = r#"
fn run(input: serde_json::Value) -> serde_json::Value {
    loop { std::hint::spin_loop(); }
}
"#;

    let mut policy = permissive_policy();
    // 3s is enough for compilation but the loop will be killed.
    policy.timeout = Duration::from_secs(3);

    let request = rust_request(code, policy);
    let result = executor.execute(request).await.expect("should return result, not infra error");

    assert_eq!(
        result.status,
        ExecutionStatus::Timeout,
        "infinite loop must produce Timeout, got: {:?}",
        result.status
    );
}

// ════════════════════════════════════════════════════════════════════════
// Property 5: Rust Compilation Failure Is Explicit
// ════════════════════════════════════════════════════════════════════════

/// **Feature: code-execution, Property 5: Rust Compilation Failure Is Explicit**
/// *For any* invalid Rust source code, the result status SHALL be
/// `ExecutionStatus::CompileFailed`, not `ExecutionStatus::Failed` or
/// `ExecutionStatus::Success`.
/// **Validates: Requirements 3.4, 7.5, 10.3, 12.1**
///
/// This test exercises all invalid code variants from the generator. Each
/// iteration involves actual `rustc` compilation, so we run the property
/// across the full variant space rather than using random sampling.
#[tokio::test]
async fn prop_rust_compilation_failure_is_explicit() {
    let executor = RustSandboxExecutor::default();

    let invalid_snippets = [
        // Syntax error: missing semicolon between statements
        "fn run(input: serde_json::Value) -> serde_json::Value { let x = 42 input }",
        // Undefined variable reference
        "fn run(input: serde_json::Value) -> serde_json::Value { undefined_var }",
        // Type mismatch: returns &str instead of Value
        r#"fn run(input: serde_json::Value) -> serde_json::Value { "not a value" }"#,
        // Calling nonexistent function
        "fn run(input: serde_json::Value) -> serde_json::Value { nonexistent_fn() }",
        // Wrong arity: harness calls run(input) but function takes 0 args
        "fn run() -> serde_json::Value { serde_json::json!(null) }",
        // Undefined type in function body
        "fn run(input: serde_json::Value) -> serde_json::Value { let _: NoSuchType = input; input }",
        // Missing run function entirely
        "fn helper() -> i32 { 42 }",
        // Duplicate function definition
        "fn run(input: serde_json::Value) -> serde_json::Value { input }\nfn run(input: serde_json::Value) -> serde_json::Value { input }",
        // Use of nonexistent crate
        "use nonexistent_crate::Foo;\nfn run(input: serde_json::Value) -> serde_json::Value { input }",
        // Mismatched types in return
        "fn run(input: serde_json::Value) -> serde_json::Value { 42_i64 }",
    ];

    for code in &invalid_snippets {
        let request = rust_request(code, permissive_policy());
        let result = executor.execute(request).await.unwrap_or_else(|e| {
            panic!("invalid code should produce a result, not infra error: {e}")
        });

        assert_eq!(
            result.status,
            ExecutionStatus::CompileFailed,
            "invalid Rust must produce CompileFailed, got {:?} for: {}",
            result.status,
            code
        );
        assert!(
            !result.stderr.is_empty(),
            "compile failure should include diagnostics in stderr for: {code}"
        );
        assert_ne!(result.status, ExecutionStatus::Success);
        assert_ne!(result.status, ExecutionStatus::Failed);
    }
}

// ── Unit tests for specific compilation failures ───────────────────────

#[tokio::test]
async fn test_compile_failure_syntax_error() {
    let executor = RustSandboxExecutor::default();
    let code = "fn run(input: serde_json::Value) -> serde_json::Value { let x = }";
    let request = rust_request(code, permissive_policy());
    let result = executor.execute(request).await.unwrap();

    assert_eq!(result.status, ExecutionStatus::CompileFailed);
    assert!(!result.stderr.is_empty());
}

#[tokio::test]
async fn test_compile_failure_undefined_type() {
    let executor = RustSandboxExecutor::default();
    let code =
        "fn run(input: serde_json::Value) -> serde_json::Value { let x: NoSuchType = input; x }";
    let request = rust_request(code, permissive_policy());
    let result = executor.execute(request).await.unwrap();

    assert_eq!(result.status, ExecutionStatus::CompileFailed);
    assert!(!result.stderr.is_empty());
}

#[tokio::test]
async fn test_compile_failure_missing_run_function() {
    let executor = RustSandboxExecutor::default();
    // No run() function at all — harness calls run(input) which won't exist.
    let code = "fn helper() -> i32 { 42 }";
    let request = rust_request(code, permissive_policy());
    let result = executor.execute(request).await.unwrap();

    assert_eq!(result.status, ExecutionStatus::CompileFailed);
    assert!(!result.stderr.is_empty());
}
