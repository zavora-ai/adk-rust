//! Realtime API example with Tool Calling.
//!
//! This example demonstrates using tools (function calling) with the
//! OpenAI Realtime API. The assistant can call tools to get real-time
//! information during the conversation.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example realtime_tools --features realtime-openai
//! ```

use adk_realtime::{
    RealtimeConfig, RealtimeModel, ServerEvent, ToolResponse, config::ToolDefinition,
    openai::OpenAIRealtimeModel,
};
use serde_json::json;
use std::sync::Arc;

/// Simulated weather data
fn get_weather(location: &str, unit: &str) -> serde_json::Value {
    // In a real app, this would call a weather API
    let temp = match location.to_lowercase().as_str() {
        "new york" | "nyc" => 72,
        "london" => 58,
        "tokyo" => 68,
        "sydney" => 75,
        "paris" => 64,
        _ => 70,
    };

    let temp_display =
        if unit == "celsius" { ((temp - 32) as f32 * 5.0 / 9.0) as i32 } else { temp };

    json!({
        "location": location,
        "temperature": temp_display,
        "unit": unit,
        "condition": "partly cloudy",
        "humidity": 65,
        "wind_speed": 12
    })
}

/// Simulated stock price lookup
fn get_stock_price(symbol: &str) -> serde_json::Value {
    let price = match symbol.to_uppercase().as_str() {
        "AAPL" => 178.50,
        "GOOGL" => 141.25,
        "MSFT" => 378.90,
        "AMZN" => 178.75,
        "TSLA" => 248.30,
        _ => 100.00,
    };

    json!({
        "symbol": symbol.to_uppercase(),
        "price": price,
        "currency": "USD",
        "change": 2.35,
        "change_percent": 1.33
    })
}

/// Execute a tool call and return the result
fn execute_tool(name: &str, arguments: &str) -> serde_json::Value {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or(json!({}));

    match name {
        "get_weather" => {
            let location = args["location"].as_str().unwrap_or("Unknown");
            let unit = args["unit"].as_str().unwrap_or("fahrenheit");
            get_weather(location, unit)
        }
        "get_stock_price" => {
            let symbol = args["symbol"].as_str().unwrap_or("AAPL");
            get_stock_price(symbol)
        }
        _ => json!({ "error": "Unknown tool" }),
    }
}

/// Process a conversation turn, handling tool calls
async fn process_response(
    session: &dyn adk_realtime::RealtimeSession,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut has_tool_calls = false;

    loop {
        let event_result = session.next_event().await;
        if event_result.is_none() {
            return Ok(false);
        }

        match event_result.unwrap() {
            Ok(event) => match event {
                ServerEvent::SessionCreated { .. } => {
                    // Session ready
                }
                ServerEvent::TextDelta { delta, .. } => {
                    print!("{}", delta);
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                }
                ServerEvent::FunctionCallDone { call_id, name, arguments, .. } => {
                    has_tool_calls = true;
                    println!("\n\n[Tool call: {}({})]", name, arguments);

                    // Execute the tool
                    let result = execute_tool(&name, &arguments);
                    println!("[Tool result: {}]", serde_json::to_string(&result)?);

                    // Send the tool response immediately
                    let tool_response = ToolResponse::new(&call_id, result);
                    session.send_tool_response(tool_response).await?;
                }
                ServerEvent::ResponseDone { .. } => {
                    // Response complete
                    if has_tool_calls {
                        // send_tool_response already triggers a new response internally,
                        // so we just continue processing to receive the assistant's reply
                        print!("\nAssistant: ");
                        has_tool_calls = false;
                        // Continue processing for the new response
                    } else {
                        // No tool calls in this response, we're done
                        println!();
                        return Ok(true);
                    }
                }
                ServerEvent::Error { error, .. } => {
                    eprintln!("\nError: {} - {}", error.error_type, error.message);
                    return Ok(false);
                }
                _ => {}
            },
            Err(e) => {
                eprintln!("Error: {}", e);
                return Ok(false);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    println!("=== ADK-Rust Realtime Tool Calling Example ===\n");

    let model = Arc::new(OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17"));

    // Define the tools available to the assistant
    let tools = vec![
        ToolDefinition {
            name: "get_weather".to_string(),
            description: Some("Get current weather for a location".to_string()),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "City name, e.g., 'New York', 'London', 'Tokyo'"
                    },
                    "unit": {
                        "type": "string",
                        "enum": ["fahrenheit", "celsius"],
                        "description": "Temperature unit"
                    }
                },
                "required": ["location"]
            })),
        },
        ToolDefinition {
            name: "get_stock_price".to_string(),
            description: Some("Get current stock price for a symbol".to_string()),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Stock ticker symbol, e.g., 'AAPL', 'GOOGL'"
                    }
                },
                "required": ["symbol"]
            })),
        },
    ];

    let config = RealtimeConfig::default()
        .with_instruction(
            "You are a helpful assistant with access to real-time information. \
             Use the available tools to answer questions about weather and stock prices. \
             Be concise and provide specific numbers when available.",
        )
        .with_tools(tools)
        .with_modalities(vec!["text".to_string()]);

    println!("Connecting to OpenAI Realtime API with tools...");

    let session = model.connect(config).await?;

    println!("Connected! Available tools: get_weather, get_stock_price\n");

    // First query - requires tool use
    let query = "What's the weather like in Tokyo?";
    println!("User: {}\n", query);
    print!("Assistant: ");

    session.send_text(query).await?;
    session.create_response().await?;

    process_response(session.as_ref()).await?;

    // Second query
    println!("\n---\n");

    let query2 = "What's Apple's current stock price?";
    println!("User: {}\n", query2);
    print!("Assistant: ");

    session.send_text(query2).await?;
    session.create_response().await?;

    process_response(session.as_ref()).await?;

    println!("\n=== Session Complete ===");

    Ok(())
}
