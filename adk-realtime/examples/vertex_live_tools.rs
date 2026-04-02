//! # Vertex AI Live Voice Agent with Tool Calling
//!
//! Demonstrates a Gemini Live voice agent running on Vertex AI that can call
//! tools during a realtime conversation. The agent has access to a weather
//! tool and a time tool, showing how function calling works over a
//! bidirectional streaming session.
//!
//! This example showcases:
//!
//! - Vertex AI Live backend with Application Default Credentials
//! - Tool/function declarations via [`ToolDefinition`]
//! - Handling [`ServerEvent::FunctionCallDone`] events
//! - Sending [`ToolResponse`] back to the model
//! - The full request → tool call → tool response → final answer loop
//!
//! ## Prerequisites
//!
//! 1. A Google Cloud project with the **Vertex AI API** enabled.
//! 2. Application Default Credentials configured:
//!    ```sh
//!    gcloud auth application-default login
//!    ```
//! 3. The `vertex-live` feature enabled for `adk-realtime`.
//!
//! ## Environment Variables
//!
//! | Variable               | Required | Description                            |
//! |------------------------|----------|----------------------------------------|
//! | `GOOGLE_CLOUD_PROJECT` | **Yes**  | Your Google Cloud project ID           |
//! | `GOOGLE_CLOUD_REGION`  | No       | GCP region (defaults to `us-central1`) |
//!
//! ## Running
//!
//! ```sh
//! cargo run -p adk-realtime --example vertex_live_tools --features vertex-live
//! ```

use adk_realtime::config::ToolDefinition;
use adk_realtime::events::ToolResponse;
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::{RealtimeConfig, RealtimeModel, ServerEvent};
use serde_json::json;

// ── Tool implementations ────────────────────────────────────────────────

/// Simulated weather lookup. In a real app this would call a weather API.
fn get_weather(city: &str) -> String {
    // Simulated responses for demo purposes
    match city.to_lowercase().as_str() {
        "nairobi" => json!({
            "city": "Nairobi",
            "temperature_c": 22,
            "condition": "Partly cloudy",
            "humidity_pct": 65
        }),
        "san francisco" => json!({
            "city": "San Francisco",
            "temperature_c": 15,
            "condition": "Foggy",
            "humidity_pct": 80
        }),
        "tokyo" => json!({
            "city": "Tokyo",
            "temperature_c": 28,
            "condition": "Sunny",
            "humidity_pct": 55
        }),
        _ => json!({
            "city": city,
            "temperature_c": 20,
            "condition": "Clear",
            "humidity_pct": 50
        }),
    }
    .to_string()
}

/// Returns the current time in a given timezone. Simulated for demo.
fn get_current_time(timezone: &str) -> String {
    let tz = timezone.to_lowercase();
    let (offset, label) = match tz.as_str() {
        "eat" | "africa/nairobi" => ("+03:00", "East Africa Time"),
        "pst" | "america/los_angeles" => ("-08:00", "Pacific Standard Time"),
        "jst" | "asia/tokyo" => ("+09:00", "Japan Standard Time"),
        "utc" | "gmt" => ("+00:00", "UTC"),
        _ => ("+00:00", "UTC"),
    };
    json!({
        "timezone": label,
        "utc_offset": offset,
        "current_time": "2026-02-15T12:00:00",
        "note": "Simulated time for demo"
    })
    .to_string()
}

/// Dispatch a tool call by name and return the result string.
fn execute_tool(name: &str, arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or(json!({}));

    match name {
        "get_weather" => {
            let city = args["city"].as_str().unwrap_or("Unknown");
            get_weather(city)
        }
        "get_current_time" => {
            let timezone = args["timezone"].as_str().unwrap_or("UTC");
            get_current_time(timezone)
        }
        _ => json!({"error": format!("Unknown tool: {}", name)}).to_string(),
    }
}

// ── Tool definitions ────────────────────────────────────────────────────

fn weather_tool() -> ToolDefinition {
    ToolDefinition::new("get_weather")
        .with_description("Get the current weather for a city.")
        .with_parameters(json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "The city name, e.g. 'Nairobi', 'San Francisco'"
                }
            },
            "required": ["city"]
        }))
}

fn time_tool() -> ToolDefinition {
    ToolDefinition::new("get_current_time")
        .with_description("Get the current time in a given timezone.")
        .with_parameters(json!({
            "type": "object",
            "properties": {
                "timezone": {
                    "type": "string",
                    "description": "IANA timezone name or abbreviation, e.g. 'America/Los_Angeles', 'EAT', 'JST'"
                }
            },
            "required": ["timezone"]
        }))
}

// ── Main ────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // --- 1. Build the Vertex AI Live backend via ADC ---
    let project_id =
        std::env::var("GOOGLE_CLOUD_PROJECT").expect("GOOGLE_CLOUD_PROJECT env var is required");
    let region = std::env::var("GOOGLE_CLOUD_REGION").unwrap_or_else(|_| "us-central1".into());

    let model_id = "gemini-3.1-flash-live-preview";
    let backend = GeminiLiveBackend::vertex_adc(project_id, region)?;

    // --- 2. Create the model ---
    let model = GeminiRealtimeModel::new(backend, model_id);

    // --- 3. Configure the session with tools ---
    let config = RealtimeConfig::default()
        .with_instruction(
            "You are a helpful voice assistant with access to weather and time tools. \
             When asked about weather or time, use the appropriate tool. \
             Keep responses concise and conversational.",
        )
        .with_tool(weather_tool())
        .with_tool(time_tool());

    // --- 4. Connect ---
    println!("Connecting to Vertex AI Live (Gemini)...");
    let session = model.connect(config).await?;
    println!("Connected! Session ID: {}\n", session.session_id());

    // --- 5. Send a prompt that should trigger tool use ---
    let prompt = "What's the weather like in Nairobi right now, and what time is it there?";
    println!("User: {prompt}\n");
    session.send_text(prompt).await?;

    // --- 6. Event loop with tool calling ---
    let mut full_text = String::new();

    loop {
        let event = match session.next_event().await {
            Some(Ok(ev)) => ev,
            Some(Err(e)) => {
                eprintln!("Error: {e}");
                break;
            }
            None => break,
        };

        match event {
            ServerEvent::FunctionCallDone { name, arguments, call_id, .. } => {
                println!("🔧 Tool call: {name}({arguments})");

                // Execute the tool
                let result = execute_tool(&name, &arguments);
                println!("   → Result: {result}");

                // Send the result back to the model
                let response = ToolResponse::from_string(call_id, result);
                session.send_tool_response(response).await?;
                println!("   → Sent tool response, waiting for model to continue...\n");
            }

            ServerEvent::TextDelta { delta, .. } => {
                print!("{delta}");
                full_text.push_str(&delta);
            }

            ServerEvent::AudioDelta { delta, .. } => {
                // In a real app you'd play this audio.
                // Here we just log the chunk size.
                print!("🔊");
                let _ = delta.len();
            }

            ServerEvent::TranscriptDelta { delta, .. } => {
                print!("{delta}");
            }

            ServerEvent::ResponseDone { .. } => {
                println!("\n\n--- Response complete ---");
                break;
            }

            ServerEvent::Error { error, .. } => {
                eprintln!("\nServer error: {} - {}", error.error_type, error.message);
                break;
            }

            _ => {}
        }
    }

    if !full_text.is_empty() {
        println!("\nFull text response: {full_text}");
    }

    // --- 7. Clean up ---
    session.close().await?;
    println!("Session closed.");
    Ok(())
}
