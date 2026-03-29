//! Server-side compaction with the Anthropic Messages API.
//!
//! Demonstrates how compaction events appear in SSE streaming when the server
//! summarises long conversations. Also shows the `CompactionMetadata` type
//! for non-streaming responses.
//!
//! Note: Compaction fires only on very long conversations that approach the
//! context window limit. This example shows the type system and streaming
//! event handling — you'll see compaction events in production with long
//! agentic conversations.
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run -p adk-anthropic --example compaction`

use adk_anthropic::{
    Anthropic, CompactionMetadata, ContentBlockDelta, KnownModel, MessageCreateParams,
    MessageParam, MessageRole, MessageStreamEvent,
};
use futures::StreamExt;
use std::pin::pin;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    // ── 1. Show CompactionMetadata type ──────────────────────────
    println!("=== CompactionMetadata Type ===\n");

    let example = CompactionMetadata {
        compacted_token_count: 45000,
        summary_token_count: 2500,
        context_window_remaining: 152500,
    };
    println!("Example compaction event:");
    println!("{}\n", serde_json::to_string_pretty(&example)?);

    // ── 2. Streaming with compaction event handling ───────────────
    println!("=== Streaming (with compaction event handler) ===\n");

    // Build a multi-turn conversation to demonstrate the streaming pattern.
    // In production, compaction fires on very long conversations.
    let messages = vec![MessageParam::new_with_string(
        "You are helping me refactor a large codebase. We've been working for a while. \
             Summarize what a good refactoring strategy looks like in 2-3 sentences."
            .to_string(),
        MessageRole::User,
    )];

    let params =
        MessageCreateParams::new_streaming(1024, messages, KnownModel::ClaudeSonnet46.into());

    let stream = client.stream(&params).await?;
    let mut stream = pin!(stream);

    let mut saw_compaction = false;

    while let Some(event) = stream.next().await {
        match event? {
            MessageStreamEvent::ContentBlockDelta(delta) => {
                if let ContentBlockDelta::TextDelta(text) = &delta.delta {
                    print!("{}", text.text);
                }
            }
            MessageStreamEvent::CompactionEvent(meta) => {
                saw_compaction = true;
                println!("\n\n[COMPACTION EVENT]");
                println!("  compacted_token_count:    {}", meta.compacted_token_count);
                println!("  summary_token_count:      {}", meta.summary_token_count);
                println!("  context_window_remaining: {}", meta.context_window_remaining);
            }
            MessageStreamEvent::MessageDelta(delta) => {
                if let Some(reason) = &delta.delta.stop_reason {
                    println!("\n\n[stop: {reason}]");
                }
            }
            _ => {}
        }
    }

    if !saw_compaction {
        println!("\n\n(No compaction event — expected for short conversations.");
        println!(" Compaction fires when context approaches the window limit.)");
    }

    Ok(())
}
