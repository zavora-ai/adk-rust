//! Schema Validation Guardrail Example
//!
//! Demonstrates JSON schema validation for agent outputs.
//!
//! Run with: cargo run --example guardrail_schema --features "guardrails"

use adk_guardrail::{Guardrail, SchemaValidator};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Schema Validation Example ===\n");

    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer", "minimum": 0 }
        },
        "required": ["name"]
    });

    let validator = SchemaValidator::new(&schema)?;

    let tests = [
        (r#"{"name": "Alice", "age": 30}"#, true),
        (r#"{"name": "Bob"}"#, true),
        (r#"{"age": 30}"#, false),
        (r#"{"name": "Eve", "age": -5}"#, false),
        ("not json", false),
    ];

    for (input, expected) in tests {
        let content = adk_core::Content::new("model").with_text(input);
        let result = validator.validate(&content).await;
        let status = if result.is_pass() == expected { "✓" } else { "✗" };
        println!("  {} '{}' -> pass={}", status, input, result.is_pass());
    }

    // Test JSON in markdown
    println!("\n--- JSON in Markdown ---\n");

    let markdown = r#"Here is the result:
```json
{"name": "Charlie", "age": 25}
```"#;

    let content = adk_core::Content::new("model").with_text(markdown);
    let result = validator.validate(&content).await;
    println!("  Markdown with JSON -> pass={}", result.is_pass());

    println!("\n=== Complete ===");
    Ok(())
}
