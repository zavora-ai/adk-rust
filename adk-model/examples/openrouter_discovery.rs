#![allow(clippy::result_large_err)]

#[path = "openrouter/common.rs"]
mod common;
#[path = "openrouter/discovery_support.rs"]
mod discovery_support;

use adk_model::openrouter::OpenRouterApiMode;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let (client, config) = common::build_client(OpenRouterApiMode::ChatCompletions)?;

    common::print_section("models");
    let models = client.list_models().await?;
    println!("discovered models: {}", models.len());

    if let Some(model) = models.iter().find(|model| model.id == config.model) {
        println!("configured model: {}", model.id);
        println!(
            "context_length={:?} supported_parameters={} description={}",
            model.context_length,
            model.supported_parameters.len(),
            model.description.as_deref().unwrap_or("<none>")
        );
    } else {
        println!("configured model '{}' was not found; first few models:", config.model);
        for model in models.iter().take(5) {
            println!("- {}", model.id);
        }
    }

    if let Some((author, slug)) = discovery_support::model_author_slug(&config.model) {
        common::print_section("model endpoints");
        match client.get_model_endpoints(&author, &slug).await {
            Ok(endpoints) => {
                println!("endpoint set: {}", endpoints.id);
                println!("provider endpoints: {}", endpoints.endpoints.len());
                for endpoint in endpoints.endpoints.iter().take(5) {
                    println!(
                        "- provider={} quantization={:?} status={:?} implicit_caching={:?}",
                        endpoint.provider_name.as_deref().unwrap_or("<unknown>"),
                        endpoint.quantization,
                        endpoint.status,
                        endpoint.supports_implicit_caching
                    );
                }
            }
            Err(err) => {
                println!("endpoint discovery failed for {}: {err}", config.model);
            }
        }
    }

    common::print_section("providers");
    let providers = client.list_providers().await?;
    println!("providers: {}", providers.len());
    for provider in providers.iter().take(10) {
        println!("- {} ({})", provider.name, provider.slug);
    }

    common::print_section("credits");
    match client.get_credits().await {
        Ok(credits) => {
            println!(
                "credits: total_credits={} total_usage={}",
                credits.total_credits, credits.total_usage
            );
        }
        Err(err) => {
            println!("credits lookup is unavailable for this key: {err}");
        }
    }

    Ok(())
}
