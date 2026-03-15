//! Property-based tests for backend capability reporting truthfulness.
//!
//! **Feature: code-execution, Property 6: Backend Isolation Is Truthful**
//! *For any* `SandboxPolicy` requesting controls that the Rust sandbox backend
//! reports as unenforced, execution SHALL be rejected before user code runs.
//! The backend SHALL report `HostLocal` isolation (not `ContainerEphemeral` or
//! `InProcess`), and its capability flags SHALL be internally consistent with
//! that isolation class.
//! **Validates: Requirements 1.5, 2.6, 10.6, 12.1**

use adk_code::{
    BackendCapabilities, CodeExecutor, EnvironmentPolicy, ExecutionError, ExecutionIsolation,
    ExecutionLanguage, FilesystemPolicy, NetworkPolicy, RustSandboxExecutor, SandboxPolicy,
    validate_policy,
};
use proptest::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

// ── Helpers ────────────────────────────────────────────────────────────

fn rust_sandbox() -> RustSandboxExecutor {
    RustSandboxExecutor::default()
}

fn rust_sandbox_caps() -> BackendCapabilities {
    rust_sandbox().capabilities()
}

// ── Generators ─────────────────────────────────────────────────────────

/// Generate a sandbox policy that requests at least one control the Rust
/// sandbox backend cannot enforce (network disabled, filesystem access,
/// or environment access).
fn arb_policy_with_unenforced_control() -> impl Strategy<Value = SandboxPolicy> {
    // At least one of these must be a restriction the backend can't enforce.
    let network = prop_oneof![
        // Disabled network — backend can't enforce this
        Just(NetworkPolicy::Disabled),
        Just(NetworkPolicy::Enabled),
    ];

    let filesystem = prop_oneof![
        Just(FilesystemPolicy::None),
        "[a-z/]{1,20}".prop_map(|p| FilesystemPolicy::WorkspaceReadOnly { root: PathBuf::from(p) }),
        "[a-z/]{1,20}"
            .prop_map(|p| FilesystemPolicy::WorkspaceReadWrite { root: PathBuf::from(p) }),
    ];

    let environment = prop_oneof![
        Just(EnvironmentPolicy::None),
        proptest::collection::vec("[A-Z_]{1,10}", 1..5).prop_map(EnvironmentPolicy::AllowList),
    ];

    (network, filesystem, environment)
        .prop_filter("at least one unenforced control must be requested", |(net, fs, env)| {
            let caps = RustSandboxExecutor::default().capabilities();
            let has_unenforced_network =
                matches!(net, NetworkPolicy::Disabled) && !caps.enforce_network_policy;
            let has_unenforced_fs =
                !matches!(fs, FilesystemPolicy::None) && !caps.enforce_filesystem_policy;
            let has_unenforced_env =
                !matches!(env, EnvironmentPolicy::None) && !caps.enforce_environment_policy;
            has_unenforced_network || has_unenforced_fs || has_unenforced_env
        })
        .prop_map(|(network, filesystem, environment)| SandboxPolicy {
            network,
            filesystem,
            environment,
            timeout: Duration::from_secs(30),
            max_stdout_bytes: 1_048_576,
            max_stderr_bytes: 1_048_576,
            working_directory: None,
        })
}

/// Generate a sandbox policy that only requests controls the Rust sandbox
/// backend CAN enforce (enabled network, no filesystem, no environment).
fn arb_policy_within_backend_capabilities() -> impl Strategy<Value = SandboxPolicy> {
    (1u64..120, 1024usize..2_097_152, 1024usize..2_097_152).prop_map(
        |(timeout_secs, max_stdout, max_stderr)| SandboxPolicy {
            network: NetworkPolicy::Enabled,
            filesystem: FilesystemPolicy::None,
            environment: EnvironmentPolicy::None,
            timeout: Duration::from_secs(timeout_secs),
            max_stdout_bytes: max_stdout,
            max_stderr_bytes: max_stderr,
            working_directory: None,
        },
    )
}

// ════════════════════════════════════════════════════════════════════════
// Property 6: Backend Isolation Is Truthful
// ════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: code-execution, Property 6: Backend Isolation Is Truthful**
    /// *For any* `SandboxPolicy` requesting controls that the Rust sandbox backend
    /// reports as unenforced, `validate_policy()` SHALL reject the policy.
    /// The backend never silently ignores unenforced controls.
    /// **Validates: Requirements 1.5, 2.6, 10.6, 12.1**
    #[test]
    fn prop_unenforced_controls_always_rejected(
        policy in arb_policy_with_unenforced_control()
    ) {
        let caps = rust_sandbox_caps();
        let result = validate_policy(&caps, &policy);

        prop_assert!(
            result.is_err(),
            "policy requesting unenforced controls must be rejected, but was accepted"
        );

        let err = result.unwrap_err();
        prop_assert!(
            matches!(err, ExecutionError::UnsupportedPolicy(_)),
            "expected UnsupportedPolicy, got: {err:?}"
        );
    }

    /// **Feature: code-execution, Property 6: Backend Isolation Is Truthful**
    /// *For any* `SandboxPolicy` requesting only controls the Rust sandbox backend
    /// CAN enforce, `validate_policy()` SHALL accept the policy.
    /// **Validates: Requirements 1.5, 2.6, 10.6, 12.1**
    #[test]
    fn prop_enforced_controls_always_accepted(
        policy in arb_policy_within_backend_capabilities()
    ) {
        let caps = rust_sandbox_caps();
        let result = validate_policy(&caps, &policy);

        prop_assert!(
            result.is_ok(),
            "policy requesting only enforced controls must be accepted, but was rejected: {:?}",
            result.unwrap_err()
        );
    }
}

// ── Isolation class truthfulness unit tests ────────────────────────────

/// The Rust sandbox backend MUST report `HostLocal` isolation in phase 1,
/// not `ContainerEphemeral`, `InProcess`, or any other class.
/// This prevents the backend from overstating its isolation guarantees.
/// **Validates: Requirements 1.5, 10.6**
#[test]
fn isolation_class_is_host_local() {
    let caps = rust_sandbox_caps();
    assert_eq!(
        caps.isolation,
        ExecutionIsolation::HostLocal,
        "phase 1 Rust sandbox must report HostLocal isolation"
    );
    assert_ne!(caps.isolation, ExecutionIsolation::ContainerEphemeral);
    assert_ne!(caps.isolation, ExecutionIsolation::InProcess);
    assert_ne!(caps.isolation, ExecutionIsolation::ContainerPersistent);
    assert_ne!(caps.isolation, ExecutionIsolation::ProviderHosted);
}

/// Host-local isolation MUST NOT claim to enforce network, filesystem,
/// or environment restrictions — those require container or provider-hosted
/// isolation.
/// **Validates: Requirements 2.6, 10.6**
#[test]
fn host_local_does_not_claim_os_level_controls() {
    let caps = rust_sandbox_caps();
    assert_eq!(caps.isolation, ExecutionIsolation::HostLocal);
    assert!(!caps.enforce_network_policy, "host-local backend must not claim network enforcement");
    assert!(
        !caps.enforce_filesystem_policy,
        "host-local backend must not claim filesystem enforcement"
    );
    assert!(
        !caps.enforce_environment_policy,
        "host-local backend must not claim environment enforcement"
    );
}

/// The Rust sandbox backend MUST claim timeout enforcement — it uses
/// `tokio::time::timeout` which works regardless of isolation class.
/// **Validates: Requirements 2.6, 12.1**
#[test]
fn host_local_claims_timeout_enforcement() {
    let caps = rust_sandbox_caps();
    assert!(caps.enforce_timeout, "Rust sandbox must claim timeout enforcement");
}

/// The Rust sandbox backend MUST claim structured output support — the
/// harness extracts JSON from the last stdout line.
/// **Validates: Requirements 2.6, 12.1**
#[test]
fn host_local_claims_structured_output() {
    let caps = rust_sandbox_caps();
    assert!(caps.supports_structured_output, "Rust sandbox must claim structured output support");
}

/// The Rust sandbox backend MUST NOT claim process execution, persistent
/// workspace, or interactive session support in phase 1.
/// **Validates: Requirements 2.6, 12.1**
#[test]
fn host_local_does_not_claim_advanced_features() {
    let caps = rust_sandbox_caps();
    assert!(
        !caps.supports_process_execution,
        "Rust sandbox must not claim process execution support"
    );
    assert!(
        !caps.supports_persistent_workspace,
        "Rust sandbox must not claim persistent workspace support"
    );
    assert!(
        !caps.supports_interactive_sessions,
        "Rust sandbox must not claim interactive session support"
    );
}

/// The backend name MUST be non-empty and descriptive.
/// **Validates: Requirements 1.5, 12.1**
#[test]
fn backend_name_is_descriptive() {
    let executor = rust_sandbox();
    let name = executor.name();
    assert!(!name.is_empty(), "backend name must not be empty");
    assert_eq!(name, "rust-sandbox");
}

/// The backend MUST support only Rust and reject all other languages.
/// **Validates: Requirements 1.5, 12.1**
#[test]
fn backend_supports_only_rust() {
    let executor = rust_sandbox();
    assert!(executor.supports_language(&ExecutionLanguage::Rust));
    assert!(!executor.supports_language(&ExecutionLanguage::JavaScript));
    assert!(!executor.supports_language(&ExecutionLanguage::Python));
    assert!(!executor.supports_language(&ExecutionLanguage::Wasm));
    assert!(!executor.supports_language(&ExecutionLanguage::Command));
}

/// Different isolation classes MUST be distinguishable — the type system
/// ensures `HostLocal != ContainerEphemeral != InProcess`.
/// **Validates: Requirements 1.5, 10.6**
#[test]
fn isolation_classes_are_distinct() {
    let all_classes = [
        ExecutionIsolation::InProcess,
        ExecutionIsolation::HostLocal,
        ExecutionIsolation::ContainerEphemeral,
        ExecutionIsolation::ContainerPersistent,
        ExecutionIsolation::ProviderHosted,
    ];

    for (i, a) in all_classes.iter().enumerate() {
        for (j, b) in all_classes.iter().enumerate() {
            if i != j {
                assert_ne!(a, b, "isolation classes must be distinct: {a:?} vs {b:?}");
            }
        }
    }
}
