use adk_gemini::{ContentEmbeddingResponse, Gemini, Model, TaskType};
use display_error_chain::DisplayErrorChain;
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
    let api_key = std::env::var("GEMINI_API_KEY")?;

    // Create client with the default model (gemini-2.0-flash)
    let client = Gemini::with_model(api_key, Model::GeminiEmbedding001)
        .expect("unable to create Gemini API client");

    info!("sending embedding request to gemini api");

    // Simple text embedding
    let response: ContentEmbeddingResponse = client
        .embed_content()
        .with_text("Hello")
        .with_task_type(TaskType::RetrievalDocument)
        .execute()
        .await?;

    info!(
        embedding_length = response.embedding.values.len(),
        first_values = ?&response.embedding.values[..5.min(response.embedding.values.len())],
        "embedding completed"
    );

    Ok(())
}
