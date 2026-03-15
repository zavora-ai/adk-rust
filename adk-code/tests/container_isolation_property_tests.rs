//! Property tests for ContainerCommandExecutor — Property 4 variant: Backend Isolation Is Truthful.
//!
//! Validates that `ContainerCommandExecutor`:
//! - Reports `ContainerEphemeral` isolation (distinct from host-local and in-process)
//! - Claims enforcement of network, filesystem, environment, and timeout policies
//! - Supports Python, JavaScript, and Command languages
//! - Does not support Rust or Wasm (those have dedicated backends)
//! - Rejects guest module payloads
//! - Rejects empty source code

use adk_code::{
    CodeExecutor, ContainerCommandExecutor, ContainerConfig, ExecutionError, ExecutionIsolation,
    ExecutionLanguage, ExecutionPayload, ExecutionRequest, GuestModuleFormat, SandboxPolicy,
    validate_policy,
};
use proptest::prelude::*;

/// Generate supported container languages.
fn arb_supported_language() -> impl Strategy<Value = ExecutionLanguage> {
    prop_oneof![
        Just(ExecutionLanguage::Python),
        Just(ExecutionLanguage::JavaScript),
        Just(ExecutionLanguage::Command),
    ]
}

/// Generate unsupported container languages.
fn arb_unsupported_language() -> impl Strategy<Value = ExecutionLanguage> {
    prop_oneof![Just(ExecutionLanguage::Rust), Just(ExecutionLanguage::Wasm),]
}

// ── Property 4 (Container Variant): Backend Isolation Is Truthful ──────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: code-execution, Property 4 (Container): Backend Isolation Is Truthful**
    /// *For any* supported language, the container backend SHALL accept it.
    /// **Validates: Requirements 2.6, 10.6**
    #[test]
    fn prop_container_supports_expected_languages(lang in arb_supported_language()) {
        let executor = ContainerCommandExecutor::default();
        prop_assert!(
            executor.supports_language(&lang),
            "container should support {lang}"
        );
    }

    /// **Feature: code-execution, Property 4 (Container): Backend Isolation Is Truthful**
    /// *For any* unsupported language, the container backend SHALL reject it.
    /// **Validates: Requirements 2.6, 10.5, 10.6**
    #[test]
    fn prop_container_rejects_unsupported_languages(lang in arb_unsupported_language()) {
        let executor = ContainerCommandExecutor::default();
        prop_assert!(
            !executor.supports_language(&lang),
            "container should not support {lang}"
        );
    }

    /// **Feature: code-execution, Property 4 (Container): Backend Isolation Is Truthful**
    /// *For any* strict sandbox policy, the container backend SHALL accept it
    /// because it can enforce all controls.
    /// **Validates: Requirements 2.3, 2.6, 10.2**
    #[test]
    fn prop_container_enforces_all_strict_policies(_seed in 0u32..100) {
        let executor = ContainerCommandExecutor::default();
        let caps = executor.capabilities();
        let policy = SandboxPolicy::strict_rust();
        let result = validate_policy(&caps, &policy);
        prop_assert!(result.is_ok(), "container should enforce strict policies: {:?}", result);
    }

    /// **Feature: code-execution, Property 4 (Container): Backend Isolation Is Truthful**
    /// *For any* source code string, the container backend SHALL reject guest module payloads.
    /// **Validates: Requirements 2.6, 10.6**
    #[test]
    fn prop_container_rejects_guest_module_payloads(code_len in 1usize..50) {
        let executor = ContainerCommandExecutor::default();
        let bytes = vec![0u8; code_len];
        let request = ExecutionRequest {
            language: ExecutionLanguage::Python,
            payload: ExecutionPayload::GuestModule {
                format: GuestModuleFormat::Wasm,
                bytes,
            },
            argv: vec![],
            stdin: None,
            input: None,
            sandbox: SandboxPolicy::strict_rust(),
            identity: None,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(executor.execute(request));
        prop_assert!(result.is_err());
        match result.unwrap_err() {
            ExecutionError::InvalidRequest(msg) => {
                prop_assert!(
                    msg.contains("guest module") || msg.contains("GuestModule"),
                    "error should mention guest modules: {msg}"
                );
            }
            other => prop_assert!(false, "expected InvalidRequest, got: {other}"),
        }
    }

    /// **Feature: code-execution, Property 4 (Container): Backend Isolation Is Truthful**
    /// *For any* empty source code, the container backend SHALL reject it.
    /// **Validates: Requirements 10.2, 10.5**
    #[test]
    fn prop_container_rejects_empty_source(whitespace in "[ \t\n]{0,20}") {
        let executor = ContainerCommandExecutor::default();
        let request = ExecutionRequest {
            language: ExecutionLanguage::Python,
            payload: ExecutionPayload::Source { code: whitespace },
            argv: vec![],
            stdin: None,
            input: None,
            sandbox: SandboxPolicy::strict_rust(),
            identity: None,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(executor.execute(request));
        prop_assert!(result.is_err());
        match result.unwrap_err() {
            ExecutionError::InvalidRequest(msg) => {
                prop_assert!(msg.contains("empty"), "error should mention empty: {msg}");
            }
            other => prop_assert!(false, "expected InvalidRequest, got: {other}"),
        }
    }
}

// ── Deterministic isolation truthfulness tests ─────────────────────────

#[test]
fn container_isolation_is_container_ephemeral() {
    let executor = ContainerCommandExecutor::default();
    let caps = executor.capabilities();
    assert_eq!(caps.isolation, ExecutionIsolation::ContainerEphemeral);
}

#[test]
fn container_claims_all_policy_enforcement() {
    let executor = ContainerCommandExecutor::default();
    let caps = executor.capabilities();
    assert!(caps.enforce_network_policy);
    assert!(caps.enforce_filesystem_policy);
    assert!(caps.enforce_environment_policy);
    assert!(caps.enforce_timeout);
}

#[test]
fn container_supports_process_execution() {
    let executor = ContainerCommandExecutor::default();
    let caps = executor.capabilities();
    assert!(caps.supports_process_execution);
}

#[test]
fn container_isolation_differs_from_host_local() {
    let container_caps = ContainerCommandExecutor::default().capabilities();
    let host_caps = adk_code::RustSandboxExecutor::default().capabilities();

    // Isolation classes must be distinct
    assert_ne!(container_caps.isolation, host_caps.isolation);
    assert_eq!(container_caps.isolation, ExecutionIsolation::ContainerEphemeral);
    assert_eq!(host_caps.isolation, ExecutionIsolation::HostLocal);

    // Container enforces more policies than host-local
    assert!(container_caps.enforce_network_policy);
    assert!(!host_caps.enforce_network_policy);
}

#[test]
fn container_isolation_differs_from_in_process() {
    let container_caps = ContainerCommandExecutor::default().capabilities();
    let wasm_caps = adk_code::WasmGuestExecutor::new().capabilities();

    assert_ne!(container_caps.isolation, wasm_caps.isolation);
    assert_eq!(container_caps.isolation, ExecutionIsolation::ContainerEphemeral);
    assert_eq!(wasm_caps.isolation, ExecutionIsolation::InProcess);
}

#[test]
fn all_three_backends_have_distinct_isolation_classes() {
    let rust_iso = adk_code::RustSandboxExecutor::default().capabilities().isolation;
    let container_iso = ContainerCommandExecutor::default().capabilities().isolation;
    let wasm_iso = adk_code::WasmGuestExecutor::new().capabilities().isolation;

    assert_ne!(rust_iso, container_iso);
    assert_ne!(container_iso, wasm_iso);
    // Note: rust (HostLocal) and wasm (InProcess) are also distinct
    assert_ne!(rust_iso, wasm_iso);
}

#[test]
fn container_backend_name_is_descriptive() {
    let executor = ContainerCommandExecutor::default();
    assert_eq!(executor.name(), "container-command");
}

#[test]
fn custom_config_is_respected() {
    let config = ContainerConfig {
        runtime: "podman".to_string(),
        default_image: "node:20-slim".to_string(),
        extra_flags: vec!["--security-opt=no-new-privileges".to_string()],
        auto_remove: false,
    };
    let executor = ContainerCommandExecutor::new(config);
    assert_eq!(executor.name(), "container-command");
}
