use adk_gemini::{Gemini, GenerationResponse};
use display_error_chain::DisplayErrorChain;
use futures_util::TryStreamExt;
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
    let client = Gemini::new(api_key).expect("unable to create Gemini API client");

    // Simple streaming generation
    info!("starting streaming generation example");

    let mut stream = client
        .generate_content()
        .with_thinking_budget(0)
        .with_system_prompt("You are a helpful, creative assistant.")
        .with_user_message("Write a short story about a robot who learns to feel emotions.")
        .execute_stream()
        .await?;

    // pin!(stream);

    info!("streaming response chunks");
    let mut full_response = String::new();
    while let Some(chunk) = stream.try_next().await? {
        let chunk_text = chunk.text();
        full_response.push_str(&chunk_text);
        tracing::debug!(chunk = chunk_text, "received chunk");
    }
    info!(response = full_response, "streaming generation completed");

    // Multi-turn conversation
    info!("starting multi-turn conversation example");

    // First turn
    info!(
        question = "I'm planning a trip to Japan. What are the best times to visit?",
        "sending first turn"
    );
    let response1: GenerationResponse = client
        .generate_content()
        .with_system_prompt("You are a helpful travel assistant.")
        .with_user_message("I'm planning a trip to Japan. What are the best times to visit?")
        .execute()
        .await?;

    info!(
        question = "I'm planning a trip to Japan. What are the best times to visit?",
        response = response1.text(),
        "first turn completed"
    );

    // Second turn (continuing the conversation)
    info!(
        question = "What about cherry blossom season? When exactly does that happen?",
        "sending second turn"
    );
    let response2: GenerationResponse = client
        .generate_content()
        .with_system_prompt("You are a helpful travel assistant.")
        .with_user_message("I'm planning a trip to Japan. What are the best times to visit?")
        .with_model_message(response1.text())
        .with_user_message("What about cherry blossom season? When exactly does that happen?")
        .execute()
        .await?;

    info!(
        question = "What about cherry blossom season? When exactly does that happen?",
        response = response2.text(),
        "second turn completed"
    );

    // Third turn (continuing the conversation)
    info!(question = "What are some must-visit places in Tokyo?", "sending third turn");
    let response3: GenerationResponse = client
        .generate_content()
        .with_system_prompt("You are a helpful travel assistant.")
        .with_user_message("I'm planning a trip to Japan. What are the best times to visit?")
        .with_model_message(response1.text())
        .with_user_message("What about cherry blossom season? When exactly does that happen?")
        .with_model_message(response2.text())
        .with_user_message("What are some must-visit places in Tokyo?")
        .execute()
        .await?;

    info!(
        question = "What are some must-visit places in Tokyo?",
        response = response3.text(),
        "third turn completed"
    );

    Ok(())
}
