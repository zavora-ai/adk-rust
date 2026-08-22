//! Per-request beta headers and bearer-token authentication.
//!
//! Set `ANTHROPIC_API_KEY`, or set `ANTHROPIC_AUTH_TOKEN` when the endpoint
//! accepts bearer authentication, then run:
//! `cargo run -p adk-anthropic --example request_customization`.

use adk_anthropic::{Anthropic, KnownModel, MessageCreateParams};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = match std::env::var("ANTHROPIC_AUTH_TOKEN") {
        Ok(token) => Anthropic::new_with_auth_token(token)?,
        Err(_) => Anthropic::new(None)?,
    };
    let params =
        MessageCreateParams::simple("Reply with one short greeting.", KnownModel::ClaudeSonnet46);
    let response =
        client.send_with_betas(params, &["fine-grained-tool-streaming-2025-05-14"]).await?;

    for block in response.content {
        if let Some(text) = block.as_text() {
            println!("{}", text.text);
        }
    }
    Ok(())
}
