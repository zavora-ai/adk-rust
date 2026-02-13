use adk_gemini::{
    Content, FunctionCallingMode, FunctionDeclaration, Gemini, GenerationResponse, Message, Role,
    Tool,
};
use display_error_chain::DisplayErrorChain;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::env;
use std::process::ExitCode;
use tracing::info;

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
struct Weather {
    /// The city and state, e.g., San Francisco, CA
    location: String,
    /// The unit of temperature
    unit: Option<Unit>,
}

impl Default for Weather {
    fn default() -> Self {
        Weather { location: "".to_string(), unit: Some(Unit::Celsius) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct WeatherResponse {
    temperature: i32,
    unit: String,
    condition: String,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "The mathematical operation to perform")]
#[serde(rename_all = "lowercase")]
enum Operation {
    #[default]
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
struct Calculation {
    /// The mathematical operation to perform
    operation: Operation,
    /// The first number
    a: f64,
    /// The second number
    b: f64,
}

impl Default for Calculation {
    fn default() -> Self {
        Calculation { operation: Operation::Add, a: 0.0, b: 0.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CalculationResponse {
    result: f64,
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

    info!("starting tools example with multiple functions");

    // Define a weather function
    let get_weather =
        FunctionDeclaration::new("get_weather", "Get the current weather for a location", None)
            .with_parameters::<Weather>()
            .with_response::<WeatherResponse>();

    // Define a calculator function
    let calculate = FunctionDeclaration::new("calculate", "Perform a calculation", None)
        .with_parameters::<Calculation>()
        .with_response::<CalculationResponse>();

    // Create a tool with multiple functions
    let tool = Tool::with_functions(vec![get_weather, calculate]);

    // Create a request with tool functions
    let response: GenerationResponse = client
        .generate_content()
        .with_system_prompt(
            "You are a helpful assistant that can check weather and perform calculations.",
        )
        .with_user_message("What's 42 times 12?")
        .with_tool(tool)
        .with_function_calling_mode(FunctionCallingMode::Any)
        .execute()
        .await?;

    // Process function calls
    if let Some(function_call) = response.function_calls().first() {
        info!(
            function_name = function_call.name,
            args = ?function_call.args,
            "function call received"
        );

        // Handle different function calls
        match function_call.name.as_str() {
            "calculate" => {
                let calculation: Calculation = serde_json::from_value(function_call.args.clone())?;

                info!(
                    operation = ?calculation.operation,
                    a = calculation.a,
                    b = calculation.b,
                    "performing calculation"
                );

                let result = match calculation.operation {
                    Operation::Add => calculation.a + calculation.b,
                    Operation::Subtract => calculation.a - calculation.b,
                    Operation::Multiply => calculation.a * calculation.b,
                    Operation::Divide => calculation.a / calculation.b,
                };

                let function_response = CalculationResponse { result };

                // Based on the curl example, we need to structure the conversation properly:
                // 1. A user message with the original query
                // 2. A model message containing the function call
                // 3. A user message containing the function response

                // Construct conversation following the exact curl pattern
                let mut conversation = client.generate_content();

                // 1. Add user message with original query and system prompt
                conversation = conversation
                    .with_system_prompt("You are a helpful assistant that can check weather and perform calculations.")
                    .with_user_message("What's 42 times 12?");

                // 2. Create model content with function call
                let model_content = Content::function_call((*function_call).clone());

                // Add as model message
                let model_message = Message { content: model_content, role: Role::Model };
                conversation = conversation.with_message(model_message);

                // 3. Add user message with function response
                conversation =
                    conversation.with_function_response("calculate", function_response)?;

                // Execute the request
                let final_response: GenerationResponse = conversation.execute().await?;

                info!(response = final_response.text(), "final response received");
            }
            "get_weather" => {
                let weather: Weather = serde_json::from_value(function_call.args.clone())?;

                info!(
                    location = weather.location,
                    unit = ?weather.unit,
                    "weather request received"
                );

                let unit_str = match weather.unit.unwrap_or_default() {
                    Unit::Celsius => "celsius",
                    Unit::Fahrenheit => "fahrenheit",
                };

                let weather_response = WeatherResponse {
                    temperature: 22,
                    unit: unit_str.to_string(),
                    condition: "sunny".to_string(),
                };

                // Based on the curl example, we need to structure the conversation properly:
                // 1. A user message with the original query
                // 2. A model message containing the function call
                // 3. A user message containing the function response

                // Construct conversation following the exact curl pattern
                let mut conversation = client.generate_content();

                // 1. Add user message with original query and system prompt
                conversation = conversation
                    .with_system_prompt("You are a helpful assistant that can check weather and perform calculations.")
                    .with_user_message("What's 42 times 12?");

                // 2. Create model content with function call
                let model_content = Content::function_call((*function_call).clone());

                // Add as model message
                let model_message = Message { content: model_content, role: Role::Model };
                conversation = conversation.with_message(model_message);

                // 3. Add user message with function response
                conversation =
                    conversation.with_function_response("get_weather", weather_response)?;

                // Execute the request
                let final_response: GenerationResponse = conversation.execute().await?;

                info!(response = final_response.text(), "final response received");
            }
            _ => info!(function_name = function_call.name, "unknown function call"),
        }
    } else {
        info!("no function calls in response");
        info!(response = response.text(), "direct response received");
    }

    Ok(())
}
