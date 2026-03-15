//! mistral.rs multi-model serving example.
//!
//! This example demonstrates how to load and serve multiple models from a single
//! instance, enabling A/B testing, model comparison, and task-specific routing.
//!
//! # Features
//!
//! - Load multiple models simultaneously
//! - Route requests to specific models by name
//! - Set a default model for unspecified requests
//! - Hot-swap between models at runtime
//!
//! # Running
//!
//! ```bash
//! cargo run --example mistralrs_multimodel
//! ```
//!
//! # Configuration
//!
//! You can also load models from a JSON configuration file:
//! ```bash
//! CONFIG_FILE=models.json cargo run --example mistralrs_multimodel
//! ```

use adk_core::{Content, LlmRequest};
use adk_mistralrs::{MistralRsConfig, MistralRsMultiModel, ModelSource, QuantizationLevel};
use futures::StreamExt;
use std::io::{self, Write};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    println!("ADK mistral.rs Multi-Model Example");
    println!("===================================");
    println!();

    // Check for config file
    if let Ok(config_path) = std::env::var("CONFIG_FILE") {
        println!("Loading models from config file: {}", config_path);
        let multi_model = MistralRsMultiModel::from_config(&config_path).await?;
        run_interactive(multi_model).await?;
        return Ok(());
    }

    // Otherwise, load models programmatically
    println!("Loading models programmatically...");
    println!("(Set CONFIG_FILE env var to load from JSON config)");
    println!();

    let multi_model = MistralRsMultiModel::new();

    // Load first model (smaller, faster)
    println!("Loading model 1: Phi-3.5-mini (fast, general purpose)");
    let phi_config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
        .isq(QuantizationLevel::Q4_0) // Quantize for memory efficiency
        .temperature(0.7)
        .max_tokens(512)
        .build();
    multi_model.add_model("phi", phi_config).await?;

    // Note: In a real scenario, you might load additional models:
    // println!("Loading model 2: Llama-3.2-3B (larger, more capable)");
    // let llama_config = MistralRsConfig::builder()
    //     .model_source(ModelSource::huggingface("meta-llama/Llama-3.2-3B-Instruct"))
    //     .isq(QuantizationLevel::Q4_0)
    //     .temperature(0.7)
    //     .max_tokens(1024)
    //     .build();
    // multi_model.add_model("llama", llama_config).await?;

    // Set default model
    multi_model.set_default("phi").await?;

    println!();
    println!("Models loaded successfully!");
    run_interactive(multi_model).await?;

    Ok(())
}

async fn run_interactive(multi_model: MistralRsMultiModel) -> anyhow::Result<()> {
    println!();
    println!("Available models: {:?}", multi_model.model_names().await);
    if let Some(default) = multi_model.default_model().await {
        println!("Default model: {}", default);
    }
    println!();
    println!("Commands:");
    println!("  /models          - List available models");
    println!("  /use <name>      - Switch to a specific model");
    println!("  /default <name>  - Set default model");
    println!("  /compare <msg>   - Compare response from all models");
    println!("  /quit            - Exit");
    println!();
    println!("Or just type a message to chat with the default model.");
    println!();

    let mut current_model: Option<String> = None;

    loop {
        // Show prompt with current model
        let model_indicator = current_model
            .as_ref()
            .map(|m| format!("[{}]", m))
            .unwrap_or_else(|| "[default]".to_string());

        print!("{} > ", model_indicator);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        // Handle commands
        if input.starts_with('/') {
            let parts: Vec<&str> = input.splitn(2, ' ').collect();
            let command = parts[0];
            let arg = parts.get(1).map(|s| s.trim());

            match command {
                "/quit" | "/exit" | "/q" => {
                    println!("Goodbye!");
                    break;
                }
                "/models" => {
                    println!("Available models: {:?}", multi_model.model_names().await);
                    if let Some(default) = multi_model.default_model().await {
                        println!("Default: {}", default);
                    }
                }
                "/use" => {
                    if let Some(name) = arg {
                        if multi_model.has_model(name).await {
                            current_model = Some(name.to_string());
                            println!("Switched to model: {}", name);
                        } else {
                            println!(
                                "Model '{}' not found. Available: {:?}",
                                name,
                                multi_model.model_names().await
                            );
                        }
                    } else {
                        current_model = None;
                        println!("Switched to default model");
                    }
                }
                "/default" => {
                    if let Some(name) = arg {
                        match multi_model.set_default(name).await {
                            Ok(()) => println!("Default model set to: {}", name),
                            Err(e) => println!("Error: {}", e),
                        }
                    } else if let Some(default) = multi_model.default_model().await {
                        println!("Current default: {}", default);
                    } else {
                        println!("No default model set");
                    }
                }
                "/compare" => {
                    if let Some(message) = arg {
                        compare_models(&multi_model, message).await?;
                    } else {
                        println!("Usage: /compare <message>");
                    }
                }
                _ => {
                    println!("Unknown command: {}", command);
                }
            }
            continue;
        }

        // Generate response
        let request = LlmRequest::new("", vec![Content::new("user").with_text(input)]);

        let model_name = current_model.as_deref();
        print!("\nAssistant: ");
        io::stdout().flush()?;

        match multi_model.generate_with_model(model_name, request, true).await {
            Ok(mut stream) => {
                while let Some(response) = stream.next().await {
                    match response {
                        Ok(resp) => {
                            if let Some(content) = resp.content {
                                for part in content.parts {
                                    if let Some(text) = part.text() {
                                        print!("{}", text);
                                        io::stdout().flush()?;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("\nError: {}", e);
                            break;
                        }
                    }
                }
                println!("\n");
            }
            Err(e) => {
                println!("Error: {}", e);
            }
        }
    }

    Ok(())
}

async fn compare_models(multi_model: &MistralRsMultiModel, message: &str) -> anyhow::Result<()> {
    let models = multi_model.model_names().await;

    if models.is_empty() {
        println!("No models loaded");
        return Ok(());
    }

    println!("\nComparing responses from {} models...\n", models.len());

    for model_name in models {
        println!("=== {} ===", model_name);

        let request = LlmRequest::new("", vec![Content::new("user").with_text(message)]);

        match multi_model.generate_with_model(Some(&model_name), request, false).await {
            Ok(mut stream) => {
                while let Some(response) = stream.next().await {
                    if let Ok(resp) = response
                        && let Some(content) = resp.content
                    {
                        for part in content.parts {
                            if let Some(text) = part.text() {
                                println!("{}", text);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("Error: {}", e);
            }
        }
        println!();
    }

    Ok(())
}
