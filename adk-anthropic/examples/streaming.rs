//! SSE streaming with the Anthropic Messages API.
//!
//! Demonstrates: streaming responses, handling deltas, final usage stats.
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run`

use adk_anthropic::{
    Anthropic, ContentBlockDelta, KnownModel, MessageCreateParams, MessageStreamEvent,
};
use futures::StreamExt;
use std::pin::pin;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    let params = MessageCreateParams::simple_streaming(
        "Write a short poem about Rust programming.",
        KnownModel::ClaudeSonnet46,
    );

    let stream = client.stream(&params).await?;
    let mut stream = pin!(stream);

    println!();
    while let Some(event) = stream.next().await {
        match event? {
            MessageStreamEvent::ContentBlockDelta(delta) => {
                if let ContentBlockDelta::TextDelta(text) = &delta.delta {
                    print!("{}", text.text);
                }
            }
            MessageStreamEvent::MessageDelta(delta) => {
                if let Some(reason) = &delta.delta.stop_reason {
                    println!("\n\n[stop: {reason}]");
                }
                println!("[output tokens: {}]", delta.usage.output_tokens);
            }
            MessageStreamEvent::Ping => {}
            _ => {}
        }
    }

    Ok(())
}
