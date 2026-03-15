//! Basic Guardrails Example
//!
//! Demonstrates PII redaction and content filtering.
//!
//! Run with: cargo run --example guardrail_basic --features "guardrails"

use adk_guardrail::{ContentFilter, Guardrail, GuardrailExecutor, GuardrailSet, PiiRedactor};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Basic Guardrails Example ===\n");

    // === PII Redaction ===
    println!("--- PII Redaction ---\n");

    let pii_redactor = PiiRedactor::new();
    let tests = [
        "My email is john@example.com",
        "Call me at 555-123-4567",
        "SSN: 123-45-6789",
        "Hello, how are you?",
    ];

    for text in tests {
        let (redacted, found) = pii_redactor.redact(text);
        if found.is_empty() {
            println!("  '{}' -> No PII", text);
        } else {
            println!("  '{}' -> '{}'", text, redacted);
        }
    }

    // === Content Filtering ===
    println!("\n--- Content Filtering ---\n");

    let filter = ContentFilter::blocked_keywords(vec!["forbidden".into(), "banned".into()]);
    let content = adk_core::Content::new("user").with_text("This is forbidden content");
    let result = filter.validate(&content).await;
    println!("  'forbidden content' -> pass={}", result.is_pass());

    let content = adk_core::Content::new("user").with_text("This is normal content");
    let result = filter.validate(&content).await;
    println!("  'normal content' -> pass={}", result.is_pass());

    // === Combined Guardrails ===
    println!("\n--- Combined Guardrails ---\n");

    let guardrails =
        GuardrailSet::new().with(ContentFilter::max_length(50)).with(PiiRedactor::new());

    let content = adk_core::Content::new("user").with_text("Contact: test@example.com");
    let result = GuardrailExecutor::run(&guardrails, &content).await?;

    println!("  Input: 'Contact: test@example.com'");
    println!("  Passed: {}", result.passed);
    if let Some(transformed) = result.transformed_content {
        let text = transformed.parts.iter().filter_map(|p| p.text()).collect::<String>();
        println!("  Transformed: '{}'", text);
    }

    println!("\n=== Complete ===");
    Ok(())
}
