use adk_gemini::{Gemini, GenerationConfig, GenerationResponse, ThinkingConfig};
use display_error_chain::DisplayErrorChain;
use std::env;
use std::process::ExitCode;
use tracing::info;

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
    let client = Gemini::pro(api_key).expect("unable to create Gemini API client");

    info!("starting gemini 2.5 thinking basic example");

    // Example 1: Using default dynamic thinking
    info!("example 1: dynamic thinking (model automatically determines thinking budget)");
    let response1: GenerationResponse = client
        .generate_content()
        .with_system_prompt("You are a helpful mathematics assistant.")
        .with_user_message(
            "Explain Occam's razor principle and provide a simple example from daily life.",
        )
        .with_dynamic_thinking()
        .with_thoughts_included(true)
        .execute()
        .await?;

    // Display thinking process
    let thoughts = response1.thoughts();
    if !thoughts.is_empty() {
        info!("showing thinking summary");
        for (i, thought) in thoughts.iter().enumerate() {
            info!(thought_number = i + 1, thought = thought, "thought");
        }
    }

    info!(answer = response1.text(), "answer");

    // Display token usage
    if let Some(usage) = &response1.usage_metadata {
        info!("token usage");
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

    // Example 2: Set specific thinking budget
    info!("example 2: set thinking budget (1024 tokens)");
    let response2: GenerationResponse = client
        .generate_content()
        .with_system_prompt("You are a helpful programming assistant.")
        .with_user_message("List 3 main advantages of using the Rust programming language")
        .with_thinking_budget(1024)
        .with_thoughts_included(true)
        .execute()
        .await?;

    // Display thinking process
    let thoughts2 = response2.thoughts();
    if !thoughts2.is_empty() {
        info!("showing thinking summary");
        for (i, thought) in thoughts2.iter().enumerate() {
            info!(thought_number = i + 1, thought = thought, "thought");
        }
    }

    info!(answer = response2.text(), "answer");

    // Example 3: Disable thinking feature
    info!("example 3: disable thinking feature");
    let response3: GenerationResponse = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant.")
        .with_user_message("What is artificial intelligence?")
        .execute()
        .await?;

    info!(answer = response3.text(), "answer");

    // Example 4: Use GenerationConfig to set thinking
    info!("example 4: use GenerationConfig to set thinking");
    let thinking_config =
        ThinkingConfig::new().with_thinking_budget(2048).with_thoughts_included(true);

    let generation_config = GenerationConfig {
        temperature: Some(0.7),
        max_output_tokens: Some(500),
        thinking_config: Some(thinking_config),
        ..Default::default()
    };

    let response4: GenerationResponse = client
        .generate_content()
        .with_system_prompt("You are a creative writing assistant.")
        .with_user_message(
            "Write the opening of a short story about a robot learning to feel emotions.",
        )
        .with_generation_config(generation_config)
        .execute()
        .await?;

    // Display thinking process
    let thoughts4 = response4.thoughts();
    if !thoughts4.is_empty() {
        info!("showing thinking summary");
        for (i, thought) in thoughts4.iter().enumerate() {
            info!(thought_number = i + 1, thought = thought, "thought");
        }
    }

    info!(answer = response4.text(), "answer");

    Ok(())
}
