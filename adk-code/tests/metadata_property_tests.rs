//! Property tests for execution metadata — Property 7: Execution Results Remain Structured.
//!
//! Validates that:
//! - ExecutionMetadata captures backend name, language, isolation, status, duration, and identity
//! - ArtifactRef captures key, size, and content type
//! - ExecutionResult metadata field is optional and serializable
//! - Structured output, stdout, stderr, and compile diagnostics remain distinguishable

use adk_code::{
    ArtifactRef, ExecutionIsolation, ExecutionLanguage, ExecutionMetadata, ExecutionResult,
    ExecutionStatus,
};
use proptest::prelude::*;

fn arb_language() -> impl Strategy<Value = ExecutionLanguage> {
    prop_oneof![
        Just(ExecutionLanguage::Rust),
        Just(ExecutionLanguage::JavaScript),
        Just(ExecutionLanguage::Wasm),
        Just(ExecutionLanguage::Python),
        Just(ExecutionLanguage::Command),
    ]
}

fn arb_isolation() -> impl Strategy<Value = ExecutionIsolation> {
    prop_oneof![
        Just(ExecutionIsolation::InProcess),
        Just(ExecutionIsolation::HostLocal),
        Just(ExecutionIsolation::ContainerEphemeral),
        Just(ExecutionIsolation::ContainerPersistent),
        Just(ExecutionIsolation::ProviderHosted),
    ]
}

fn arb_status() -> impl Strategy<Value = ExecutionStatus> {
    prop_oneof![
        Just(ExecutionStatus::Success),
        Just(ExecutionStatus::Timeout),
        Just(ExecutionStatus::CompileFailed),
        Just(ExecutionStatus::Failed),
        Just(ExecutionStatus::Rejected),
    ]
}

fn arb_metadata() -> impl Strategy<Value = ExecutionMetadata> {
    (
        "[a-z-]{3,20}",
        arb_language(),
        arb_isolation(),
        arb_status(),
        0u64..100_000,
        proptest::option::of("[a-z0-9-]{5,30}"),
    )
        .prop_map(|(backend_name, language, isolation, status, duration_ms, identity)| {
            ExecutionMetadata {
                backend_name,
                language,
                isolation,
                status,
                duration_ms,
                identity,
                artifact_refs: vec![],
            }
        })
}

fn arb_artifact_ref() -> impl Strategy<Value = ArtifactRef> {
    (
        "[a-z-]{3,20}",
        0u64..10_000_000,
        proptest::option::of(prop_oneof![
            Just("text/plain".to_string()),
            Just("application/json".to_string()),
            Just("application/octet-stream".to_string()),
        ]),
    )
        .prop_map(|(key, size_bytes, content_type)| ArtifactRef {
            key,
            size_bytes,
            content_type,
        })
}

// ── Property 7: Execution Results Remain Structured ────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: code-execution, Property 7: Execution Results Remain Structured**
    /// *For any* execution metadata, serialization round-trip SHALL preserve all fields.
    /// **Validates: Requirements 6.1, 6.2, 6.3**
    #[test]
    fn prop_metadata_serialization_roundtrip(meta in arb_metadata()) {
        let json = serde_json::to_string(&meta).unwrap();
        let deserialized: ExecutionMetadata = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&meta.backend_name, &deserialized.backend_name);
        prop_assert_eq!(meta.language, deserialized.language);
        prop_assert_eq!(meta.isolation, deserialized.isolation);
        prop_assert_eq!(meta.status, deserialized.status);
        prop_assert_eq!(meta.duration_ms, deserialized.duration_ms);
        prop_assert_eq!(&meta.identity, &deserialized.identity);
    }

    /// **Feature: code-execution, Property 7: Execution Results Remain Structured**
    /// *For any* artifact reference, serialization round-trip SHALL preserve all fields.
    /// **Validates: Requirements 7.4**
    #[test]
    fn prop_artifact_ref_serialization_roundtrip(artifact in arb_artifact_ref()) {
        let json = serde_json::to_string(&artifact).unwrap();
        let deserialized: ArtifactRef = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&artifact.key, &deserialized.key);
        prop_assert_eq!(artifact.size_bytes, deserialized.size_bytes);
        prop_assert_eq!(&artifact.content_type, &deserialized.content_type);
    }

    /// **Feature: code-execution, Property 7: Execution Results Remain Structured**
    /// *For any* execution result with metadata, the metadata field SHALL survive
    /// serialization round-trip.
    /// **Validates: Requirements 6.3, 7.1, 7.2, 7.5**
    #[test]
    fn prop_result_with_metadata_roundtrip(
        meta in arb_metadata(),
        status in arb_status(),
        duration in 0u64..100_000,
    ) {
        let result = ExecutionResult {
            status,
            stdout: "some output".to_string(),
            stderr: "some error".to_string(),
            output: Some(serde_json::json!({"key": "value"})),
            exit_code: Some(0),
            stdout_truncated: false,
            stderr_truncated: false,
            duration_ms: duration,
            metadata: Some(meta.clone()),
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: ExecutionResult = serde_json::from_str(&json).unwrap();

        prop_assert_eq!(result.status, deserialized.status);
        prop_assert_eq!(&result.stdout, &deserialized.stdout);
        prop_assert_eq!(&result.stderr, &deserialized.stderr);
        prop_assert_eq!(&result.output, &deserialized.output);
        prop_assert!(deserialized.metadata.is_some());
        let dm = deserialized.metadata.unwrap();
        prop_assert_eq!(&meta.backend_name, &dm.backend_name);
        prop_assert_eq!(meta.language, dm.language);
    }

    /// **Feature: code-execution, Property 7: Execution Results Remain Structured**
    /// *For any* execution result without metadata, the metadata field SHALL be
    /// absent in serialized JSON.
    /// **Validates: Requirements 7.1, 7.2**
    #[test]
    fn prop_result_without_metadata_omits_field(status in arb_status()) {
        let result = ExecutionResult {
            status,
            stdout: String::new(),
            stderr: String::new(),
            output: None,
            exit_code: None,
            stdout_truncated: false,
            stderr_truncated: false,
            duration_ms: 0,
            metadata: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        prop_assert!(!json.contains("metadata"), "metadata should be omitted when None");
        let deserialized: ExecutionResult = serde_json::from_str(&json).unwrap();
        prop_assert!(deserialized.metadata.is_none());
    }
}

// ── Deterministic structured result tests ──────────────────────────────

#[test]
fn compile_failure_is_distinct_from_runtime_failure() {
    let compile_fail = ExecutionResult {
        status: ExecutionStatus::CompileFailed,
        stdout: String::new(),
        stderr: "error[E0308]: mismatched types".to_string(),
        output: None,
        exit_code: Some(1),
        stdout_truncated: false,
        stderr_truncated: false,
        duration_ms: 100,
        metadata: None,
    };

    let runtime_fail = ExecutionResult {
        status: ExecutionStatus::Failed,
        stdout: String::new(),
        stderr: "thread 'main' panicked".to_string(),
        output: None,
        exit_code: Some(101),
        stdout_truncated: false,
        stderr_truncated: false,
        duration_ms: 50,
        metadata: None,
    };

    assert_ne!(compile_fail.status, runtime_fail.status);
    assert_eq!(compile_fail.status, ExecutionStatus::CompileFailed);
    assert_eq!(runtime_fail.status, ExecutionStatus::Failed);
}

#[test]
fn structured_output_is_distinct_from_stdout() {
    let result = ExecutionResult {
        status: ExecutionStatus::Success,
        stdout: "debug: processing...".to_string(),
        stderr: String::new(),
        output: Some(serde_json::json!({"answer": 42})),
        exit_code: Some(0),
        stdout_truncated: false,
        stderr_truncated: false,
        duration_ms: 10,
        metadata: None,
    };

    // stdout and output are separate fields
    assert_ne!(result.stdout, serde_json::to_string(&result.output).unwrap());
    assert!(result.output.is_some());
    assert!(!result.stdout.is_empty());
}

#[test]
fn metadata_with_artifact_refs() {
    let meta = ExecutionMetadata {
        backend_name: "rust-sandbox".to_string(),
        language: ExecutionLanguage::Rust,
        isolation: ExecutionIsolation::HostLocal,
        status: ExecutionStatus::Success,
        duration_ms: 42,
        identity: Some("inv-123".to_string()),
        artifact_refs: vec![
            ArtifactRef {
                key: "stdout-full".to_string(),
                size_bytes: 2_000_000,
                content_type: Some("text/plain".to_string()),
            },
            ArtifactRef {
                key: "binary-output".to_string(),
                size_bytes: 500_000,
                content_type: Some("application/octet-stream".to_string()),
            },
        ],
    };

    let json = serde_json::to_string(&meta).unwrap();
    let deserialized: ExecutionMetadata = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.artifact_refs.len(), 2);
    assert_eq!(deserialized.artifact_refs[0].key, "stdout-full");
    assert_eq!(deserialized.artifact_refs[1].size_bytes, 500_000);
}
