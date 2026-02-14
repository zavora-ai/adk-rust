use std::process::ExitCode;

use adk_gemini::{ClientError, Gemini, Model, TaskType};
use display_error_chain::DisplayErrorChain;
use tracing::{error, info};

async fn do_main(api_key: &str) -> Result<(), ClientError> {
    let client = Gemini::with_model(api_key, Model::GeminiEmbedding001)
        .expect("unable to create Gemini API client");

    info!("sending embedding request to gemini api");

    // Simple text embedding
    let response = client
        .embed_content()
        .with_text("Hello")
        .with_task_type(TaskType::RetrievalDocument)
        .execute()
        .await?;

    info!(embedding_values = ?response.embedding.values, "embedding response received");

    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    // Initialize tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    let api_key = std::env::var("GEMINI_API_KEY").expect("no gemini api key provided");

    match do_main(&api_key).await {
        Err(err) => {
            let formatted = DisplayErrorChain::new(err).to_string();
            error!(error = formatted, "request failed");
            ExitCode::FAILURE
        }
        _ => {
            info!("embedding request completed successfully");
            ExitCode::SUCCESS
        }
    }
}
