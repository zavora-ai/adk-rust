//! Roadmap example: standardized retry config across providers.
//!
//! Run with Gemini only (default):
//!   cargo run --example roadmap_retry_matrix
//!
//! Run with all remote providers:
//!   cargo run --example roadmap_retry_matrix --features openai,anthropic,deepseek,groq
//!
//! Optional env:
//!   ROADMAP_RUN_PROVIDER=gemini|openai|anthropic|deepseek|groq
//!   ROADMAP_PROMPT="..."

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::{GeminiModel, RetryConfig};
use anyhow::Result;
use futures::StreamExt;
use std::{env, time::Duration};

#[cfg(feature = "anthropic")]
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
#[cfg(feature = "deepseek")]
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
#[cfg(feature = "groq")]
use adk_model::groq::{GroqClient, GroqConfig};
#[cfg(feature = "openai")]
use adk_model::openai::{OpenAIClient, OpenAIConfig};

fn google_api_key() -> Option<String> {
    env::var("GOOGLE_API_KEY").ok().or_else(|| env::var("GEMINI_API_KEY").ok())
}

fn print_retry(label: &str, retry: &RetryConfig) {
    println!(
        "{} retry -> enabled={}, max_retries={}, initial_delay_ms={}, max_delay_ms={}, backoff_multiplier={}",
        label,
        retry.enabled,
        retry.max_retries,
        retry.initial_delay.as_millis(),
        retry.max_delay.as_millis(),
        retry.backoff_multiplier
    );
}

async fn call_once<M: Llm + ?Sized>(label: &str, model: &M, prompt: &str) -> Result<()> {
    println!("Running {} request...", label);
    let request = LlmRequest::new(model.name(), vec![Content::new("user").with_text(prompt)]);
    let mut stream = model.generate_content(request, false).await?;
    let mut output = String::new();

    while let Some(chunk) = stream.next().await {
        let response = chunk?;
        if let Some(content) = response.content {
            for part in content.parts {
                if let Part::Text { text } = part {
                    output.push_str(&text);
                }
            }
        }
    }

    println!("\n{} response:\n{}\n", label, output);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    let retry = RetryConfig::default()
        .with_max_retries(4)
        .with_initial_delay(Duration::from_millis(250))
        .with_max_delay(Duration::from_secs(4))
        .with_backoff_multiplier(2.0);
    let run_provider = env::var("ROADMAP_RUN_PROVIDER").ok().map(|s| s.to_lowercase());
    let prompt = env::var("ROADMAP_PROMPT")
        .unwrap_or_else(|_| "Give one sentence about retry policy design.".to_string());

    println!("Standardized retry policy configured once and applied across providers.");
    print_retry("Shared", &retry);

    if let Some(api_key) = google_api_key() {
        let gemini =
            GeminiModel::new(api_key, "gemini-2.5-flash")?.with_retry_config(retry.clone());
        print_retry("Gemini", gemini.retry_config());
        if run_provider.as_deref() == Some("gemini") {
            call_once("Gemini", &gemini, &prompt).await?;
        }
    } else {
        println!("Gemini skipped: set GOOGLE_API_KEY (or GEMINI_API_KEY).");
    }

    #[cfg(feature = "openai")]
    {
        if let Ok(api_key) = env::var("OPENAI_API_KEY") {
            let openai = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?
                .with_retry_config(retry.clone());
            print_retry("OpenAI", openai.retry_config());
            if run_provider.as_deref() == Some("openai") {
                call_once("OpenAI", &openai, &prompt).await?;
            }
        } else {
            println!("OpenAI skipped: set OPENAI_API_KEY.");
        }
    }

    #[cfg(feature = "anthropic")]
    {
        if let Ok(api_key) = env::var("ANTHROPIC_API_KEY") {
            let anthropic =
                AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-4-5-20250929"))?
                    .with_retry_config(retry.clone());
            print_retry("Anthropic", anthropic.retry_config());
            if run_provider.as_deref() == Some("anthropic") {
                call_once("Anthropic", &anthropic, &prompt).await?;
            }
        } else {
            println!("Anthropic skipped: set ANTHROPIC_API_KEY.");
        }
    }

    #[cfg(feature = "deepseek")]
    {
        if let Ok(api_key) = env::var("DEEPSEEK_API_KEY") {
            let deepseek = DeepSeekClient::new(DeepSeekConfig::chat(api_key))?
                .with_retry_config(retry.clone());
            print_retry("DeepSeek", deepseek.retry_config());
            if run_provider.as_deref() == Some("deepseek") {
                call_once("DeepSeek", &deepseek, &prompt).await?;
            }
        } else {
            println!("DeepSeek skipped: set DEEPSEEK_API_KEY.");
        }
    }

    #[cfg(feature = "groq")]
    {
        if let Ok(api_key) = env::var("GROQ_API_KEY") {
            let groq = GroqClient::new(GroqConfig::llama8b(api_key))?.with_retry_config(retry);
            print_retry("Groq", groq.retry_config());
            if run_provider.as_deref() == Some("groq") {
                call_once("Groq", &groq, &prompt).await?;
            }
        } else {
            println!("Groq skipped: set GROQ_API_KEY.");
        }
    }

    if run_provider.is_some() {
        println!("ROADMAP_RUN_PROVIDER set: only that provider attempted a live call.");
    } else {
        println!("Set ROADMAP_RUN_PROVIDER to execute a live call for one provider.");
    }

    Ok(())
}
