//! Guardrails doc-test - validates guardrails.md documentation

use adk_guardrail::{
    ContentFilter, ContentFilterConfig, GuardrailExecutor, GuardrailSet,
    PiiRedactor, PiiType, SchemaValidator, Severity, Guardrail, GuardrailResult,
};
use adk_core::Content;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Guardrails Doc-Test ===\n");

    // From docs: PII Redaction
    let redactor = PiiRedactor::new();
    let (redacted, found_types) = redactor.redact("Email: test@example.com");
    assert!(redacted.contains("[EMAIL REDACTED]"));
    assert!(found_types.contains(&PiiType::Email));
    println!("✓ PII redaction works");

    // From docs: PII with specific types
    let redactor = PiiRedactor::with_types(&[PiiType::Email, PiiType::Phone]);
    let (redacted, _) = redactor.redact("Call 555-123-4567");
    assert!(redacted.contains("[PHONE REDACTED]"));
    println!("✓ PII with specific types works");

    // From docs: Content filtering - blocked keywords
    let filter = ContentFilter::blocked_keywords(vec!["forbidden".into(), "banned".into()]);
    let content = Content::new("user").with_text("This is forbidden content");
    let result = filter.validate(&content).await;
    assert!(result.is_fail());
    println!("✓ Blocked keywords filter works");

    // From docs: Content filtering - on topic
    let filter = ContentFilter::on_topic("cooking", vec![
        "recipe".into(), "cook".into(), "bake".into()
    ]);
    let content = Content::new("user").with_text("Give me a recipe");
    let result = filter.validate(&content).await;
    assert!(result.is_pass());
    println!("✓ On-topic filter works");

    // From docs: Content filtering - max length
    let filter = ContentFilter::max_length(10);
    let content = Content::new("user").with_text("This is too long");
    let result = filter.validate(&content).await;
    assert!(result.is_fail());
    println!("✓ Max length filter works");

    // From docs: Custom ContentFilterConfig
    let config = ContentFilterConfig {
        blocked_keywords: vec!["spam".into()],
        required_topics: vec!["rust".into()],
        max_length: Some(5000),
        min_length: Some(10),
        severity: Severity::High,
    };
    let _filter = ContentFilter::new("custom_filter", config);
    println!("✓ Custom ContentFilterConfig works");

    // From docs: Schema validation
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer", "minimum": 0 }
        },
        "required": ["name"]
    });
    let validator = SchemaValidator::new(&schema)?
        .with_name("user_schema")
        .with_severity(Severity::High);

    let content = Content::new("model").with_text(r#"{"name": "Alice", "age": 30}"#);
    let result = validator.validate(&content).await;
    assert!(result.is_pass());
    println!("✓ Schema validation works");

    // From docs: GuardrailSet
    let guardrails = GuardrailSet::new()
        .with(ContentFilter::harmful_content())
        .with(ContentFilter::max_length(5000))
        .with(PiiRedactor::new());
    assert!(!guardrails.is_empty());
    println!("✓ GuardrailSet works");

    // From docs: GuardrailExecutor
    let guardrails = GuardrailSet::new().with(PiiRedactor::new());
    let content = Content::new("user").with_text("Contact: test@example.com");
    let result = GuardrailExecutor::run(&guardrails, &content).await?;

    assert!(result.passed);
    assert!(result.transformed_content.is_some());
    let transformed = result.transformed_content.unwrap();
    let text: String = transformed.parts.iter().filter_map(|p| p.text()).collect();
    assert!(text.contains("[EMAIL REDACTED]"));
    println!("✓ GuardrailExecutor works");

    // From docs: GuardrailResult variants
    let pass = GuardrailResult::pass();
    assert!(pass.is_pass());

    let fail = GuardrailResult::fail("test reason", Severity::High);
    assert!(fail.is_fail());
    println!("✓ GuardrailResult variants work");

    println!("\n=== All guardrails tests passed! ===");
    Ok(())
}
