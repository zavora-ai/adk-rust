//! Unit tests for the CodeExecutor trait and validation helpers.

use adk_code::{
    BackendCapabilities, EnvironmentPolicy, ExecutionError, ExecutionIsolation, ExecutionLanguage,
    ExecutionPayload, ExecutionRequest, GuestModuleFormat, NetworkPolicy, SandboxPolicy,
    validate_policy, validate_request,
};
use std::path::PathBuf;

/// Helper: capabilities that can enforce everything.
fn full_caps() -> BackendCapabilities {
    BackendCapabilities {
        isolation: ExecutionIsolation::ContainerEphemeral,
        enforce_network_policy: true,
        enforce_filesystem_policy: true,
        enforce_environment_policy: true,
        enforce_timeout: true,
        supports_structured_output: true,
        supports_process_execution: false,
        supports_persistent_workspace: false,
        supports_interactive_sessions: false,
    }
}

/// Helper: capabilities that cannot enforce anything.
fn weak_caps() -> BackendCapabilities {
    BackendCapabilities {
        isolation: ExecutionIsolation::HostLocal,
        enforce_network_policy: false,
        enforce_filesystem_policy: false,
        enforce_environment_policy: false,
        enforce_timeout: false,
        supports_structured_output: false,
        supports_process_execution: true,
        supports_persistent_workspace: false,
        supports_interactive_sessions: false,
    }
}

fn rust_source_request() -> ExecutionRequest {
    ExecutionRequest {
        language: ExecutionLanguage::Rust,
        payload: ExecutionPayload::Source { code: "fn main() {}".to_string() },
        argv: vec![],
        stdin: None,
        input: None,
        sandbox: SandboxPolicy::strict_rust(),
        identity: None,
    }
}

// ── validate_policy tests ──

#[test]
fn validate_policy_passes_with_full_caps_and_strict_policy() {
    let result = validate_policy(&full_caps(), &SandboxPolicy::strict_rust());
    assert!(result.is_ok());
}

#[test]
fn validate_policy_rejects_network_restriction_when_unenforced() {
    let mut caps = full_caps();
    caps.enforce_network_policy = false;
    let policy = SandboxPolicy { network: NetworkPolicy::Disabled, ..SandboxPolicy::strict_rust() };
    let err = validate_policy(&caps, &policy).unwrap_err();
    assert!(matches!(err, ExecutionError::UnsupportedPolicy(_)));
    assert!(err.to_string().contains("network"));
}

#[test]
fn validate_policy_allows_enabled_network_even_when_unenforced() {
    let mut caps = full_caps();
    caps.enforce_network_policy = false;
    let policy = SandboxPolicy { network: NetworkPolicy::Enabled, ..SandboxPolicy::strict_rust() };
    assert!(validate_policy(&caps, &policy).is_ok());
}

#[test]
fn validate_policy_rejects_filesystem_when_unenforced() {
    let mut caps = full_caps();
    caps.enforce_filesystem_policy = false;
    let policy = SandboxPolicy {
        filesystem: adk_code::FilesystemPolicy::WorkspaceReadOnly { root: PathBuf::from("/tmp") },
        ..SandboxPolicy::strict_rust()
    };
    let err = validate_policy(&caps, &policy).unwrap_err();
    assert!(matches!(err, ExecutionError::UnsupportedPolicy(_)));
    assert!(err.to_string().contains("filesystem"));
}

#[test]
fn validate_policy_allows_no_filesystem_even_when_unenforced() {
    let mut caps = full_caps();
    caps.enforce_filesystem_policy = false;
    let policy = SandboxPolicy {
        filesystem: adk_code::FilesystemPolicy::None,
        ..SandboxPolicy::strict_rust()
    };
    assert!(validate_policy(&caps, &policy).is_ok());
}

#[test]
fn validate_policy_rejects_environment_when_unenforced() {
    let mut caps = full_caps();
    caps.enforce_environment_policy = false;
    let policy = SandboxPolicy {
        environment: EnvironmentPolicy::AllowList(vec!["PATH".to_string()]),
        ..SandboxPolicy::strict_rust()
    };
    let err = validate_policy(&caps, &policy).unwrap_err();
    assert!(matches!(err, ExecutionError::UnsupportedPolicy(_)));
    assert!(err.to_string().contains("environment"));
}

#[test]
fn validate_policy_rejects_when_timeout_unenforced() {
    let mut caps = full_caps();
    caps.enforce_timeout = false;
    let err = validate_policy(&caps, &SandboxPolicy::strict_rust()).unwrap_err();
    assert!(matches!(err, ExecutionError::UnsupportedPolicy(_)));
    assert!(err.to_string().contains("timeout"));
}

#[test]
fn validate_policy_rejects_weak_caps_with_strict_policy() {
    let err = validate_policy(&weak_caps(), &SandboxPolicy::strict_rust()).unwrap_err();
    assert!(matches!(err, ExecutionError::UnsupportedPolicy(_)));
}

// ── validate_request tests ──

#[test]
fn validate_request_passes_for_supported_rust() {
    let caps = full_caps();
    let supported = [ExecutionLanguage::Rust];
    let request = rust_source_request();
    assert!(validate_request(&caps, &supported, &request).is_ok());
}

#[test]
fn validate_request_rejects_unsupported_language() {
    let caps = full_caps();
    let supported = [ExecutionLanguage::JavaScript];
    let request = rust_source_request();
    let err = validate_request(&caps, &supported, &request).unwrap_err();
    assert!(matches!(err, ExecutionError::UnsupportedLanguage(_)));
    assert!(err.to_string().contains("Rust"));
}

#[test]
fn validate_request_rejects_wasm_payload_for_non_wasm_language() {
    let caps = full_caps();
    let supported = [ExecutionLanguage::Rust, ExecutionLanguage::Wasm];
    let request = ExecutionRequest {
        language: ExecutionLanguage::Rust,
        payload: ExecutionPayload::GuestModule {
            format: GuestModuleFormat::Wasm,
            bytes: vec![0x00, 0x61, 0x73, 0x6d],
        },
        argv: vec![],
        stdin: None,
        input: None,
        sandbox: SandboxPolicy::strict_rust(),
        identity: None,
    };
    let err = validate_request(&caps, &supported, &request).unwrap_err();
    assert!(matches!(err, ExecutionError::InvalidRequest(_)));
    assert!(err.to_string().contains("GuestModule"));
}

#[test]
fn validate_request_rejects_source_payload_for_wasm_language() {
    let caps = full_caps();
    let supported = [ExecutionLanguage::Wasm];
    let request = ExecutionRequest {
        language: ExecutionLanguage::Wasm,
        payload: ExecutionPayload::Source { code: "not wasm".to_string() },
        argv: vec![],
        stdin: None,
        input: None,
        sandbox: SandboxPolicy::strict_rust(),
        identity: None,
    };
    let err = validate_request(&caps, &supported, &request).unwrap_err();
    assert!(matches!(err, ExecutionError::InvalidRequest(_)));
    assert!(err.to_string().contains("GuestModule"));
}

#[test]
fn validate_request_accepts_wasm_guest_module_for_wasm_language() {
    let caps = full_caps();
    let supported = [ExecutionLanguage::Wasm];
    let request = ExecutionRequest {
        language: ExecutionLanguage::Wasm,
        payload: ExecutionPayload::GuestModule {
            format: GuestModuleFormat::Wasm,
            bytes: vec![0x00, 0x61, 0x73, 0x6d],
        },
        argv: vec![],
        stdin: None,
        input: None,
        sandbox: SandboxPolicy::strict_rust(),
        identity: None,
    };
    assert!(validate_request(&caps, &supported, &request).is_ok());
}

#[test]
fn validate_request_propagates_policy_failure() {
    let caps = weak_caps();
    let supported = [ExecutionLanguage::Rust];
    let request = rust_source_request();
    let err = validate_request(&caps, &supported, &request).unwrap_err();
    assert!(matches!(err, ExecutionError::UnsupportedPolicy(_)));
}
