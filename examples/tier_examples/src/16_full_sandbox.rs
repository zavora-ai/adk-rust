//! # 16 — Full: Sandbox Code Execution
//!
//! The `full` tier includes code execution and sandbox capabilities.
//! This example demonstrates sandboxed code execution with the process backend.
//!
//! ```toml
//! [dependencies]
//! adk-rust = { version = "0.8.2", features = ["full"] }
//! ```

use adk_rust::sandbox::{ExecRequest, Language, ProcessBackend, ProcessConfig, SandboxBackend};
use std::collections::HashMap;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    println!("=== Full Tier: Sandbox Code Execution ===\n");

    // Create a process-based sandbox backend with default config
    let backend = ProcessBackend::new(ProcessConfig::default());

    // Execute a Python script in the sandbox
    let python_request = ExecRequest {
        language: Language::Python,
        code: r#"
import json
result = {"sum": 2 + 2, "product": 3 * 7, "message": "Hello from sandbox!"}
print(json.dumps(result))
"#
        .to_string(),
        stdin: None,
        timeout: Duration::from_secs(10),
        memory_limit_mb: None,
        env: HashMap::new(),
    };

    println!("Executing Python code in sandbox...");
    let result = backend.execute(python_request).await?;

    println!("  Exit code: {}", result.exit_code);
    println!("  Stdout: {}", result.stdout.trim());
    if !result.stderr.is_empty() {
        println!("  Stderr: {}", result.stderr.trim());
    }
    println!("  Duration: {:?}", result.duration);

    // Execute a JavaScript snippet in the sandbox
    let js_request = ExecRequest {
        language: Language::JavaScript,
        code: r#"
const numbers = Array.from({length: 10}, (_, i) => i + 1);
const sum = numbers.reduce((a, b) => a + b, 0);
console.log(`Sum of 1..10 = ${sum}`);
"#
        .to_string(),
        stdin: None,
        timeout: Duration::from_secs(10),
        memory_limit_mb: None,
        env: HashMap::new(),
    };

    println!("\nExecuting JavaScript code in sandbox...");
    match backend.execute(js_request).await {
        Ok(js_result) => {
            println!("  Exit code: {}", js_result.exit_code);
            println!("  Stdout: {}", js_result.stdout.trim());
            if !js_result.stderr.is_empty() {
                println!("  Stderr: {}", js_result.stderr.trim());
            }
            println!("  Duration: {:?}", js_result.duration);
        }
        Err(e) => {
            println!("  Skipped (node not available): {e}");
        }
    }

    println!("\n✅ Sandbox code execution works with full tier.");
    Ok(())
}
