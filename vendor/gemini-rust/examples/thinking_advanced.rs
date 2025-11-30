use display_error_chain::DisplayErrorChain;
use futures::TryStreamExt;
use gemini_rust::{FunctionDeclaration, Gemini, ThinkingConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::env;
use std::process::ExitCode;
use tracing::info;

#[derive(Default, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Type of calculation")]
#[serde(rename_all = "lowercase")]
enum OperationType {
    #[default]
    Arithmetic,
    Advanced,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
struct CalculationRequest {
    /// The mathematical expression to calculate, e.g., '2 + 3 * 4'
    expression: String,
    /// Type of calculation
    operation_type: Option<OperationType>,
}

impl Default for CalculationRequest {
    fn default() -> Self {
        CalculationRequest {
            expression: "".to_string(),
            operation_type: Some(OperationType::Arithmetic),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CalculationResponse {
    result: String,
    steps: Vec<String>,
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
    let client = Gemini::with_model(api_key, "models/gemini-2.5-pro".to_string())
        .expect("unable to create Gemini API client");

    info!("starting gemini 2.5 thinking advanced example");

    // Example 1: Streaming with thinking
    info!("example 1: streaming with thinking");
    let mut stream = client
        .generate_content()
        .with_system_prompt("You are a mathematics expert skilled at solving complex mathematical problems.")
        .with_user_message("Solve this math problem: Find the sum of the first 50 prime numbers. Please explain your solution process in detail.")
        .with_thinking_budget(2048)
        .with_thoughts_included(true)
        .execute_stream()
        .await?;

    info!("starting streaming response");
    let mut thoughts_shown = false;
    while let Some(chunk) = stream.try_next().await? {
        // Check if there's thinking content
        let thoughts = chunk.thoughts();
        if !thoughts.is_empty() && !thoughts_shown {
            info!("showing thinking process");
            for (i, thought) in thoughts.iter().enumerate() {
                info!(thought_number = i + 1, thought = thought, "thought");
            }
            info!("showing answer");
            thoughts_shown = true;
        }

        // Display general text content
        tracing::debug!(text = chunk.text(), "received text chunk");
    }
    info!("streaming response completed");

    // Example 2: Thinking combined with function calls
    info!("example 2: thinking combined with function calls");

    // Define a calculator function
    let calculator =
        FunctionDeclaration::new("calculate", "Perform basic mathematical calculations", None)
            .with_parameters::<CalculationRequest>()
            .with_response::<CalculationResponse>();

    let response = client
        .generate_content()
        .with_system_prompt("You are a mathematics assistant. When calculations are needed, use the provided calculator function.")
        .with_user_message("Calculate the result of (15 + 25) * 3 - 8 and explain the calculation steps.")
        .with_function(calculator)
        .with_thinking_budget(1024)
        .with_thoughts_included(true)
        .execute()
        .await?;

    // Display thinking process
    let thoughts = response.thoughts();
    if !thoughts.is_empty() {
        info!("showing thinking process");
        for (i, thought) in thoughts.iter().enumerate() {
            info!(thought_number = i + 1, thought = thought, "thought");
        }
    }

    // Check for function calls
    let function_calls = response.function_calls();
    if !function_calls.is_empty() {
        info!("function calls detected");
        for (i, call) in function_calls.iter().enumerate() {
            info!(
                call_number = i + 1,
                function_name = call.name,
                args = ?call.args,
                "function call"
            );
        }
    }

    info!(answer = response.text(), "answer");

    // Example 3: Complex reasoning task
    info!("example 3: complex reasoning task");
    let complex_response = client
        .generate_content()
        .with_system_prompt("You are a logical reasoning expert.")
        .with_user_message(
            "There are three people: Alice, Bob, and Carol, who live in red, green, and blue houses respectively.\
            Given:\
            1. The person in the red house owns a cat\
            2. Bob does not live in the green house\
            3. Carol owns a dog\
            4. The green house is to the left of the red house\
            5. Alice does not own a cat\
            Please reason out which color house each person lives in and what pets they own.",
        )
        .with_thinking_config(
            ThinkingConfig::new()
                .with_thinking_budget(3072)
                .with_thoughts_included(true),
        )
        .execute()
        .await?;

    // Display thinking process
    let complex_thoughts = complex_response.thoughts();
    if !complex_thoughts.is_empty() {
        info!("showing reasoning process");
        for (i, thought) in complex_thoughts.iter().enumerate() {
            info!(step_number = i + 1, reasoning = thought, "reasoning step");
        }
    }

    info!(conclusion = complex_response.text(), "conclusion");

    // Display token usage statistics
    if let Some(usage) = &complex_response.usage_metadata {
        info!("token usage statistics");
        if let Some(prompt_tokens) = usage.prompt_token_count {
            info!(prompt_tokens = prompt_tokens, "prompt tokens");
        }
        if let Some(response_tokens) = usage.candidates_token_count {
            info!(response_tokens = response_tokens, "response tokens");
        }
        if let Some(thinking_tokens) = usage.thoughts_token_count {
            info!(thinking_tokens = thinking_tokens, "thinking tokens");
        }
        if let Some(total_tokens) = usage.total_token_count {
            info!(total_tokens = total_tokens, "total tokens");
        }
    }

    Ok(())
}
