//! Custom base URL with the Anthropic client.
//!
//! Several providers expose Anthropic-compatible APIs at custom endpoints:
//!
//! | Provider        | Base URL                                    |
//! |-----------------|---------------------------------------------|
//! | Anthropic       | https://api.anthropic.com (default)          |
//! | Ollama          | http://localhost:11434                       |
//! | Vercel Gateway  | https://ai-gateway.vercel.sh                |
//! | MiniMax (intl)  | https://api.minimax.io/anthropic            |
//! | MiniMax (China) | https://api.minimaxi.com/anthropic          |
//! | Enterprise      | https://your-proxy.internal.com/anthropic   |
//!
//! Two ways to set the base URL:
//! 1. Builder: `Anthropic::new(key)?.with_base_url(url)`
//! 2. Env var: `ANTHROPIC_BASE_URL=https://...`
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run -p adk-anthropic --example custom_base_url`

use adk_anthropic::{Anthropic, KnownModel, MessageCreateParams};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    // ── 1. Default (api.anthropic.com) ───────────────────────────
    println!("=== Default Base URL ===\n");

    let client = Anthropic::new(None)?;
    let r = client
        .send(MessageCreateParams::simple(
            "Say hello in one word.",
            KnownModel::ClaudeSonnet46,
        ))
        .await?;

    for b in &r.content {
        if let Some(t) = b.as_text() {
            println!("Response: {}", t.text);
        }
    }

    // ── 2. Custom base URL (builder) ─────────────────────────────
    println!("\n=== Custom Base URL (builder) ===\n");

    // Example: point to a local proxy or alternative provider.
    // This will fail unless you have a compatible server running.
    let custom_url = std::env::var("CUSTOM_ANTHROPIC_URL")
        .unwrap_or_else(|_| "https://api.anthropic.com".to_string());

    println!("Using base URL: {custom_url}");

    let client = Anthropic::new(None)?.with_base_url(custom_url);

    let r = client
        .send(MessageCreateParams::simple(
            "What is 1+1? Answer with just the number.",
            KnownModel::ClaudeSonnet46,
        ))
        .await?;

    for b in &r.content {
        if let Some(t) = b.as_text() {
            println!("Response: {}", t.text);
        }
    }

    // ── 3. Ollama example (if running locally) ───────────────────
    println!("\n=== Ollama (localhost:11434) ===\n");

    let ollama_client = Anthropic::new(Some("ollama".to_string()))?
        .with_base_url("http://localhost:11434".to_string());

    match ollama_client
        .send(MessageCreateParams::simple(
            "Say hi.",
            adk_anthropic::Model::Custom("llama3.2".to_string()),
        ))
        .await
    {
        Ok(r) => {
            for b in &r.content {
                if let Some(t) = b.as_text() {
                    println!("Ollama response: {}", t.text);
                }
            }
        }
        Err(e) => {
            println!("Ollama not available (expected if not running): {e}");
            println!("To test: ollama serve & ollama pull llama3.2");
        }
    }

    Ok(())
}
