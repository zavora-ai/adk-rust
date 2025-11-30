use display_error_chain::DisplayErrorChain;
use gemini_rust::{
    Content, FunctionCallingMode, FunctionDeclaration, Gemini, GenerationConfig, Message, Role,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::env;
use std::process::ExitCode;
use tracing::{info, warn};

#[derive(Default, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "The unit of temperature")]
#[serde(rename_all = "lowercase")]
enum Unit {
    #[default]
    Celsius,
    Fahrenheit,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
struct WeatherRequest {
    /// The city and state, e.g., San Francisco, CA
    location: String,
    /// The unit of temperature
    unit: Option<Unit>,
}

impl Default for WeatherRequest {
    fn default() -> Self {
        WeatherRequest {
            location: "".to_string(),
            unit: Some(Unit::Celsius),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct WeatherResponse {
    temperature: i32,
    unit: String,
    condition: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    match do_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let error_chain = DisplayErrorChain::new(e.as_ref());
            tracing::error!(error.debug = ?e, error.chained = %error_chain, "execution failed");
            ExitCode::FAILURE
        }
    }
}

async fn do_main() -> Result<(), Box<dyn std::error::Error>> {
    // Get API key from environment variable
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable not set");

    // Create client
    let client = Gemini::new(api_key).expect("unable to create Gemini API client");

    // Simple generation
    info!("starting simple generation");
    let response = client
        .generate_content()
        .with_user_message("Hello, can you tell me a joke about programming?")
        .with_generation_config(GenerationConfig {
            temperature: Some(0.7),
            max_output_tokens: Some(5000),
            ..Default::default()
        })
        .execute()
        .await?;

    info!(response = response.text(), "simple generation completed");

    // Function calling example
    info!("starting function calling example");

    // Define a weather function
    let get_weather = FunctionDeclaration::new(
        "get_weather",
        "Get the current weather for a location",
        None,
    )
    .with_parameters::<WeatherRequest>()
    .with_response::<WeatherResponse>();

    // Create a request with function calling
    let response = client
        .generate_content()
        .with_system_prompt("You are a helpful weather assistant.")
        .with_user_message("What's the weather like in San Francisco right now?")
        .with_function(get_weather)
        .with_function_calling_mode(FunctionCallingMode::Any)
        .execute()
        .await?;

    // Check if there are function calls
    if let Some(function_call) = response.function_calls().first() {
        info!(
            function_name = function_call.name,
            args = function_call.args.to_string(),
            "function call received"
        );

        // Parse the function call arguments
        let weather_request: WeatherRequest = serde_json::from_value(function_call.args.clone())?;

        info!(
            location = weather_request.location,
            unit = ?weather_request.unit,
            "function call parameters extracted"
        );

        let unit_str = match weather_request.unit.unwrap_or_default() {
            Unit::Celsius => "celsius",
            Unit::Fahrenheit => "fahrenheit",
        };

        // Create model content with function call
        let model_content = Content::function_call((*function_call).clone());

        // Add as model message
        let model_message = Message {
            content: model_content,
            role: Role::Model,
        };

        // Simulate function execution
        let weather_response = format!(
            "{{\"temperature\": 22, \"unit\": \"{}\", \"condition\": \"sunny\"}}",
            unit_str
        );
        info!(response = weather_response, "simulated function response");

        // Continue the conversation with the function result
        let final_response = client
            .generate_content()
            .with_system_prompt("You are a helpful weather assistant.")
            .with_user_message("What's the weather like in San Francisco right now?")
            .with_message(model_message)
            .with_function_response_str("get_weather", weather_response)?
            .with_generation_config(GenerationConfig {
                temperature: Some(0.7),
                max_output_tokens: Some(100),
                ..Default::default()
            })
            .execute()
            .await?;

        info!(
            final_response = final_response.text(),
            "function calling completed"
        );
    } else {
        warn!("no function calls in the response");
    }

    Ok(())
}
