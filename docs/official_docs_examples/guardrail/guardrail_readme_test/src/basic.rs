//! Validates adk-guardrail README examples compile correctly

use adk_guardrail::{
    PiiRedactor, ContentFilter, GuardrailSet, Guardrail,
    GuardrailResult, Severity,
};
use adk_agent::LlmAgentBuilder;
use adk_core::Content;

// Validate: PiiRedactor
async fn _pii_redactor_example() {
    let redactor = PiiRedactor::new();
    let content = Content::new("user").with_text("Contact me at test@example.com");
    let _result = redactor.validate(&content).await;
}

// Validate: ContentFilter factory methods
fn _content_filter_examples() {
    let _filter1 = ContentFilter::harmful_content();
    let _filter2 = ContentFilter::on_topic("cooking", vec!["recipe".into(), "bake".into()]);
    let _filter3 = ContentFilter::max_length(1000);
    let _filter4 = ContentFilter::blocked_keywords(vec!["forbidden".into()]);
}

// Validate: GuardrailSet builder
fn _guardrail_set_example() {
    let _set = GuardrailSet::new()
        .with(ContentFilter::harmful_content())
        .with(PiiRedactor::new());
}

// Validate: Agent integration
fn _agent_integration_example() {
    let input_guardrails = GuardrailSet::new()
        .with(ContentFilter::harmful_content())
        .with(PiiRedactor::new());

    let _agent = LlmAgentBuilder::new("assistant")
        .input_guardrails(input_guardrails)
        .build();
}

// Validate: GuardrailResult variants
fn _guardrail_result_examples() {
    let _pass = GuardrailResult::pass();
    let _fail = GuardrailResult::fail("reason", Severity::High);
    let _transform = GuardrailResult::transform(
        Content::new("user").with_text("redacted"),
        "PII removed"
    );
}

// Validate: Severity levels
fn _severity_examples() {
    let _low = Severity::Low;
    let _medium = Severity::Medium;
    let _high = Severity::High;
    let _critical = Severity::Critical;
}

fn main() {
    println!("✓ PiiRedactor::new() compiles");
    println!("✓ Guardrail::validate() compiles");
    println!("✓ ContentFilter::harmful_content() compiles");
    println!("✓ ContentFilter::on_topic() compiles");
    println!("✓ ContentFilter::max_length() compiles");
    println!("✓ ContentFilter::blocked_keywords() compiles");
    println!("✓ GuardrailSet::new() compiles");
    println!("✓ .with() compiles");
    println!("✓ LlmAgentBuilder::input_guardrails() compiles");
    println!("✓ GuardrailResult::pass() compiles");
    println!("✓ GuardrailResult::fail() compiles");
    println!("✓ GuardrailResult::transform() compiles");
    println!("✓ Severity variants compile");
    println!("\nadk-guardrail README validation passed!");
}
