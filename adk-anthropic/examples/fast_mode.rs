//! Fast mode (beta) with the Anthropic Messages API.
//!
//! Fast mode delivers up to 2.5× higher output tokens per second on
//! Claude Opus 4.6 at 6× standard pricing. The beta header
//! `fast-mode-2026-02-01` is injected automatically when `speed` is set.
//!
//! Requires waitlist access — if you don't have it, the API returns an error
//! and the example falls back to standard speed to show the comparison.
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run -p adk-anthropic --example fast_mode`

use adk_anthropic::{Anthropic, KnownModel, MessageCreateParams, SpeedMode};
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    let prompt = "Write a detailed explanation of how TCP/IP works, covering the four layers, \
                  the three-way handshake, flow control, and congestion avoidance. \
                  Be thorough but concise.";

    // ── 1. Standard speed (baseline) ─────────────────────────────
    println!("=== Standard Speed (baseline) ===\n");

    let params = MessageCreateParams::simple(prompt, KnownModel::ClaudeOpus46);

    let start = Instant::now();
    let r_std = client.send(params).await?;
    let std_elapsed = start.elapsed();

    let std_tokens = r_std.usage.output_tokens;
    let std_tps = std_tokens as f64 / std_elapsed.as_secs_f64();

    println!("Output tokens: {std_tokens}");
    println!("Time:          {:.2}s", std_elapsed.as_secs_f64());
    println!("Throughput:    {std_tps:.1} tokens/sec");
    print_preview(&r_std);

    // ── 2. Fast mode ─────────────────────────────────────────────
    println!("\n=== Fast Mode (beta) ===\n");

    let mut params = MessageCreateParams::simple(prompt, KnownModel::ClaudeOpus46);
    params.speed = Some(SpeedMode::Fast);

    // Show the serialized speed field
    println!("Request includes: speed={}\n", serde_json::to_string(&params.speed)?);

    let start = Instant::now();
    match client.send(params).await {
        Ok(r_fast) => {
            let fast_elapsed = start.elapsed();
            let fast_tokens = r_fast.usage.output_tokens;
            let fast_tps = fast_tokens as f64 / fast_elapsed.as_secs_f64();

            println!("Output tokens: {fast_tokens}");
            println!("Time:          {:.2}s", fast_elapsed.as_secs_f64());
            println!("Throughput:    {fast_tps:.1} tokens/sec");
            print_preview(&r_fast);

            // ── Comparison ───────────────────────────────────────
            println!("\n=== Comparison ===\n");
            println!("Standard: {std_tps:.1} tok/s");
            println!("Fast:     {fast_tps:.1} tok/s");
            if fast_tps > std_tps {
                println!("Speedup:  {:.2}×", fast_tps / std_tps);
            }
        }
        Err(e) => {
            println!("Fast mode not available: {e}");
            println!("\nFast mode is in beta (research preview).");
            println!("Join the waitlist at https://console.anthropic.com to request access.");
            println!("\nThe `speed: \"fast\"` field and `fast-mode-2026-02-01` beta header");
            println!("were sent correctly — the API rejected because your account");
            println!("doesn't have fast mode enabled yet.");
        }
    }

    Ok(())
}

fn print_preview(msg: &adk_anthropic::Message) {
    for block in &msg.content {
        if let Some(text) = block.as_text() {
            let preview: String = text.text.chars().take(150).collect();
            println!("\n\"{preview}...\"");
        }
    }
}
