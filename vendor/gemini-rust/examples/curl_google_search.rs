use display_error_chain::DisplayErrorChain;
use gemini_rust::{Content, Gemini, Part, Tool};
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

    info!("starting curl equivalent with google search tool example");

    // This is equivalent to the curl example:
    // curl "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent?key=$GEMINI_API_KEY" \
    //   -H "Content-Type: application/json" \
    //   -d '{
    //       "contents": [
    //           {
    //               "parts": [
    //                   {"text": "What is the current Google stock price?"}
    //               ]
    //           }
    //       ],
    //       "tools": [
    //           {
    //               "google_search": {}
    //           }
    //       ]
    //   }'

    // Create client
    let client = Gemini::new(api_key).expect("unable to create Gemini API client");

    // Create a content part that matches the JSON in the curl example
    let text_part = Part::Text {
        text: "What is the current Google stock price?".to_string(),
        thought: None,
        thought_signature: None,
    };

    let content = Content {
        parts: vec![text_part].into(),
        role: None,
    };

    // Create a Google Search tool
    let google_search_tool = Tool::google_search();

    // Add the content and tool directly to the request
    // This exactly mirrors the JSON structure in the curl example
    let mut content_builder = client.generate_content();
    content_builder.contents.push(content);
    content_builder = content_builder.with_tool(google_search_tool);

    let response = content_builder.execute().await?;

    info!(
        response = response.text(),
        "google search response received"
    );

    Ok(())
}
