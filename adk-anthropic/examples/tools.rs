//! Tool calling with the Anthropic Messages API.
//!
//! Demonstrates: defining tools, handling tool_use responses, sending tool
//! results back, and getting a final answer.
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run`

use adk_anthropic::{
    Anthropic, ContentBlock, KnownModel, MessageCreateParams, MessageParam, MessageRole, ToolParam,
    ToolResultBlock, ToolUnionParam,
};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    // Define a weather tool
    let weather_tool = ToolParam::new(
        "get_weather".to_string(),
        json!({
            "type": "object",
            "properties": {
                "city": { "type": "string", "description": "City name" }
            },
            "required": ["city"]
        }),
    )
    .with_description("Get the current weather for a city.".to_string());

    let tools = vec![ToolUnionParam::new_custom_tool(
        weather_tool.name.clone(),
        weather_tool.input_schema.clone(),
    )];

    // Step 1: Ask Claude about the weather
    let params = MessageCreateParams::simple(
        "What's the weather like in Tokyo?",
        KnownModel::ClaudeSonnet46,
    )
    .with_tools(tools.clone());

    println!("=== Step 1: Sending request with tool ===\n");
    let response = client.send(params).await?;
    println!("Stop reason: {:?}", response.stop_reason);

    // Step 2: Extract tool use and simulate execution
    let mut tool_results = Vec::new();
    for block in &response.content {
        if let Some(tool_use) = block.as_tool_use() {
            println!("Tool call: {} with input: {}", tool_use.name, tool_use.input);

            // Simulate tool execution
            let result = json!({
                "temperature": "22°C",
                "condition": "Partly cloudy",
                "humidity": "65%"
            });

            tool_results.push(ContentBlock::ToolResult(
                ToolResultBlock::new(tool_use.id.clone()).with_string_content(result.to_string()),
            ));
        }
    }

    if tool_results.is_empty() {
        println!("No tool calls made.");
        return Ok(());
    }

    // Step 3: Send tool results back
    println!("\n=== Step 2: Sending tool results ===\n");

    let messages = vec![
        MessageParam::new_with_string(
            "What's the weather like in Tokyo?".to_string(),
            MessageRole::User,
        ),
        MessageParam::new_with_blocks(response.content, MessageRole::Assistant),
        MessageParam::new_with_blocks(tool_results, MessageRole::User),
    ];

    let params = MessageCreateParams::new(1024, messages, KnownModel::ClaudeSonnet46.into())
        .with_tools(tools);

    let final_response = client.send(params).await?;

    for block in &final_response.content {
        if let Some(text) = block.as_text() {
            println!("{}", text.text);
        }
    }

    Ok(())
}
