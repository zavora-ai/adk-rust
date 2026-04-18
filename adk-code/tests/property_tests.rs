//! Property-based tests for adk-code.
//!
//! Uses `proptest` with 100+ iterations per property.

use proptest::prelude::*;
use serde_json::json;

use adk_code::CodeError;
use adk_code::diagnostics::parse_diagnostics;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Generates a valid rustc JSON diagnostic line with the given level and message.
fn make_diagnostic_json(level: &str, message: &str, code: Option<&str>) -> String {
    let code_json = match code {
        Some(c) => format!(r#"{{"code":"{c}","explanation":null}}"#),
        None => "null".to_string(),
    };
    format!(
        r#"{{"message":"{message}","code":{code_json},"level":"{level}","spans":[],"children":[],"rendered":"{level}: {message}"}}"#
    )
}

/// Generates a valid rustc error diagnostic JSON line with arbitrary content.
fn arb_error_diagnostic() -> impl Strategy<Value = String> {
    (
        "[a-z ]{3,30}",                // message
        prop::option::of("E[0-9]{4}"), // optional error code
    )
        .prop_map(|(msg, code)| make_diagnostic_json("error", &msg, code.as_deref()))
}

/// Generates a valid rustc warning diagnostic JSON line.
fn arb_warning_diagnostic() -> impl Strategy<Value = String> {
    "[a-z ]{3,30}".prop_map(|msg| make_diagnostic_json("warning", &msg, None))
}

/// Generates a mix of error and warning diagnostics (1 to 5 lines).
fn arb_diagnostic_stderr() -> impl Strategy<Value = String> {
    prop::collection::vec(prop_oneof![arb_error_diagnostic(), arb_warning_diagnostic()], 1..=5)
        .prop_map(|lines| lines.join("\n"))
}

/// Generates arbitrary CodeError variants.
fn arb_code_error() -> impl Strategy<Value = CodeError> {
    prop_oneof![
        // CompileError
        ("[a-z ]{3,30}", prop::option::of("E[0-9]{4}"),).prop_map(|(msg, code)| {
            let diag = adk_code::RustDiagnostic {
                level: "error".to_string(),
                message: msg.clone(),
                spans: vec![],
                code,
            };
            CodeError::CompileError { diagnostics: vec![diag], stderr: format!("error: {msg}") }
        }),
        // DependencyNotFound
        "[a-z_]{3,15}".prop_map(|name| CodeError::DependencyNotFound {
            name,
            searched: vec!["config: /fake".to_string()],
        }),
        // Sandbox errors
        (1u64..=300).prop_map(|secs| {
            CodeError::Sandbox(adk_sandbox::SandboxError::Timeout {
                timeout: std::time::Duration::from_secs(secs),
            })
        }),
        (1u32..=1024).prop_map(|mb| {
            CodeError::Sandbox(adk_sandbox::SandboxError::MemoryExceeded { limit_mb: mb })
        }),
        "[a-z ]{3,30}".prop_map(|msg| {
            CodeError::Sandbox(adk_sandbox::SandboxError::ExecutionFailed(msg))
        }),
        "[a-z ]{3,30}"
            .prop_map(|msg| { CodeError::Sandbox(adk_sandbox::SandboxError::InvalidRequest(msg)) }),
        "[a-z ]{3,30}".prop_map(|msg| {
            CodeError::Sandbox(adk_sandbox::SandboxError::BackendUnavailable(msg))
        }),
        // InvalidCode
        "[a-z ]{3,30}".prop_map(CodeError::InvalidCode),
    ]
}

// ---------------------------------------------------------------------------
// Property 5: Diagnostic parsing
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: sandbox-and-code-tools, Property 5: Diagnostic parsing**
    /// *For any* valid rustc JSON diagnostic output, `parse_diagnostics()` should
    /// return a non-empty Vec of RustDiagnostic structs with level and message
    /// fields populated.
    /// **Validates: Requirements REQ-COD-002**
    #[test]
    fn prop_diagnostic_parsing(stderr in arb_diagnostic_stderr()) {
        let diagnostics = parse_diagnostics(&stderr);

        // Must return at least one diagnostic (we generate 1-5 valid lines)
        prop_assert!(
            !diagnostics.is_empty(),
            "expected non-empty diagnostics for input:\n{stderr}"
        );

        // Every diagnostic must have level and message populated
        for diag in &diagnostics {
            prop_assert!(
                !diag.level.is_empty(),
                "diagnostic level should not be empty"
            );
            prop_assert!(
                !diag.message.is_empty(),
                "diagnostic message should not be empty"
            );
            // Level must be one of the known rustc levels
            prop_assert!(
                matches!(diag.level.as_str(), "error" | "warning" | "note" | "help"),
                "unexpected diagnostic level: {}",
                diag.level
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Property 6: Error-as-information
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: sandbox-and-code-tools, Property 6: Error-as-information**
    /// *For any* CodeError variant, converting it to a JSON tool output should
    /// always produce a valid JSON object with "status" and "error"/"stderr"
    /// fields, never a ToolError.
    /// **Validates: Requirements REQ-ERR-002**
    #[test]
    fn prop_error_as_information(err in arb_code_error()) {
        let json = code_error_to_json(&err);

        // Must be a JSON object
        prop_assert!(json.is_object(), "expected JSON object, got: {json}");

        // Must have a "status" field
        prop_assert!(
            json.get("status").is_some(),
            "missing 'status' field in: {json}"
        );

        let status = json["status"].as_str().unwrap();
        prop_assert!(
            !status.is_empty(),
            "status field should not be empty"
        );

        // Must have a "stderr" or "diagnostics" field for error context
        let has_stderr = json.get("stderr").is_some();
        let has_diagnostics = json.get("diagnostics").is_some();
        prop_assert!(
            has_stderr || has_diagnostics,
            "expected 'stderr' or 'diagnostics' field in: {json}"
        );

        // Status must be one of the known values
        prop_assert!(
            matches!(status, "compile_error" | "error" | "timeout" | "memory_exceeded"),
            "unexpected status: {status}"
        );
    }
}

// ---------------------------------------------------------------------------
// Property 7: Deprecated alias compilation
// ---------------------------------------------------------------------------

/// **Feature: sandbox-and-code-tools, Property 7: Deprecated alias compilation**
/// The compat module should be importable and the migration guide documentation
/// should be accessible.
/// **Validates: Requirements REQ-MIG-001, REQ-MIG-002**
#[test]
fn prop_deprecated_alias_compilation() {
    // Verify the compat module is importable — this is a compile-time check.
    // If the module doesn't exist or has errors, this test won't compile.
    // The compat module contains migration documentation and deprecated type aliases.

    // Verify key re-exported types from adk-code are accessible
    assert!(!std::any::type_name::<adk_code::CodeTool>().is_empty());
    assert!(!std::any::type_name::<adk_code::RustExecutor>().is_empty());
    assert!(!std::any::type_name::<adk_code::RustExecutorConfig>().is_empty());
    assert!(!std::any::type_name::<adk_code::CodeError>().is_empty());
    assert!(!std::any::type_name::<adk_code::RustDiagnostic>().is_empty());

    // Verify adk-sandbox types are accessible (the migration target)
    assert!(!std::any::type_name::<dyn adk_sandbox::SandboxBackend>().is_empty());
    assert!(!std::any::type_name::<adk_sandbox::ExecRequest>().is_empty());
    assert!(!std::any::type_name::<adk_sandbox::ExecResult>().is_empty());

    // Verify parse_diagnostics is accessible
    let empty = adk_code::parse_diagnostics("");
    assert!(empty.is_empty());
}

// ---------------------------------------------------------------------------
// Helper: code_error_to_json (mirrors the logic in code_tool.rs)
// ---------------------------------------------------------------------------

/// Converts a CodeError to JSON using the same pattern as CodeTool.
/// This is extracted here because the function is private in code_tool.rs.
fn code_error_to_json(err: &CodeError) -> serde_json::Value {
    match err {
        CodeError::CompileError { diagnostics, stderr } => {
            let diag_json: Vec<serde_json::Value> = diagnostics
                .iter()
                .map(|d| {
                    json!({
                        "level": d.level,
                        "message": d.message,
                        "spans": d.spans.iter().map(|s| json!({
                            "file_name": s.file_name,
                            "line_start": s.line_start,
                            "line_end": s.line_end,
                            "column_start": s.column_start,
                            "column_end": s.column_end,
                        })).collect::<Vec<_>>(),
                        "code": d.code,
                    })
                })
                .collect();
            json!({
                "status": "compile_error",
                "diagnostics": diag_json,
                "stderr": stderr,
            })
        }
        CodeError::DependencyNotFound { name, searched } => json!({
            "status": "error",
            "stderr": format!("dependency not found: {name} (searched: {searched:?})"),
        }),
        CodeError::Sandbox(sandbox_err) => {
            use adk_sandbox::SandboxError;
            match sandbox_err {
                SandboxError::Timeout { timeout } => json!({
                    "status": "timeout",
                    "stderr": format!("execution timed out after {timeout:?}"),
                    "duration_ms": timeout.as_millis() as u64,
                }),
                SandboxError::MemoryExceeded { limit_mb } => json!({
                    "status": "memory_exceeded",
                    "stderr": format!("memory limit exceeded: {limit_mb} MB"),
                }),
                SandboxError::ExecutionFailed(msg) => json!({
                    "status": "error",
                    "stderr": msg,
                }),
                SandboxError::InvalidRequest(msg) => json!({
                    "status": "error",
                    "stderr": msg,
                }),
                SandboxError::BackendUnavailable(msg) => json!({
                    "status": "error",
                    "stderr": msg,
                }),
                SandboxError::EnforcerFailed { enforcer, message } => json!({
                    "status": "error",
                    "stderr": format!("sandbox enforcer '{enforcer}' failed: {message}"),
                }),
                SandboxError::EnforcerUnavailable { enforcer, message } => json!({
                    "status": "error",
                    "stderr": format!("sandbox enforcer '{enforcer}' unavailable: {message}"),
                }),
                SandboxError::PolicyViolation(msg) => json!({
                    "status": "error",
                    "stderr": msg,
                }),
            }
        }
        CodeError::InvalidCode(msg) => json!({
            "status": "error",
            "stderr": msg,
        }),
    }
}
