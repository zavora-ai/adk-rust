//! Integration tests for `RustExecutor`.
//!
//! These tests require `rustc` and a discoverable `serde_json` rlib.
//! Run with:
//!
//! ```bash
//! cargo test -p adk-code --test rust_executor_integration -- --ignored
//! ```

use adk_code::{CodeError, RustExecutor, RustExecutorConfig};
use adk_sandbox::ProcessBackend;
use std::sync::Arc;
use std::time::Duration;

fn make_executor() -> RustExecutor {
    let backend = Arc::new(ProcessBackend::default());
    RustExecutor::new(backend, RustExecutorConfig::default())
}

// ---------------------------------------------------------------------------
// Full pipeline: check → build → execute
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn full_pipeline_with_valid_code() {
    let executor = make_executor();
    let code = r#"
fn run(input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "greeting": "hello from rust" })
}
"#;

    let result = executor
        .execute(code, None, Duration::from_secs(60))
        .await
        .expect("valid code should compile and execute");

    assert_eq!(result.exec_result.exit_code, 0);
    let output = result.output.expect("should have structured JSON output");
    assert_eq!(output["greeting"], "hello from rust");
}

// ---------------------------------------------------------------------------
// Compile error passthrough
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn compile_error_returns_code_error_with_diagnostics() {
    let executor = make_executor();
    let code = r#"
fn run(input: serde_json::Value) -> serde_json::Value {
    let x: i32 = "not a number";
    input
}
"#;

    let err = executor
        .execute(code, None, Duration::from_secs(60))
        .await
        .expect_err("invalid code should produce CodeError");

    match err {
        CodeError::CompileError { diagnostics, stderr } => {
            assert!(
                diagnostics.iter().any(|d| d.level == "error"),
                "should contain at least one error-level diagnostic"
            );
            assert!(!stderr.is_empty(), "stderr should contain compiler output");
        }
        other => panic!("expected CodeError::CompileError, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// serde_json linking
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn serde_json_is_usable_in_user_code() {
    let executor = make_executor();
    let code = r#"
fn run(input: serde_json::Value) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("key".to_string(), serde_json::Value::String("value".to_string()));
    serde_json::Value::Object(map)
}
"#;

    let result = executor
        .execute(code, None, Duration::from_secs(60))
        .await
        .expect("code using serde_json types should compile");

    assert_eq!(result.exec_result.exit_code, 0);
    let output = result.output.expect("should have structured output");
    assert_eq!(output["key"], "value");
}
