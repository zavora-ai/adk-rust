//! Structured JSON output with the Anthropic Messages API.
//!
//! Demonstrates: OutputConfig with JSON schema for guaranteed schema-conformant
//! responses. Also shows the effort parameter inside OutputConfig.
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run`

use adk_anthropic::{
    Anthropic, EffortLevel, KnownModel, MessageCreateParams, OutputConfig, OutputFormat,
};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    // --- JSON Schema output ---
    println!("=== Structured Output (JSON Schema) ===\n");

    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "capital": { "type": "string" },
            "population": { "type": "integer" },
            "languages": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["name", "capital", "population", "languages"],
        "additionalProperties": false
    });

    let mut params =
        MessageCreateParams::simple("Give me information about Japan.", KnownModel::ClaudeSonnet46);
    params.output_config = Some(OutputConfig::new(OutputFormat::json_schema(schema)));

    let response = client.send(params).await?;

    for block in &response.content {
        if let Some(text) = block.as_text() {
            let parsed: serde_json::Value = serde_json::from_str(&text.text)?;
            println!("{}", serde_json::to_string_pretty(&parsed)?);
        }
    }

    // --- Effort parameter inside OutputConfig ---
    println!("\n=== OutputConfig with Effort Level ===\n");

    let mut params = MessageCreateParams::simple(
        "What are the three laws of thermodynamics?",
        KnownModel::ClaudeSonnet46,
    );
    params.output_config = Some(OutputConfig::with_effort(EffortLevel::Low));

    let response = client.send(params).await?;

    for block in &response.content {
        if let Some(text) = block.as_text() {
            println!("{}", text.text);
        }
    }

    println!(
        "\nUsage: {} in / {} out tokens",
        response.usage.input_tokens, response.usage.output_tokens
    );

    Ok(())
}
