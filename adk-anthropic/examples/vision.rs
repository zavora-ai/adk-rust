//! Vision (image understanding) with the Anthropic Messages API.
//!
//! Demonstrates Claude's ability to analyze images:
//! 1. URL-based image — simplest, for publicly hosted images
//! 2. Base64-encoded image — for local files
//! 3. Multiple images — comparison in a single request
//!
//! Supported formats: JPEG, PNG, GIF, WebP. Max 8000×8000 px.
//! Images are resized if the long edge exceeds 1568 px.
//! Cost: ~(width × height) / 750 tokens per image.
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run -p adk-anthropic --example vision`

use adk_anthropic::{
    Anthropic, ContentBlock, ImageBlock, KnownModel, MessageCreateParams, MessageParam,
    MessageRole, TextBlock, UrlImageSource,
};

const ANT_IMAGE: &str =
    "https://upload.wikimedia.org/wikipedia/commons/a/a7/Camponotus_flavomarginatus_ant.jpg";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    // ── 1. Single URL image ──────────────────────────────────────
    println!("=== 1. Single Image (URL) ===\n");

    let messages = vec![MessageParam::new_with_blocks(
        vec![
            ContentBlock::Image(ImageBlock::new_with_url(UrlImageSource::new(
                ANT_IMAGE.to_string(),
            ))),
            ContentBlock::Text(TextBlock::new(
                "What insect is in this image? Describe it briefly.",
            )),
        ],
        MessageRole::User,
    )];

    let r = client
        .send(MessageCreateParams::new(256, messages, KnownModel::ClaudeSonnet46.into()))
        .await?;

    print_response(&r);

    // ── 2. Multiple images in one request ──────────────────────────
    println!("\n=== 2. Multiple Images in One Request ===\n");

    let messages = vec![MessageParam::new_with_blocks(
        vec![
            ContentBlock::Image(ImageBlock::new_with_url(UrlImageSource::new(
                ANT_IMAGE.to_string(),
            ))),
            ContentBlock::Text(TextBlock::new(
                "Image 1 above shows an insect. Now answer two questions:\n\
                 1. Estimate the body length of this ant in millimeters.\n\
                 2. What habitat would you expect to find this species in?",
            )),
        ],
        MessageRole::User,
    )];

    let r = client
        .send(MessageCreateParams::new(512, messages, KnownModel::ClaudeSonnet46.into()))
        .await?;

    print_response(&r);

    // ── 3. Image with system prompt ──────────────────────────────
    println!("\n=== 3. Image with System Prompt ===\n");

    let messages = vec![MessageParam::new_with_blocks(
        vec![
            ContentBlock::Image(ImageBlock::new_with_url(UrlImageSource::new(
                ANT_IMAGE.to_string(),
            ))),
            ContentBlock::Text(TextBlock::new("Classify this specimen.")),
        ],
        MessageRole::User,
    )];

    let r = client
        .send(
            MessageCreateParams::new(256, messages, KnownModel::ClaudeSonnet46.into())
                .with_system("You are an entomologist. Respond with the scientific classification: Order, Family, Genus, Species (if identifiable). Be concise."),
        )
        .await?;

    print_response(&r);

    Ok(())
}

fn print_response(msg: &adk_anthropic::Message) {
    for block in &msg.content {
        if let Some(text) = block.as_text() {
            println!("{}", text.text);
        }
    }
    println!("\n  [{} in / {} out tokens]", msg.usage.input_tokens, msg.usage.output_tokens);
}
