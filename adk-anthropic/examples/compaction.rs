//! On-demand compaction with the Anthropic Messages API (beta `compact-2026-09-04`).
//!
//! Asks Claude for a summary of a conversation in place of a reply, then continues the
//! conversation from the signed `compaction` block, which goes first in `messages` in place of
//! the turns it summarizes. The client adds the beta header to both requests.
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run -p adk-anthropic --example compaction`

use adk_anthropic::{
    Anthropic, CompactionParam, ContentBlock, MessageCreateParams, MessageParam, MessageRole,
    Model, StopReason,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    let client = Anthropic::new(None)?;
    let model = Model::Custom("claude-haiku-5-5".to_string());

    let conversation = vec![
        MessageParam::new_with_string(
            "I am building a recipe app. Help me name the main entities in the data model."
                .to_string(),
            MessageRole::User,
        ),
        MessageParam::new_with_string(
            "Start with Recipe, Ingredient, and Step. Add a RecipeIngredient entry that holds \
             the quantity and unit for each ingredient in a recipe."
                .to_string(),
            MessageRole::Assistant,
        ),
        MessageParam::new_with_string(
            "Good. Now suggest field names for Recipe.".to_string(),
            MessageRole::User,
        ),
    ];

    // ── 1. Ask for a summary in place of a reply ─────────────────
    let mut params = MessageCreateParams::new(4_096, conversation, model.clone());
    params.compaction = Some(CompactionParam::summarize());
    let compacted = client.send(params).await?;
    if compacted.stop_reason != Some(StopReason::Compaction) {
        println!(
            "No summary came back (stop reason {:?}); keep the history.",
            compacted.stop_reason
        );
        return Ok(());
    }
    let Some(ContentBlock::Compaction(block)) = compacted.content.first() else {
        println!("The response held no compaction block.");
        return Ok(());
    };
    println!("=== Summary ===\n{}\n", block.content);
    if let Some(iterations) = compacted.usage.raw.as_deref().and_then(|raw| raw.get("iterations")) {
        println!("=== Usage of the summarization call ===\n{iterations}\n");
    }

    // ── 2. Continue from the block, which replaces the summarized turns ──
    let messages = vec![
        MessageParam::new_with_blocks(compacted.content.clone(), MessageRole::Assistant),
        MessageParam::new_with_string(
            "Now suggest field names for Ingredient.".to_string(),
            MessageRole::User,
        ),
    ];
    let reply = client.send(MessageCreateParams::new(1_024, messages, model)).await?;
    println!("=== Reply after the summary ===");
    for block in &reply.content {
        if let Some(text) = block.as_text() {
            println!("{}", text.text);
        }
    }
    Ok(())
}
