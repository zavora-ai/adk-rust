//! Handling stop reasons with the Anthropic Messages API.
//!
//! Every successful response includes a `stop_reason` field that tells you
//! *why* Claude stopped generating. This example demonstrates each value
//! and the correct handling pattern for production use.
//!
//! Stop reasons:
//!   end_turn                      — natural completion
//!   max_tokens                    — hit the token limit (truncated)
//!   stop_sequence                 — hit a custom stop sequence
//!   tool_use                      — Claude wants to call a tool
//!   pause_turn                    — server tool loop hit iteration limit
//!   refusal                       — safety refusal
//!   model_context_window_exceeded — context window full
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run -p adk-anthropic --example stop_reasons`

use adk_anthropic::{
    Anthropic, ContentBlock, KnownModel, MessageCreateParams, MessageParam, MessageRole,
    StopReason, ToolResultBlock, ToolUnionParam,
};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    // ── 1. end_turn — natural completion ─────────────────────────
    println!("=== 1. EndTurn (natural completion) ===\n");

    let r = client
        .send(MessageCreateParams::simple(
            "What is 2 + 2? Answer in one word.",
            KnownModel::ClaudeSonnet46,
        ))
        .await?;

    match r.stop_reason {
        Some(StopReason::EndTurn) => println!("✓ EndTurn — complete response"),
        other => println!("  unexpected: {other:?}"),
    }
    print_text(&r.content);

    // ── 2. max_tokens — truncated response ───────────────────────
    println!("\n=== 2. MaxTokens (truncated) ===\n");

    let mut params = MessageCreateParams::simple(
        "Write a 500-word essay about the history of computing.",
        KnownModel::ClaudeSonnet46,
    );
    // Force truncation with a tiny limit
    params.max_tokens = 15;

    let r = client.send(params).await?;

    match r.stop_reason {
        Some(StopReason::MaxTokens) => {
            println!(
                "✓ MaxTokens — response was truncated at {} output tokens",
                r.usage.output_tokens
            );
            println!("  In production: retry with higher max_tokens or continue generation");
        }
        other => println!("  unexpected: {other:?}"),
    }
    print_text(&r.content);

    // ── 3. stop_sequence — custom stop string ────────────────────
    println!("\n=== 3. StopSequence (custom stop) ===\n");

    let params = MessageCreateParams::simple(
        "Count from 1 to 10, one number per line.",
        KnownModel::ClaudeSonnet46,
    )
    .with_stop_sequences(vec!["5".to_string()]);

    let r = client.send(params).await?;

    match r.stop_reason {
        Some(StopReason::StopSequence) => {
            println!("✓ StopSequence — stopped at: {:?}", r.stop_sequence);
        }
        other => println!("  unexpected: {other:?}"),
    }
    print_text(&r.content);

    // ── 4. tool_use — Claude wants to call a tool ────────────────
    println!("\n=== 4. ToolUse (tool calling) ===\n");

    let tool = ToolUnionParam::new_custom_tool(
        "get_weather".to_string(),
        json!({
            "type": "object",
            "properties": {
                "city": {"type": "string"}
            },
            "required": ["city"]
        }),
    );

    let params =
        MessageCreateParams::simple("What's the weather in Paris?", KnownModel::ClaudeSonnet46)
            .with_tools(vec![tool.clone()]);

    let r = client.send(params).await?;

    match r.stop_reason {
        Some(StopReason::ToolUse) => {
            println!("✓ ToolUse — Claude wants to call a tool");
            for block in &r.content {
                if let Some(tu) = block.as_tool_use() {
                    println!("  tool: {} input: {}", tu.name, tu.input);

                    // Complete the tool loop
                    let tool_result = ContentBlock::ToolResult(
                        ToolResultBlock::new(tu.id.clone()).with_string_content(
                            r#"{"temp": "18°C", "condition": "Cloudy"}"#.to_string(),
                        ),
                    );

                    let messages = vec![
                        MessageParam::new_with_string(
                            "What's the weather in Paris?".to_string(),
                            MessageRole::User,
                        ),
                        MessageParam::new_with_blocks(r.content.clone(), MessageRole::Assistant),
                        MessageParam::new_with_blocks(vec![tool_result], MessageRole::User),
                    ];

                    let final_r = client
                        .send(
                            MessageCreateParams::new(
                                1024,
                                messages,
                                KnownModel::ClaudeSonnet46.into(),
                            )
                            .with_tools(vec![tool.clone()]),
                        )
                        .await?;

                    println!("  final stop_reason: {:?}", final_r.stop_reason);
                    print_text(&final_r.content);
                }
            }
        }
        other => println!("  unexpected: {other:?}"),
    }

    // ── 5. Dispatcher pattern ────────────────────────────────────
    println!("\n=== 5. Dispatcher Pattern (production best practice) ===\n");

    let r = client
        .send(MessageCreateParams::simple("Say hello in Japanese.", KnownModel::ClaudeSonnet46))
        .await?;

    let result = handle_response(&r);
    println!("Dispatcher returned: {result}");

    Ok(())
}

/// Production-grade stop reason dispatcher.
fn handle_response(response: &adk_anthropic::Message) -> String {
    match response.stop_reason {
        Some(StopReason::EndTurn) => {
            // Normal completion — extract text
            response
                .content
                .iter()
                .filter_map(|b| b.as_text())
                .map(|t| t.text.clone())
                .collect::<Vec<_>>()
                .join("\n")
        }
        Some(StopReason::MaxTokens) => {
            format!(
                "[TRUNCATED after {} tokens] {}",
                response.usage.output_tokens,
                response
                    .content
                    .iter()
                    .filter_map(|b| b.as_text())
                    .map(|t| t.text.as_str())
                    .collect::<String>()
            )
        }
        Some(StopReason::StopSequence) => {
            format!(
                "[STOPPED at {:?}] {}",
                response.stop_sequence,
                response
                    .content
                    .iter()
                    .filter_map(|b| b.as_text())
                    .map(|t| t.text.as_str())
                    .collect::<String>()
            )
        }
        Some(StopReason::ToolUse) => "[TOOL_USE — execute tools and continue]".to_string(),
        Some(StopReason::PauseTurn) => "[PAUSE_TURN — send response back to continue]".to_string(),
        Some(StopReason::PauseRun) => "[PAUSE_RUN — run paused for human-in-the-loop]".to_string(),
        Some(StopReason::Refusal) => "[REFUSAL — Claude declined this request]".to_string(),
        Some(StopReason::ModelContextWindowExceeded) => {
            format!(
                "[CONTEXT_WINDOW_EXCEEDED] {}",
                response
                    .content
                    .iter()
                    .filter_map(|b| b.as_text())
                    .map(|t| t.text.as_str())
                    .collect::<String>()
            )
        }
        None => "[NO STOP REASON — streaming in progress?]".to_string(),
    }
}

fn print_text(content: &[ContentBlock]) {
    for block in content {
        if let Some(text) = block.as_text() {
            let preview: String = text.text.chars().take(150).collect();
            println!("  \"{preview}\"");
        }
    }
}
