use display_error_chain::DisplayErrorChain;
use gemini_rust::{Gemini, Model};
use std::process::ExitCode;
use std::time::Duration;
use tracing::{error, info, warn};

// Include the story text at compile time
const GRIEF_EATER_STORY: &str = include_str!("../test_data/grief_eater.txt");

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
    let api_key =
        std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable must be set");

    let client = Gemini::with_model(api_key, Model::Gemini25FlashLite)?;

    info!("creating cached content with full story text");

    // Create cached content with the full story for analysis
    let cache = client
        .create_cache()
        .with_display_name("Grief Eater Story Analysis Cache")?
        .with_system_instruction("You are a literary analyst specialized in horror and supernatural fiction. Analyze stories for themes, character development, narrative techniques, and psychological elements.")
        .with_user_message("Please read and analyze this story:")
        .with_user_message(GRIEF_EATER_STORY)
        .with_ttl(Duration::from_secs(3600)) // Cache for 1 hour
        .execute()
        .await?;

    info!(cache_name = cache.name(), "cache created successfully");

    // Demonstrate cache retrieval to show token count
    info!("retrieving cache information");
    let cached_content = cache.get().await?;
    info!(
        cache_name = cached_content.name,
        display_name = cached_content.display_name.as_ref().unwrap_or(&"N/A".to_string()),
        model = %cached_content.model,
        create_time = %cached_content.create_time,
        update_time = %cached_content.update_time,
        total_tokens = cached_content.usage_metadata.total_token_count,
        "cache details retrieved"
    );
    if let Some(expire_time) = cached_content.expiration.expire_time {
        info!(expire_time = %expire_time, "cache expiration time");
    }

    // Ask several analytical questions using the cached content
    info!("asking question 1: main theme");
    let response1 = client
        .generate_content()
        .with_cached_content(&cache)
        .with_user_message("What is the central theme of this story? How does the protagonist's relationship with grief evolve?")
        .execute()
        .await?;

    info!(response = response1.text(), "question 1 response received");

    info!("asking question 2: narrative technique");
    let response2 = client
        .generate_content()
        .with_cached_content(&cache)
        .with_user_message("Analyze the narrative technique. How does the author use the protagonist's childhood nightmare as a literary device?")
        .execute()
        .await?;

    info!(response = response2.text(), "question 2 response received");

    info!("asking question 3: character arc");
    let response3 = client
        .generate_content()
        .with_cached_content(&cache)
        .with_user_message("Describe the protagonist's character arc. What does he learn about grief by the end of the story?")
        .execute()
        .await?;

    info!(response = response3.text(), "question 3 response received");

    info!("asking question 4: symbolism");
    let response4 = client
        .generate_content()
        .with_cached_content(&cache)
        .with_user_message("What symbolic meaning does the grandfather's pocket knife hold in the story's resolution?")
        .execute()
        .await?;

    info!(response = response4.text(), "question 4 response received");

    // Clean up by deleting the cache
    info!("cleaning up cache");
    match cache.delete().await {
        Ok(_) => info!("cache deleted successfully"),
        Err((cache, error)) => {
            error!(error = %error, "failed to delete cache");
            warn!(
                cache_name = cache.name(),
                "cache handle returned for potential retry"
            );
        }
    }

    Ok(())
}
