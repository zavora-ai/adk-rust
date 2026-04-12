//! Gemini 3 Function Calling Features
//!
//! Demonstrates the Gemini 3 series function calling enhancements:
//! - `VALIDATED` mode: schema validation without forced calling
//! - `allowed_function_names`: restrict which functions the model may call
//! - Function call `id` field: unique identifier per call for correlation
//!
//! Run: `cargo run -p adk-gemini --example gemini3_function_calling`
//! Requires: `GEMINI_API_KEY` environment variable

use adk_gemini::{
    Content, FunctionCallingMode, FunctionDeclaration, Gemini, GenerationConfig, Message, Part,
    Role,
};
use display_error_chain::DisplayErrorChain;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::env;
use std::process::ExitCode;
use tracing::info;

// ── Tool parameter types ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct WeatherParams {
    /// City name, e.g. "Tokyo"
    location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct StockParams {
    /// Stock ticker symbol, e.g. "GOOG"
    symbol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct NewsParams {
    /// Topic to search for
    topic: String,
    /// Maximum number of results
    max_results: Option<u32>,
}

// ── Simulated tool execution ──────────────────────────────────────────

fn execute_weather(params: &WeatherParams) -> serde_json::Value {
    serde_json::json!({
        "temperature": 24,
        "unit": "celsius",
        "condition": "partly cloudy",
        "location": params.location
    })
}

fn execute_stock(params: &StockParams) -> serde_json::Value {
    serde_json::json!({
        "symbol": params.symbol,
        "price": 182.45,
        "currency": "USD",
        "change": "+1.23%"
    })
}

fn execute_news(params: &NewsParams) -> serde_json::Value {
    serde_json::json!({
        "articles": [
            {"title": format!("Latest on {}", params.topic), "source": "Reuters"},
            {"title": format!("{} update", params.topic), "source": "AP News"},
        ]
    })
}

// ── Main ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let chain = DisplayErrorChain::new(e.as_ref());
            tracing::error!(error = %chain, "execution failed");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let client = Gemini::new(api_key)?;

    // Define three tools
    let weather_fn =
        FunctionDeclaration::new("get_weather", "Get current weather for a city", None)
            .with_parameters::<WeatherParams>();

    let stock_fn =
        FunctionDeclaration::new("get_stock_price", "Get current stock price by ticker", None)
            .with_parameters::<StockParams>();

    let news_fn =
        FunctionDeclaration::new("search_news", "Search recent news articles by topic", None)
            .with_parameters::<NewsParams>();

    // ── Demo 1: VALIDATED mode ────────────────────────────────────────
    // The model validates calls against the schema but is not forced to call.
    // It may respond with text if it can answer directly.
    info!("── Demo 1: VALIDATED mode ──");
    info!("asking a question the model can answer without tools...");

    let response = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant with access to tools. Only use tools when you need real-time data.")
        .with_user_message("What is the capital of France?")
        .with_function(weather_fn.clone())
        .with_function(stock_fn.clone())
        .with_function_calling_mode(FunctionCallingMode::Validated)
        .with_generation_config(GenerationConfig {
            temperature: Some(0.1),
            max_output_tokens: Some(200),
            ..Default::default()
        })
        .execute()
        .await?;

    let calls = response.function_calls();
    if calls.is_empty() {
        info!(
            text = response.text(),
            "model answered directly (no tool call) — VALIDATED mode allows this"
        );
    } else {
        info!(count = calls.len(), "model chose to call tools anyway");
    }

    // ── Demo 2: allowed_function_names ────────────────────────────────
    // Restrict the model to only call get_weather, even though all three tools are declared.
    info!("\n── Demo 2: allowed_function_names ──");
    info!("three tools declared, but only get_weather is allowed...");

    let response = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant.")
        .with_user_message("What's the weather in Berlin and what's the latest tech news?")
        .with_function(weather_fn.clone())
        .with_function(stock_fn.clone())
        .with_function(news_fn.clone())
        .with_function_calling_mode_restricted(
            FunctionCallingMode::Any,
            vec!["get_weather".to_string()],
        )
        .with_generation_config(GenerationConfig {
            temperature: Some(0.1),
            max_output_tokens: Some(200),
            ..Default::default()
        })
        .execute()
        .await?;

    for call in response.function_calls() {
        info!(
            name = call.name,
            id = ?call.id,
            args = %call.args,
            "function call received — should only be get_weather"
        );
        assert_eq!(
            call.name, "get_weather",
            "model should only call get_weather due to allowed_function_names"
        );
    }

    // ── Demo 3: Function call id + multi-turn ─────────────────────────
    // Gemini 3 returns an `id` on each function call for correlation.
    info!("\n── Demo 3: Function call id + parallel calls ──");
    info!("asking for weather, stock price, AND news to trigger parallel calls...");

    let response = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant. When asked about multiple things, call all relevant tools.")
        .with_user_message("What's the weather in Tokyo, what's Google's stock price, and what's the latest AI news?")
        .with_function(weather_fn.clone())
        .with_function(stock_fn.clone())
        .with_function(news_fn.clone())
        .with_function_calling_mode(FunctionCallingMode::Any)
        .with_generation_config(GenerationConfig {
            temperature: Some(0.1),
            max_output_tokens: Some(500),
            ..Default::default()
        })
        .execute()
        .await?;

    let calls = response.function_calls();
    info!(count = calls.len(), "function calls received");

    // Log each call with its id
    for call in &calls {
        info!(
            name = call.name,
            id = ?call.id,
            args = %call.args,
            "call details"
        );
    }

    // Execute each tool and build function responses
    let mut response_parts = Vec::new();
    let mut call_parts = Vec::new();

    for call in &calls {
        // Preserve the function call part (with id) for the model turn
        call_parts
            .push(Part::FunctionCall { function_call: (*call).clone(), thought_signature: None });

        // Execute the tool
        let result = match call.name.as_str() {
            "get_weather" => {
                let params: WeatherParams = serde_json::from_value(call.args.clone())?;
                execute_weather(&params)
            }
            "get_stock_price" => {
                let params: StockParams = serde_json::from_value(call.args.clone())?;
                execute_stock(&params)
            }
            "search_news" => {
                let params: NewsParams = serde_json::from_value(call.args.clone())?;
                execute_news(&params)
            }
            other => serde_json::json!({"error": format!("unknown function: {other}")}),
        };

        info!(name = call.name, id = ?call.id, result = %result, "tool executed");

        response_parts.push(Part::FunctionResponse {
            function_response: adk_gemini::FunctionResponse::new(&call.name, result),
            thought_signature: None,
        });
    }

    // Build the multi-turn conversation with all function responses in one turn
    let model_content = Content { parts: Some(call_parts), role: Some(Role::Model) };
    let fn_content = Content { parts: Some(response_parts), role: Some(Role::User) };

    let final_response = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant.")
        .with_user_message("What's the weather in Tokyo, what's Google's stock price, and what's the latest AI news?")
        .with_message(Message { content: model_content, role: Role::Model })
        .with_message(Message { content: fn_content, role: Role::User })
        .with_generation_config(GenerationConfig {
            temperature: Some(0.7),
            max_output_tokens: Some(500),
            ..Default::default()
        })
        .execute()
        .await?;

    info!(response = final_response.text(), "final response with both tool results");

    info!("\n✅ all demos completed successfully");
    Ok(())
}
