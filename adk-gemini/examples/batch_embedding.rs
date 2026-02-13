use adk_gemini::{BatchContentEmbeddingResponse, Gemini, Model, TaskType};
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

    info!("sending batch embedding request to gemini api");

    // Simple text embedding
    let response: BatchContentEmbeddingResponse = client
        .embed_content()
        .with_chunks(vec!["Hello", "World", "Test embedding 3"])
        .with_task_type(TaskType::RetrievalDocument)
        .execute_batch()
        .await?;

    info!(embeddings_count = response.embeddings.len(), "batch embedding completed");

    for (i, e) in response.embeddings.iter().enumerate() {
        info!(
            index = i,
            embedding_length = e.values.len(),
            first_values = ?&e.values[..5.min(e.values.len())],
            "embedding result"
        );
    }

    Ok(())
}
