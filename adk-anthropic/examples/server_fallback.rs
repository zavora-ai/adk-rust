//! Anthropic server-side safety-refusal fallback.
//!
//! This beta does not retry rate limits, overloads, or server errors. Set
//! `ANTHROPIC_API_KEY`, then run:
//! `cargo run -p adk-anthropic --example server_fallback`.

use adk_anthropic::{
    Anthropic, MessageCreateParams, Model, ServerFallbackContentBlock, ServerFallbackRequest,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Anthropic::new(None)?;
    let params = MessageCreateParams::simple(
        "Explain in one sentence why fallback is not a general retry policy.",
        Model::Custom("claude-fable-5".to_string()),
    );
    let response =
        client.send_with_server_fallbacks(ServerFallbackRequest::default_routing(params)?).await?;

    println!("served by: {}", response.model);
    println!("fallback ran: {}", response.served_by_fallback());
    for block in response.content {
        if let ServerFallbackContentBlock::Standard(block) = block
            && let Some(text) = block.as_text()
        {
            println!("{}", text.text);
        }
    }
    Ok(())
}
