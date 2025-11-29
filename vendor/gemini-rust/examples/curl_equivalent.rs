use display_error_chain::DisplayErrorChain;
use gemini_rust::{Content, Gemini, Part};
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

    // This is equivalent to the curl example:
    // curl "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent?key=$YOUR_API_KEY" \
    //   -H 'Content-Type: application/json' \
    //   -X POST \
    //   -d '{
    //     "contents": [
    //       {
    //         "parts": [
    //           {
    //             "text": "Explain how AI works in a few words"
    //           }
    //         ]
    //       }
    //     ]
    //   }'

    // Create client - now using gemini-2.0-flash by default
    let client = Gemini::new(api_key).expect("unable to create Gemini API client");

    // Method 1: Using the high-level API (simplest approach)
    info!("method 1: using high-level api");

    let response = client
        .generate_content()
        .with_user_message("Explain how AI works in a few words")
        .execute()
        .await?;

    info!(response = response.text(), "response received");

    // Method 2: Using Content directly to match the curl example exactly
    info!("method 2: matching curl example structure exactly");

    // Create a content part that matches the JSON in the curl example
    let text_part = Part::Text {
        text: "Explain how AI works in a few words".to_string(),
        thought: None,
        thought_signature: None,
    };

    let content = Content {
        parts: Some(vec![text_part]),
        role: None,
    };

    // Add the content directly to the request
    // This exactly mirrors the JSON structure in the curl example
    let mut content_builder = client.generate_content();
    content_builder.contents.push(content);
    let response = content_builder.execute().await?;

    info!(response = response.text(), "response received");

    Ok(())
}
