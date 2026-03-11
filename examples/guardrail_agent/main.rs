//! Agent with Guardrails Example
//!
//! Demonstrates attaching guardrails to an LlmAgent.
//!
//! Run with: cargo run --example guardrail_agent --features "guardrails"
//!
//! Requires: GOOGLE_API_KEY or GEMINI_API_KEY

use adk_agent::{
    Agent, LlmAgentBuilder,
    guardrails::{ContentFilter, GuardrailSet, PiiRedactor},
};
use adk_model::GeminiModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Agent with Guardrails ===\n");

    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
    {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY or GEMINI_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example guardrail_agent --features guardrails");
            return Ok(());
        }
    };

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Input guardrails: block harmful content, redact PII
    let input_guardrails =
        GuardrailSet::new().with(ContentFilter::harmful_content()).with(PiiRedactor::new());

    // Output guardrails: limit response length
    let output_guardrails = GuardrailSet::new().with(ContentFilter::max_length(2000));

    let agent = LlmAgentBuilder::new("guarded_assistant")
        .description("Assistant with safety guardrails")
        .instruction("You are a helpful assistant. Be concise.")
        .model(model)
        .input_guardrails(input_guardrails)
        .output_guardrails(output_guardrails)
        .build()?;

    println!("Agent '{}' created with guardrails:", agent.name());
    println!("  Input: harmful_content filter, PII redactor");
    println!("  Output: max_length(2000)");

    println!("Guardrails are enforced during agent execution.");

    println!("\n=== Complete ===");
    Ok(())
}
