//! RustCodeTool example — primary Rust-first code execution tool
//!
//! Demonstrates using `RustCodeTool` to execute authored Rust in a sandbox.
//!
//! Run: cargo run --bin rust_code_tool

use adk_code::{
    ExecutionLanguage, ExecutionPayload, ExecutionRequest, RustSandboxExecutor,
    SandboxPolicy,
};
use adk_tool::{RustCodeTool, Tool};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== RustCodeTool Doc-Test ===\n");

    // 1. Create the tool with default strict sandbox
    let tool = RustCodeTool::new();
    assert_eq!(tool.name(), "rust_code");
    assert_eq!(
        tool.required_scopes(),
        &["code:execute", "code:execute:rust"]
    );
    println!("✓ RustCodeTool created with correct name and scopes");

    // 2. Backend preset constructor
    let backend_tool = RustCodeTool::backend();
    assert_eq!(backend_tool.name(), "rust_code");
    println!("✓ RustCodeTool::backend() preset works");

    // 3. Custom executor constructor
    let executor = RustSandboxExecutor::default();
    let custom_tool = RustCodeTool::with_executor(executor);
    assert_eq!(custom_tool.name(), "rust_code");
    println!("✓ RustCodeTool::with_executor() works");

    // 4. Verify the underlying executor types
    let sandbox_executor = RustSandboxExecutor::default();
    let caps = adk_code::CodeExecutor::capabilities(&sandbox_executor);
    assert_eq!(
        caps.isolation,
        adk_code::ExecutionIsolation::HostLocal
    );
    println!("✓ RustSandboxExecutor reports HostLocal isolation");

    // 5. Verify strict Rust sandbox policy defaults
    let policy = SandboxPolicy::strict_rust();
    assert!(matches!(
        policy.network,
        adk_code::NetworkPolicy::Disabled
    ));
    assert!(matches!(
        policy.filesystem,
        adk_code::FilesystemPolicy::None
    ));
    println!("✓ SandboxPolicy::strict_rust() has network disabled and no filesystem");

    // 6. Verify execution request construction
    let request = ExecutionRequest {
        language: ExecutionLanguage::Rust,
        payload: ExecutionPayload::Source {
            code: r#"fn run(input: serde_json::Value) -> serde_json::Value {
    let n = input["n"].as_i64().unwrap_or(0);
    serde_json::json!({ "doubled": n * 2 })
}"#
            .to_string(),
        },
        argv: vec![],
        stdin: None,
        input: Some(serde_json::json!({ "n": 21 })),
        sandbox: SandboxPolicy::strict_rust(),
        identity: None,
    };
    assert_eq!(request.language, ExecutionLanguage::Rust);
    println!("✓ ExecutionRequest constructed for Rust source");

    // 7. Verify parameters schema
    let schema = tool.parameters_schema();
    assert!(schema.is_some());
    let schema = schema.unwrap();
    assert_eq!(schema["required"][0], "code");
    println!("✓ Parameters schema requires 'code' field");

    println!("\n=== All RustCodeTool tests passed! ===");
    Ok(())
}
