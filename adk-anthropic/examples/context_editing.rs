//! Context editing with the Anthropic Messages API (beta).
//!
//! Demonstrates the `context_management` parameter for automatically clearing
//! old tool results and thinking blocks as conversations grow.
//!
//! Requires beta header `context-management-2025-06-27` (injected automatically
//! when `context_management` is set).
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run -p adk-anthropic --example context_editing`

use adk_anthropic::{
    Anthropic, ContextEdit, ContextManagement, KnownModel, MessageCreateParams, MessageParam,
    MessageRole, ThinkingKeep, TokenThreshold,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    // ── 1. Tool result clearing (simplest form) ──────────────────
    println!("=== Tool Result Clearing (defaults) ===\n");

    let mut params = MessageCreateParams::simple(
        "What is the weather in Tokyo today?",
        KnownModel::ClaudeSonnet46,
    );
    params.context_management = Some(ContextManagement::clear_tool_uses());

    // Show what the serialized request looks like
    let json = serde_json::to_value(&params.context_management).unwrap();
    println!("context_management payload:\n{}\n", serde_json::to_string_pretty(&json)?);

    match client.send(params).await {
        Ok(r) => {
            for b in &r.content {
                if let Some(t) = b.as_text() {
                    println!("{}\n", &t.text[..t.text.len().min(200)]);
                }
            }
        }
        Err(e) => println!("API response: {e}\n(expected if beta not enabled on your account)\n"),
    }

    // ── 2. Tool result clearing (advanced config) ────────────────
    println!("=== Tool Result Clearing (advanced) ===\n");

    let cm = ContextManagement {
        edits: vec![ContextEdit::ClearToolUses {
            trigger: Some(TokenThreshold::input_tokens(30000)),
            keep: Some(TokenThreshold::tool_uses(3)),
            clear_at_least: Some(TokenThreshold::input_tokens(5000)),
            exclude_tools: Some(vec!["web_search".to_string()]),
            clear_tool_inputs: Some(true),
        }],
    };

    let json = serde_json::to_value(&cm)?;
    println!("Advanced config:\n{}\n", serde_json::to_string_pretty(&json)?);

    // ── 3. Thinking block clearing ───────────────────────────────
    println!("=== Thinking Block Clearing ===\n");

    let cm = ContextManagement {
        edits: vec![ContextEdit::ClearThinking { keep: Some(ThinkingKeep::turns(2)) }],
    };

    let json = serde_json::to_value(&cm)?;
    println!("Thinking clearing config:\n{}\n", serde_json::to_string_pretty(&json)?);

    // ── 4. Combined strategies ───────────────────────────────────
    println!("=== Combined: Thinking + Tool Clearing ===\n");

    let cm = ContextManagement {
        edits: vec![
            // clear_thinking must come first
            ContextEdit::ClearThinking { keep: Some(ThinkingKeep::turns(2)) },
            ContextEdit::ClearToolUses {
                trigger: Some(TokenThreshold::input_tokens(50000)),
                keep: Some(TokenThreshold::tool_uses(5)),
                clear_at_least: None,
                exclude_tools: None,
                clear_tool_inputs: None,
            },
        ],
    };

    let json = serde_json::to_value(&cm)?;
    println!("Combined config:\n{}\n", serde_json::to_string_pretty(&json)?);

    // Send a real request with combined strategies
    let mut params = MessageCreateParams::new(
        16000,
        vec![MessageParam::new_with_string(
            "Explain the Rust borrow checker in one paragraph.".to_string(),
            MessageRole::User,
        )],
        KnownModel::ClaudeSonnet46.into(),
    );
    params.context_management = Some(cm);
    // clear_thinking requires thinking to be enabled
    params.thinking = Some(adk_anthropic::ThinkingConfig::adaptive());

    match client.send(params).await {
        Ok(r) => {
            for b in &r.content {
                if let Some(t) = b.as_text() {
                    println!("{}\n", &t.text[..t.text.len().min(300)]);
                }
            }
            println!("Usage: {} in / {} out tokens", r.usage.input_tokens, r.usage.output_tokens);
        }
        Err(e) => println!("API response: {e}\n(expected if beta not enabled on your account)"),
    }

    Ok(())
}
