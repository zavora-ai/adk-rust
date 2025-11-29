use display_error_chain::DisplayErrorChain;
use gemini_rust::{Gemini, GenerationConfig, ThinkingConfig};
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

    // This is equivalent to the following curl example:
    // curl "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent" \
    //   -H "x-goog-api-key: $GEMINI_API_KEY" \
    //   -H 'Content-Type: application/json' \
    //   -X POST \
    //   -d '{
    //     "contents": [
    //       {
    //         "parts": [
    //           {
    //             "text": "Provide a list of the top 3 famous physicists and their major contributions"
    //           }
    //         ]
    //       }
    //     ],
    //     "generationConfig": {
    //       "thinkingConfig": {
    //         "thinkingBudget": 1024,
    //         "includeThoughts": true
    //       }
    //     }
    //   }'

    // Create client
    let client = Gemini::with_model(api_key, "models/gemini-2.5-pro".to_string())
        .expect("unable to create Gemini API client");

    info!("starting thinking curl equivalent example");

    // Method 1: Using high-level API (simplest approach)
    info!("method 1: using high-level API");

    let response1 = client
        .generate_content()
        .with_user_message(
            "Provide a list of the top 3 famous physicists and their major contributions",
        )
        .with_thinking_budget(1024)
        .with_thoughts_included(true)
        .execute()
        .await?;

    // Display thinking process
    let thoughts1 = response1.thoughts();
    if !thoughts1.is_empty() {
        info!("showing thinking summary");
        for (i, thought) in thoughts1.iter().enumerate() {
            info!(thought_number = i + 1, thought = thought, "thought");
        }
    }

    info!(answer = response1.text(), "answer");

    // Method 2: Using GenerationConfig to fully match curl example structure
    info!("method 2: fully matching curl example structure");

    let thinking_config = ThinkingConfig {
        thinking_budget: Some(1024),
        include_thoughts: Some(true),
    };

    let generation_config = GenerationConfig {
        thinking_config: Some(thinking_config),
        ..Default::default()
    };

    let response2 = client
        .generate_content()
        .with_user_message(
            "Provide a list of the top 3 famous physicists and their major contributions",
        )
        .with_generation_config(generation_config)
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

    // Show token usage
    if let Some(usage) = &response2.usage_metadata {
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

    // Method 3: Demonstrate different thinking budget settings
    info!("method 3: different thinking budget comparison");

    // Thinking disabled
    info!("testing thinking disabled");
    let response_no_thinking = client
        .generate_content()
        .with_user_message("Explain the basic principles of quantum mechanics")
        .execute()
        .await?;
    info!(answer = response_no_thinking.text(), "answer");

    // Dynamic thinking
    info!("testing dynamic thinking");
    let response_dynamic = client
        .generate_content()
        .with_user_message("Explain the basic principles of quantum mechanics")
        .with_dynamic_thinking()
        .with_thoughts_included(true)
        .execute()
        .await?;

    let thoughts_dynamic = response_dynamic.thoughts();
    if !thoughts_dynamic.is_empty() {
        info!("showing thinking summary");
        for (i, thought) in thoughts_dynamic.iter().enumerate() {
            info!(thought_number = i + 1, thought = thought, "thought");
        }
    }
    info!(answer = response_dynamic.text(), "answer");

    // High thinking budget
    info!("testing high thinking budget (4096 tokens)");
    let response_high_budget = client
        .generate_content()
        .with_user_message("Explain the basic principles of quantum mechanics")
        .with_thinking_budget(4096)
        .with_thoughts_included(true)
        .execute()
        .await?;

    let thoughts_high = response_high_budget.thoughts();
    if !thoughts_high.is_empty() {
        info!("showing thinking summary");
        for (i, thought) in thoughts_high.iter().enumerate() {
            info!(thought_number = i + 1, thought = thought, "thought");
        }
    }
    info!(answer = response_high_budget.text(), "answer");

    Ok(())
}
