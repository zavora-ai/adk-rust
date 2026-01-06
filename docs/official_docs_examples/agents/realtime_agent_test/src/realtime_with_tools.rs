//! Realtime with Tools Example
//! 
//! Demonstrates tool calling during realtime sessions.

use adk_realtime::{
    openai::OpenAIRealtimeModel,
    config::ToolDefinition,
    RealtimeConfig, RealtimeModel, ServerEvent, ToolResponse,
};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY")?;

    println!("🔧 Realtime with Tools Example");
    println!("This demonstrates tool calling during realtime sessions\n");

    // Create the realtime model
    let model = OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17");

    // Define tools
    let tools = vec![
        ToolDefinition {
            name: "get_weather".to_string(),
            description: Some("Get the current weather for a location".to_string()),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "The city name"
                    }
                },
                "required": ["location"]
            })),
        },
        ToolDefinition {
            name: "calculator".to_string(),
            description: Some("Perform mathematical calculations".to_string()),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "expression": {
                        "type": "string",
                        "description": "The math expression to evaluate"
                    }
                },
                "required": ["expression"]
            })),
        },
    ];

    // Configure the session with tools
    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful assistant with access to weather and calculator tools. Use them when needed.")
        .with_voice("alloy")
        .with_modalities(vec!["text".to_string()])
        .with_tools(tools);

    println!("📡 Connecting with tools enabled...");
    let session = model.connect(config).await?;
    println!("✅ Connected!\n");

    // Ask a question that requires tool use
    let message = "What's the weather in Tokyo?";
    println!("👤 User: {}", message);
    session.send_text(message).await?;
    session.create_response().await?;

    // Process events and handle tool calls
    loop {
        match session.next_event().await {
            Some(Ok(event)) => match event {
                ServerEvent::TextDelta { delta, .. } => {
                    print!("{}", delta);
                }
                ServerEvent::FunctionCallDone { call_id, name, arguments, .. } => {
                    println!("\n🔧 Tool called: {} with args: {}", name, arguments);
                    
                    // Execute the tool
                    let result = execute_tool(&name, &arguments);
                    println!("📤 Tool result: {}", result);
                    
                    // Send the response back
                    let response = ToolResponse::new(&call_id, result);
                    session.send_tool_response(response).await?;
                    session.create_response().await?;
                }
                ServerEvent::ResponseDone { .. } => {
                    println!();
                    break;
                }
                ServerEvent::Error { error, .. } => {
                    println!("\n❌ Error: {:?}", error);
                    break;
                }
                _ => {}
            },
            Some(Err(e)) => {
                println!("❌ Error: {}", e);
                break;
            }
            None => break,
        }
    }

    println!("\n✅ Tool calling demonstration complete!");
    Ok(())
}

fn execute_tool(name: &str, arguments: &str) -> serde_json::Value {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    
    match name {
        "get_weather" => {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("unknown");
            json!({
                "location": location,
                "temperature": "72°F",
                "condition": "Sunny",
                "humidity": "45%"
            })
        }
        "calculator" => {
            let expr = args.get("expression").and_then(|v| v.as_str()).unwrap_or("0");
            let result = match expr {
                "2 + 2" => "4",
                "10 * 5" => "50",
                "100 / 4" => "25",
                _ => "Unable to evaluate",
            };
            json!({ "result": result, "expression": expr })
        }
        _ => json!({ "error": "Unknown tool" })
    }
}
