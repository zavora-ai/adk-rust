//! Citations with the Anthropic Messages API.
//!
//! Demonstrates how Claude provides verifiable citations when answering
//! questions about documents. Covers all three document types:
//!
//! 1. Plain text — auto-chunked into sentences, cited by char index
//! 2. Custom content — your own chunks, cited by block index
//! 3. Multi-document — citations reference the correct document by index
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run -p adk-anthropic --example citations`

use adk_anthropic::{
    Anthropic, CitationsConfig, Content, ContentBlock, ContentBlockSourceParam, DocumentBlock,
    DocumentSource, KnownModel, MessageCreateParams, MessageParam, MessageRole, PlainTextSource,
    TextBlock, TextCitation,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    // ── 1. Plain text document with citations ────────────────────
    println!("=== 1. Plain Text Document ===\n");

    let doc = DocumentBlock::new(DocumentSource::PlainText(PlainTextSource::new(
        "Rust was first released in 2015. It was created by Graydon Hoare at Mozilla. \
         Rust guarantees memory safety without a garbage collector. The borrow checker \
         enforces ownership rules at compile time. Rust has won the most loved \
         programming language award on Stack Overflow for multiple years."
            .to_string(),
    )))
    .with_title("Rust Facts".to_string())
    .with_citations(CitationsConfig::enabled());

    let messages = vec![MessageParam::new_with_blocks(
        vec![
            ContentBlock::Document(doc),
            ContentBlock::Text(TextBlock::new("When was Rust first released and who created it?")),
        ],
        MessageRole::User,
    )];

    let r = client
        .send(MessageCreateParams::new(1024, messages, KnownModel::ClaudeSonnet46.into()))
        .await?;

    print_cited_response(&r.content);

    // ── 2. Custom content document (your own chunks) ─────────────
    println!("\n=== 2. Custom Content Document ===\n");

    let chunks = vec![
        Content::Text(TextBlock::new("Q1 2025 revenue was $4.2 billion, up 15% year-over-year.")),
        Content::Text(TextBlock::new(
            "Operating margin improved to 28%, compared to 24% in Q1 2024.",
        )),
        Content::Text(TextBlock::new("The company hired 2,000 new engineers in Q1 2025.")),
        Content::Text(TextBlock::new("Cloud services segment grew 32% to $1.8 billion.")),
    ];

    let doc = DocumentBlock::new(DocumentSource::ContentBlock(
        ContentBlockSourceParam::new_with_array(chunks),
    ))
    .with_title("Q1 2025 Earnings Report".to_string())
    .with_context("Fictional company earnings data for demonstration.".to_string())
    .with_citations(CitationsConfig::enabled());

    let messages = vec![MessageParam::new_with_blocks(
        vec![
            ContentBlock::Document(doc),
            ContentBlock::Text(TextBlock::new(
                "What was the revenue and how did the cloud segment perform?",
            )),
        ],
        MessageRole::User,
    )];

    let r = client
        .send(MessageCreateParams::new(1024, messages, KnownModel::ClaudeSonnet46.into()))
        .await?;

    print_cited_response(&r.content);

    // ── 3. Multi-document citations ──────────────────────────────
    println!("\n=== 3. Multi-Document Citations ===\n");

    let doc_a = DocumentBlock::new(DocumentSource::PlainText(PlainTextSource::new(
        "Python was created by Guido van Rossum and first released in 1991. \
         It emphasizes code readability and uses significant indentation."
            .to_string(),
    )))
    .with_title("Python Facts".to_string())
    .with_citations(CitationsConfig::enabled());

    let doc_b = DocumentBlock::new(DocumentSource::PlainText(PlainTextSource::new(
        "Go was designed at Google by Robert Griesemer, Rob Pike, and Ken Thompson. \
         It was first released in 2009. Go features built-in concurrency with goroutines."
            .to_string(),
    )))
    .with_title("Go Facts".to_string())
    .with_citations(CitationsConfig::enabled());

    let messages = vec![MessageParam::new_with_blocks(
        vec![
            ContentBlock::Document(doc_a),
            ContentBlock::Document(doc_b),
            ContentBlock::Text(TextBlock::new(
                "Compare when Python and Go were first released, and who created each.",
            )),
        ],
        MessageRole::User,
    )];

    let r = client
        .send(MessageCreateParams::new(1024, messages, KnownModel::ClaudeSonnet46.into()))
        .await?;

    print_cited_response(&r.content);

    Ok(())
}

/// Print response content with citation details.
fn print_cited_response(content: &[ContentBlock]) {
    for block in content {
        if let Some(text) = block.as_text() {
            if text.has_citations() {
                // Cited text — show the claim and its sources
                print!("\x1b[1m{}\x1b[0m", text.text); // bold
                for citation in text.citations.as_ref().unwrap() {
                    match citation {
                        TextCitation::CharLocation(c) => {
                            print!(
                                " \x1b[90m[doc:{} chars:{}..{} \"{}\"]\x1b[0m",
                                c.document_index,
                                c.start_char_index,
                                c.end_char_index,
                                truncate(&c.cited_text, 50),
                            );
                        }
                        TextCitation::PageLocation(p) => {
                            print!(
                                " \x1b[90m[doc:{} pp:{}-{} \"{}\"]\x1b[0m",
                                p.document_index,
                                p.start_page_number,
                                p.end_page_number,
                                truncate(&p.cited_text, 50),
                            );
                        }
                        TextCitation::ContentBlockLocation(b) => {
                            print!(
                                " \x1b[90m[doc:{} blocks:{}..{} \"{}\"]\x1b[0m",
                                b.document_index,
                                b.start_block_index,
                                b.end_block_index,
                                truncate(&b.cited_text, 50),
                            );
                        }
                        TextCitation::WebSearchResultLocation(_) => {
                            print!(" \x1b[90m[web search result]\x1b[0m");
                        }
                    }
                }
            } else {
                // Uncited connecting text
                print!("{}", text.text);
            }
        }
    }
    println!();
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}
