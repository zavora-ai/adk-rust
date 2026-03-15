//! Property tests for WasmGuestExecutor — Property 10: Guest Module Boundaries Are Explicit.
//!
//! Validates that `WasmGuestExecutor`:
//! - Accepts only `GuestModule` payloads, not `Source` payloads
//! - Accepts only `Wasm` language, not JavaScript or other languages
//! - Validates WASM binary format (magic number, minimum size)
//! - Reports capabilities truthfully (in-process isolation)
//! - Is clearly documented as guest-module execution, not raw JS execution

use adk_code::{
    BackendCapabilities, CodeExecutor, ExecutionError, ExecutionIsolation, ExecutionLanguage,
    ExecutionPayload, ExecutionRequest, ExecutionStatus, GuestModuleFormat, SandboxPolicy,
    WasmGuestExecutor, validate_wasm_bytes,
};
use proptest::prelude::*;

/// A minimal valid WASM module (magic + version 1).
#[allow(dead_code)]
fn minimal_wasm_module() -> Vec<u8> {
    vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]
}

/// Generate arbitrary byte sequences that are NOT valid WASM.
fn arb_non_wasm_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..64).prop_filter("must not be valid WASM", |bytes| {
        // Filter out anything that happens to start with WASM magic and is >= 8 bytes
        !(bytes.len() >= 8 && bytes.starts_with(b"\0asm"))
    })
}

/// Generate valid WASM modules (magic + version + arbitrary trailing bytes).
fn arb_valid_wasm_module() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..128).prop_map(|extra| {
        let mut module = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
        module.extend(extra);
        module
    })
}

/// Generate non-Wasm languages.
fn arb_non_wasm_language() -> impl Strategy<Value = ExecutionLanguage> {
    prop_oneof![
        Just(ExecutionLanguage::Rust),
        Just(ExecutionLanguage::JavaScript),
        Just(ExecutionLanguage::Python),
        Just(ExecutionLanguage::Command),
    ]
}

// ── Property 10: Guest Module Boundaries Are Explicit ──────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: code-execution, Property 10: Guest Module Boundaries Are Explicit**
    /// *For any* non-WASM byte sequence, `validate_wasm_bytes` SHALL reject it.
    /// **Validates: Requirements 1.8, 10.5**
    #[test]
    fn prop_invalid_wasm_bytes_are_rejected(bytes in arb_non_wasm_bytes()) {
        let result = validate_wasm_bytes(&bytes);
        prop_assert!(result.is_err(), "non-WASM bytes should be rejected: {:?}", &bytes[..bytes.len().min(16)]);
        match result.unwrap_err() {
            ExecutionError::InvalidRequest(msg) => {
                prop_assert!(
                    msg.contains("too small") || msg.contains("magic number"),
                    "error should mention size or magic: {msg}"
                );
            }
            other => prop_assert!(false, "expected InvalidRequest, got: {other}"),
        }
    }

    /// **Feature: code-execution, Property 10: Guest Module Boundaries Are Explicit**
    /// *For any* valid WASM module, `validate_wasm_bytes` SHALL accept it.
    /// **Validates: Requirements 1.8, 10.5**
    #[test]
    fn prop_valid_wasm_bytes_are_accepted(bytes in arb_valid_wasm_module()) {
        let result = validate_wasm_bytes(&bytes);
        prop_assert!(result.is_ok(), "valid WASM module should be accepted, got: {:?}", result);
    }

    /// **Feature: code-execution, Property 10: Guest Module Boundaries Are Explicit**
    /// *For any* non-Wasm language, `WasmGuestExecutor` SHALL reject it.
    /// **Validates: Requirements 10.5, 12.1**
    #[test]
    fn prop_wasm_executor_rejects_non_wasm_languages(lang in arb_non_wasm_language()) {
        let executor = WasmGuestExecutor::new();
        prop_assert!(
            !executor.supports_language(&lang),
            "WasmGuestExecutor should not support {lang}"
        );
    }

    /// **Feature: code-execution, Property 10: Guest Module Boundaries Are Explicit**
    /// *For any* source code string, `WasmGuestExecutor` SHALL reject Source payloads.
    /// **Validates: Requirements 1.8, 3.11, 10.5**
    #[test]
    fn prop_wasm_executor_rejects_source_payloads(code in "[a-zA-Z0-9 ]{1,100}") {
        let executor = WasmGuestExecutor::new();
        let request = ExecutionRequest {
            language: ExecutionLanguage::Wasm,
            payload: ExecutionPayload::Source { code },
            argv: vec![],
            stdin: None,
            input: None,
            sandbox: SandboxPolicy::strict_rust(),
            identity: None,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(executor.execute(request));
        prop_assert!(result.is_err(), "Source payloads should be rejected for Wasm");
        match result.unwrap_err() {
            ExecutionError::InvalidRequest(msg) => {
                prop_assert!(
                    msg.contains("GuestModule") || msg.contains("Source"),
                    "error should mention payload type: {msg}"
                );
            }
            other => prop_assert!(false, "expected InvalidRequest, got: {other}"),
        }
    }

    /// **Feature: code-execution, Property 10: Guest Module Boundaries Are Explicit**
    /// *For any* valid WASM module, `WasmGuestExecutor` SHALL accept and validate it.
    /// **Validates: Requirements 1.8, 10.5, 12.1**
    #[test]
    fn prop_wasm_executor_accepts_valid_guest_modules(bytes in arb_valid_wasm_module()) {
        let executor = WasmGuestExecutor::new();
        let request = ExecutionRequest {
            language: ExecutionLanguage::Wasm,
            payload: ExecutionPayload::GuestModule {
                format: GuestModuleFormat::Wasm,
                bytes: bytes.clone(),
            },
            argv: vec![],
            stdin: None,
            input: None,
            sandbox: SandboxPolicy::strict_rust(),
            identity: None,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(executor.execute(request));
        prop_assert!(result.is_ok(), "valid WASM module should be accepted");
        let result = result.unwrap();
        prop_assert_eq!(result.status, ExecutionStatus::Success);
        let output = result.output.as_ref().unwrap();
        prop_assert_eq!(&output["validated"], true);
        prop_assert_eq!(&output["module_size_bytes"], bytes.len());
    }
}

// ── Deterministic boundary tests ───────────────────────────────────────

#[test]
fn wasm_executor_is_not_a_javascript_executor() {
    let executor = WasmGuestExecutor::new();
    assert!(!executor.supports_language(&ExecutionLanguage::JavaScript));
    assert!(executor.supports_language(&ExecutionLanguage::Wasm));
    // The name should clearly indicate guest-module execution
    assert_eq!(executor.name(), "wasm-guest");
}

#[test]
fn wasm_executor_capabilities_are_truthful() {
    let executor = WasmGuestExecutor::new();
    let caps = executor.capabilities();
    assert_eq!(caps.isolation, ExecutionIsolation::InProcess);
    // Guest modules enforce all policies by omission
    assert!(caps.enforce_network_policy);
    assert!(caps.enforce_filesystem_policy);
    assert!(caps.enforce_environment_policy);
    assert!(caps.enforce_timeout);
    assert!(caps.supports_structured_output);
    // Guest modules cannot spawn processes
    assert!(!caps.supports_process_execution);
}

#[test]
fn wasm_executor_isolation_differs_from_container() {
    let wasm_caps = WasmGuestExecutor::new().capabilities();
    let container_caps = BackendCapabilities {
        isolation: ExecutionIsolation::ContainerEphemeral,
        enforce_network_policy: true,
        enforce_filesystem_policy: true,
        enforce_environment_policy: true,
        enforce_timeout: true,
        supports_structured_output: true,
        supports_process_execution: true,
        supports_persistent_workspace: false,
        supports_interactive_sessions: false,
    };

    // Isolation classes must be distinct
    assert_ne!(wasm_caps.isolation, container_caps.isolation);
    // Container supports process execution, WASM does not
    assert_ne!(wasm_caps.supports_process_execution, container_caps.supports_process_execution);
}
